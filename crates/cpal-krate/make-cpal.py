#!/usr/bin/env python3
"""Make cpal 0.18.2 with a Krate host (IC-932).

    python3 crates/cpal-krate/make-cpal.py OUT_DIR

copies cpal 0.18.2 from cargo's registry (run `cargo fetch` in a project
that depends on it first, or set CPAL_SRC) to OUT_DIR/cpal-0.18.2 and adds
crates/cpal-krate/src as `host/krate`: the default host for
`target_os = "wasi"`, where cpal otherwise has only its silent Null host.
Nothing else in cpal changes. A program, or kira or rodio on top of cpal,
then plays sound inside a threaded Krate app unchanged with

    [patch.crates-io]
    cpal = { path = "OUT_DIR/cpal-0.18.2" }

The patched crate depends on this checkout's SDK (crates/bindings-rust) by
absolute path.
"""

import glob
import os
import shutil
import sys

VERSION = "0.18.2"
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))

NULL_PLATFORM = """    target_os = "android",
    all(
        target_arch = "wasm32",
        target_os = "unknown",
        feature = "wasm-bindgen"
    ),
)))]
mod platform_impl {
    use crate::host::null::Host as NullHost;
"""

KRATE_PLATFORM = """
#[cfg(target_os = "wasi")]
mod platform_impl {
    use crate::host::krate::Host as KrateHost;

    impl_platform_host!(
        Krate => KrateHost,
        #[cfg(feature = "custom")] Custom => super::CustomHost,
    );

    /// The default host on Krate: krate:audio.
    pub fn default_host() -> Host {
        KrateHost::new()
            .expect("the default host should always be available")
            .into()
    }
}
"""


def find_source():
    if os.environ.get("CPAL_SRC"):
        return os.environ["CPAL_SRC"]
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    hits = glob.glob(os.path.join(home, "registry/src/*", f"cpal-{VERSION}"))
    if not hits:
        sys.exit(f"cpal {VERSION} is not in cargo's registry; `cargo fetch` a project using it")
    return hits[0]


def replace(path, old, new, count=1):
    text = open(path).read()
    if text.count(old) != count:
        sys.exit(f"{path}: expected {count} of {old[:60]!r}, found {text.count(old)}")
    open(path, "w").write(text.replace(old, new))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = os.path.join(os.path.abspath(sys.argv[1]), f"cpal-{VERSION}")
    if os.path.exists(out):
        shutil.rmtree(out)
    shutil.copytree(find_source(), out)
    for f in glob.glob(os.path.join(out, ".cargo*")):
        os.remove(f)
    os.makedirs(os.path.join(out, "src/host/krate"))
    shutil.copy(os.path.join(HERE, "src/mod.rs"), os.path.join(out, "src/host/krate/mod.rs"))

    # The Null host stays for every other platform cpal does not know; wasi
    # gets Krate's.
    host_mod = os.path.join(out, "src/host/mod.rs")
    replace(host_mod, "    target_os = \"android\",\n    all(\n        target_arch = \"wasm32\",\n"
            "        target_os = \"unknown\",\n        feature = \"wasm-bindgen\"\n    ),\n)))]\n"
            "pub(crate) mod null;\n",
            "    target_os = \"android\",\n    target_os = \"wasi\",\n    all(\n"
            "        target_arch = \"wasm32\",\n        target_os = \"unknown\",\n"
            "        feature = \"wasm-bindgen\"\n    ),\n)))]\npub(crate) mod null;\n"
            "#[cfg(target_os = \"wasi\")]\npub(crate) mod krate;\n")
    platform = os.path.join(out, "src/platform/mod.rs")
    replace(platform, NULL_PLATFORM, NULL_PLATFORM.replace(
        "    target_os = \"android\",\n", "    target_os = \"android\",\n    target_os = \"wasi\",\n", 1))
    with open(platform, "a") as f:
        f.write(KRATE_PLATFORM)

    cargo = os.path.join(out, "Cargo.toml")
    sdk = os.path.join(REPO, "crates/bindings-rust")
    with open(cargo, "a") as f:
        f.write(
            "\n[target.'cfg(target_os = \"wasi\")'.dependencies.krate]\n"
            f"path = {sdk!r}\n"
            "features = [\"gui\", \"std\"]\n"
        )
    print(out)


if __name__ == "__main__":
    main()
