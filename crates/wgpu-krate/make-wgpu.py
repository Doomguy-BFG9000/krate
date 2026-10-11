#!/usr/bin/env python3
"""Make wgpu Send + Sync inside a threaded Krate app (IC-929, IC-931).

    python3 crates/wgpu-krate/make-wgpu.py OUT_DIR VERSION

copies wgpu VERSION and wgpu-types VERSION from cargo's registry (run
`cargo fetch` in a project that depends on them first) to OUT_DIR and
changes one condition in each: where wgpu decides its handles are Send and
Sync. A program then builds for Krate with

    [patch.crates-io]
    wgpu = { path = "OUT_DIR/wgpu-VERSION" }
    wgpu-types = { path = "OUT_DIR/wgpu-types-VERSION" }

Why: wgpu makes every handle !Send on a wasm target with atomics, because
there its only backend is the browser's WebGPU, whose objects are JavaScript
values bound to one thread. A threaded Krate app is wasm with atomics
(wasm32-wasip1-threads), but its backend is wgpu-krate over krate:gpu, which
any of the app's threads may call. Without this, a program that hands a
Device, a Queue or a pipeline to another thread -- Veloren's mesh workers,
Bevy's multi-threaded executor -- does not compile for Krate at all.

The change: `not(target_feature = "atomics")` becomes
`any(not(target_feature = "atomics"), target_os = "wasi")` in the
`fragile-send-sync-non-atomic-wasm` conditions, nowhere else. The browser
build (wasm32-unknown-unknown) is untouched.
"""

import glob
import os
import shutil
import sys

ATOMICS = 'not(target_feature = "atomics")'
KRATE = 'any(not(target_feature = "atomics"), target_os = "wasi")'

# The files that carry the Send/Sync decision, by crate, with how many
# conditions each must have (so a new wgpu layout fails loudly, not quietly).
FILES = {
    "wgpu": ["build.rs"],
    "wgpu-types": ["src/send_sync.rs", "src/lib.rs"],
}


def find_source(name, version):
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    hits = glob.glob(os.path.join(home, "registry/src/*", f"{name}-{version}"))
    if not hits:
        sys.exit(f"{name} {version} is not in cargo's registry; `cargo fetch` a project using it")
    return hits[0]


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    out_dir, version = os.path.abspath(sys.argv[1]), sys.argv[2]
    for name, candidates in FILES.items():
        out = os.path.join(out_dir, f"{name}-{version}")
        if os.path.exists(out):
            shutil.rmtree(out)
        shutil.copytree(find_source(name, version), out)
        for f in glob.glob(os.path.join(out, ".cargo*")):
            os.remove(f)
        changed = 0
        for rel in candidates:
            path = os.path.join(out, rel)
            if not os.path.exists(path):
                continue
            text = open(path).read()
            if ATOMICS not in text:
                continue
            changed += text.count(ATOMICS)
            open(path, "w").write(text.replace(ATOMICS, KRATE))
        if changed == 0:
            sys.exit(f"{name} {version}: no `{ATOMICS}` condition found; wgpu's layout changed")
        print(f"{out}  ({changed} conditions)")


if __name__ == "__main__":
    main()
