#!/usr/bin/env python3
"""Make window_clipboard 0.5.1 and copypasta 0.8.2 reach the clipboard in a Krate app (IC-932).

    python3 crates/clipboard-krate/make-clipboard.py OUT_DIR

copies both crates from cargo's registry to OUT_DIR and gives each a Krate
platform for `target_os = "wasi"`: the person's clipboard through
krate:ui/clipboard (grants `ui.clipboard:read`, `ui.clipboard:write`). On
WASI both fall back to clipboards that do nothing -- window_clipboard's
"unimplemented" dummy, copypasta's no-op that prints a line -- so copy and
paste in an iced or conrod text field (Veloren's chat) silently did nothing.
Patch them in with

    [patch.crates-io]
    window_clipboard = { path = "OUT_DIR/window_clipboard-0.5.1" }
    copypasta = { path = "OUT_DIR/copypasta-0.8.2" }

The patched crates depend on this checkout's SDK by absolute path.
"""

import glob
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))


def copy(name, version):
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    hits = glob.glob(os.path.join(home, "registry/src/*", f"{name}-{version}"))
    if not hits:
        sys.exit(f"{name} {version} is not in cargo's registry; `cargo fetch` a project using it")
    out = os.path.join(os.path.abspath(sys.argv[1]), f"{name}-{version}")
    if os.path.exists(out):
        shutil.rmtree(out)
    shutil.copytree(hits[0], out)
    for f in glob.glob(os.path.join(out, ".cargo*")):
        os.remove(f)
    return out


def replace(path, old, new):
    text = open(path).read()
    if text.count(old) != 1:
        sys.exit(f"{path}: expected one {old.strip()[:60]!r}, found {text.count(old)}")
    open(path, "w").write(text.replace(old, new))


def sdk(out):
    with open(os.path.join(out, "Cargo.toml"), "a") as f:
        f.write(
            "\n[target.'cfg(target_os = \"wasi\")'.dependencies.krate]\n"
            f"path = {os.path.join(REPO, 'crates/bindings-rust')!r}\n"
            "features = [\"gui\", \"std\"]\n"
        )


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)

    out = copy("window_clipboard", "0.5.1")
    shutil.copy(os.path.join(HERE, "src/window_clipboard.rs"), os.path.join(out, "src/platform/krate.rs"))
    lib = os.path.join(out, "src/lib.rs")
    replace(lib, """    target_os = "android"
)))]
#[path = "platform/dummy.rs"]
mod platform;""", """    target_os = "android",
    target_os = "wasi"
)))]
#[path = "platform/dummy.rs"]
mod platform;

#[cfg(target_os = "wasi")]
#[path = "platform/krate.rs"]
mod platform;""")
    sdk(out)
    print(out)

    out = copy("copypasta", "0.8.2")
    shutil.copy(os.path.join(HERE, "src/copypasta.rs"), os.path.join(out, "src/krate_clipboard.rs"))
    lib = os.path.join(out, "src/lib.rs")
    replace(lib, """    target_os = "ios",
    target_os = "emscripten"
)))]
pub type ClipboardContext = nop_clipboard::NopClipboardContext;""", """    target_os = "ios",
    target_os = "emscripten",
    target_os = "wasi"
)))]
pub type ClipboardContext = nop_clipboard::NopClipboardContext;

#[cfg(target_os = "wasi")]
pub mod krate_clipboard;
#[cfg(target_os = "wasi")]
pub type ClipboardContext = krate_clipboard::KrateClipboardContext;""")
    sdk(out)
    print(out)


if __name__ == "__main__":
    main()
