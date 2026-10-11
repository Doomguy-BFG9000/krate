#!/usr/bin/env python3
"""Make quinn and quinn-udp build for a Krate app (IC-935).

    python3 crates/net-krate/make-quinn.py OUT_DIR

copies quinn 0.11.11 and quinn-udp 0.5.14 from cargo's registry (run
`cargo fetch` in a project that depends on them first) to OUT_DIR and makes
one change in each: wasm32-wasi joins the browser on the "no std sockets"
side. Neither crate's browser path touches JavaScript; it is simply the
build without socket2 and std UDP sockets, which wasm32-wasip1 cannot have.
QUIC then runs over net_krate::quinn::KrateUdpSocket (krate:net/sockets)
with Endpoint::new_with_abstract_socket, and a program patches the copies in:

    [patch.crates-io]
    quinn = { path = "OUT_DIR/quinn-0.11.11" }
    quinn-udp = { path = "OUT_DIR/quinn-udp-0.5.14" }
"""

import glob
import os
import shutil
import sys

CRATES = {"quinn": "0.11.11", "quinn-udp": "0.5.14"}
BROWSER = 'all(target_family = "wasm", target_os = "unknown")'
NO_SOCKETS = 'all(target_family = "wasm", any(target_os = "unknown", target_os = "wasi"))'


def find_source(name, version):
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    hits = glob.glob(os.path.join(home, "registry/src/*", f"{name}-{version}"))
    if not hits:
        sys.exit(f"{name} {version} is not in cargo's registry; `cargo fetch` a project using it")
    return hits[0]


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out_dir = os.path.abspath(sys.argv[1])
    for name, version in CRATES.items():
        out = os.path.join(out_dir, f"{name}-{version}")
        if os.path.exists(out):
            shutil.rmtree(out)
        shutil.copytree(find_source(name, version), out)
        for f in glob.glob(os.path.join(out, ".cargo*")):
            os.remove(f)
        changed = 0
        for rel in ("build.rs", "Cargo.toml"):
            path = os.path.join(out, rel)
            text = open(path).read()
            changed += text.count(BROWSER)
            open(path, "w").write(text.replace(BROWSER, NO_SOCKETS))
        if changed == 0:
            sys.exit(f"{name} {version}: no browser condition found; its layout changed")
        print(f"{out}  ({changed} conditions)")


if __name__ == "__main__":
    main()
