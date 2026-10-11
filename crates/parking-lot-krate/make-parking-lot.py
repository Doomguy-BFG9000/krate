#!/usr/bin/env python3
"""Make parking_lot_core 0.9.12 park threads on stable Rust in wasm (IC-939).

    python3 crates/parking-lot-krate/make-parking-lot.py OUT_DIR

copies parking_lot_core 0.9.12 from cargo's registry to
OUT_DIR/parking_lot_core-0.9.12 and adds src/std_park.rs as its thread
parker for wasm with atomics when its `nightly` feature is off. Without it a
threaded app built on stable panics "Parking not supported on this platform"
the first time two threads contend a parking_lot lock (Veloren, Bevy, most
engines); with it, it parks through std::thread::park. Patch it in with

    [patch.crates-io]
    parking_lot_core = { path = "OUT_DIR/parking_lot_core-0.9.12" }
"""

import glob
import os
import shutil
import sys

VERSION = "0.9.12"
HERE = os.path.dirname(os.path.abspath(__file__))

OLD = '''    } else if #[cfg(target_family = "wasm")] {
        #[path = "wasm.rs"]
        mod imp;'''
NEW = '''    } else if #[cfg(all(target_family = "wasm", target_feature = "atomics"))] {
        #[path = "std_park.rs"]
        mod imp;
    } else if #[cfg(target_family = "wasm")] {
        #[path = "wasm.rs"]
        mod imp;'''


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    hits = glob.glob(os.path.join(home, "registry/src/*", f"parking_lot_core-{VERSION}"))
    if not hits:
        sys.exit(f"parking_lot_core {VERSION} is not in cargo's registry")
    out = os.path.join(os.path.abspath(sys.argv[1]), f"parking_lot_core-{VERSION}")
    if os.path.exists(out):
        shutil.rmtree(out)
    shutil.copytree(hits[0], out)
    for f in glob.glob(os.path.join(out, ".cargo*")):
        os.remove(f)
    shutil.copy(os.path.join(HERE, "src/std_park.rs"), os.path.join(out, "src/thread_parker/std_park.rs"))
    mod = os.path.join(out, "src/thread_parker/mod.rs")
    text = open(mod).read()
    if text.count(OLD) != 1:
        sys.exit(f"{mod}: the wasm parker selection changed")
    open(mod, "w").write(text.replace(OLD, NEW))
    print(out)


if __name__ == "__main__":
    main()
