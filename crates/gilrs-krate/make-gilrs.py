#!/usr/bin/env python3
"""Make gilrs-core 0.6.8 with a Krate backend (IC-936).

    python3 crates/gilrs-krate/make-gilrs.py OUT_DIR

copies gilrs-core 0.6.8 from cargo's registry (`cargo fetch` a project that
uses gilrs first) to OUT_DIR/gilrs-core-0.6.8, and:

- narrows its browser code (web-sys gamepads, js Date) from any wasm32 to
  wasm32-unknown-unknown, where a browser is;
- adds crates/gilrs-krate/src as `platform/krate`, the backend for
  `target_os = "wasi"`: the person's gamepad through krate:ui/events.

gilrs (and every game on it: Veloren, Bevy's gilrs plugin) then sees the pad
inside a Krate app unchanged, with

    [patch.crates-io]
    gilrs-core = { path = "OUT_DIR/gilrs-core-0.6.8" }

The patched crate depends on this checkout's SDK by absolute path.
"""

import glob
import os
import re
import shutil
import sys

VERSION = "0.6.8"
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
BROWSER = 'all(target_arch = "wasm32", target_os = "unknown")'


def find_source():
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    hits = glob.glob(os.path.join(home, "registry/src/*", f"gilrs-core-{VERSION}"))
    if not hits:
        sys.exit(f"gilrs-core {VERSION} is not in cargo's registry; `cargo fetch` a project using it")
    return hits[0]


def replace(path, old, new):
    text = open(path).read()
    if old not in text:
        sys.exit(f"{path}: expected text not found: {old[:60]!r}")
    open(path, "w").write(text.replace(old, new, 1))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = os.path.join(os.path.abspath(sys.argv[1]), f"gilrs-core-{VERSION}")
    if os.path.exists(out):
        shutil.rmtree(out)
    shutil.copytree(find_source(), out)
    for f in glob.glob(os.path.join(out, ".cargo*")):
        os.remove(f)

    changed = 0
    for path in glob.glob(os.path.join(out, "src/**/*.rs"), recursive=True) + [
        os.path.join(out, "Cargo.toml")
    ]:
        text = open(path).read()
        new = text.replace('not(target_arch = "wasm32")', f"not({BROWSER})")
        new = re.sub(r'cfg\(target_arch = "wasm32"\)', f"cfg({BROWSER})", new)
        if new != text:
            changed += 1
            open(path, "w").write(new)
    if changed == 0:
        sys.exit("gilrs-core: no wasm32 conditions found; its layout changed")

    shutil.copytree(os.path.join(HERE, "src"), os.path.join(out, "src/platform/krate"))
    mod = os.path.join(out, "src/platform/mod.rs")
    replace(
        mod,
        f"    not({BROWSER})\n))]\n#[path = \"default/mod.rs\"]",
        f"    not({BROWSER}),\n    not(target_os = \"wasi\")\n))]\n#[path = \"default/mod.rs\"]",
    )
    with open(mod, "a") as f:
        f.write('\n#[cfg(target_os = "wasi")]\n#[path = "krate/mod.rs"]\nmod platform;\n')
    with open(os.path.join(out, "Cargo.toml"), "a") as f:
        sdk = os.path.join(REPO, "crates/bindings-rust")
        f.write(
            "\n[target.'cfg(target_os = \"wasi\")'.dependencies.krate]\n"
            f"path = {sdk!r}\n"
            "features = [\"gui\", \"std\"]\n"
        )
    print(out)


if __name__ == "__main__":
    main()
