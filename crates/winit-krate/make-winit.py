#!/usr/bin/env python3
"""Make winit 0.30.13 with a Krate backend (IC-930).

    python3 crates/winit-krate/make-winit.py OUT_DIR

copies winit 0.30.13 from cargo's registry (run `cargo fetch` in a project
that depends on it first, or set WINIT_SRC) to OUT_DIR/winit-0.30.13 and adds
crates/winit-krate/src as `platform_impl/krate`, selected for
`target_os = "wasi"`. Nothing else in winit changes. A program then builds for
Krate unchanged with

    [patch.crates-io]
    winit = { path = "OUT_DIR/winit-0.30.13" }

The patched crate depends on this checkout's SDK (crates/bindings-rust) by
absolute path.
"""

import glob
import os
import shutil
import sys

VERSION = "0.30.13"
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))


def find_source():
    if os.environ.get("WINIT_SRC"):
        return os.environ["WINIT_SRC"]
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    hits = glob.glob(os.path.join(home, "registry/src/*", f"winit-{VERSION}"))
    if not hits:
        sys.exit(f"winit {VERSION} is not in cargo's registry; `cargo fetch` a project using it")
    return hits[0]


def replace(path, old, new, count=1):
    text = open(path).read()
    if old not in text:
        sys.exit(f"{path}: expected text not found: {old[:60]!r}")
    open(path, "w").write(text.replace(old, new, count))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = os.path.join(os.path.abspath(sys.argv[1]), f"winit-{VERSION}")
    if os.path.exists(out):
        shutil.rmtree(out)
    shutil.copytree(find_source(), out)
    for f in glob.glob(os.path.join(out, ".cargo*")):
        os.remove(f)
    backend = os.path.join(out, "src/platform_impl/krate")
    shutil.copytree(os.path.join(HERE, "src"), backend)

    replace(
        os.path.join(out, "build.rs"),
        "        orbital_platform: { redox },\n",
        "        orbital_platform: { redox },\n        krate_platform: { target_os = \"wasi\" },\n",
    )
    mod = os.path.join(out, "src/platform_impl/mod.rs")
    replace(mod, "#[cfg(orbital_platform)]\nmod orbital;\n",
            "#[cfg(orbital_platform)]\nmod orbital;\n#[cfg(krate_platform)]\nmod krate;\n")
    replace(mod, "#[cfg(orbital_platform)]\nuse orbital as platform;\n",
            "#[cfg(orbital_platform)]\nuse orbital as platform;\n#[cfg(krate_platform)]\nuse krate as platform;\n")
    replace(mod, "    not(orbital_platform),\n))]\ncompile_error!",
            "    not(orbital_platform),\n    not(krate_platform),\n))]\ncompile_error!")

    cargo = os.path.join(out, "Cargo.toml")
    text = open(cargo).read()
    # The browser's dependencies are for wasm32-unknown-unknown only; a
    # Krate app is wasm32-wasi*.
    text = text.replace("[target.'cfg(target_family = \"wasm\")'.",
                        "[target.'cfg(all(target_family = \"wasm\", target_os = \"unknown\"))'.")
    sdk = os.path.join(REPO, "crates/bindings-rust")
    c_runtime = os.path.join(REPO, "crates/c-runtime")
    text += (
        "\n[target.'cfg(target_os = \"wasi\")'.dependencies.krate]\n"
        f"path = {sdk!r}\n"
        "features = [\"gui\", \"std\"]\n"
        # The program's WASI calls answered inside it (no WASI import may
        # reach Krate): every winit program on Krate links it.
        "\n[target.'cfg(target_os = \"wasi\")'.dependencies.krate-c-runtime]\n"
        f"path = {c_runtime!r}\n"
    )
    open(cargo, "w").write(text)
    print(out)


if __name__ == "__main__":
    main()
