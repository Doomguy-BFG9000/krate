#!/usr/bin/env python3
"""Make tempfile 3.27.0 find a temporary directory inside a Krate app (IC-932).

    python3 crates/tempfile-krate/make-tempfile.py OUT_DIR

copies tempfile 3.27.0 from cargo's registry to OUT_DIR/tempfile-3.27.0 and
changes where its default directory comes from on WASI. tempfile asks
std::env::temp_dir, which on every WASI target panics "not supported by WASI
yet", so any program that makes a temporary file (most test suites, many
caches) dies there. On WASI the patched crate uses $TMPDIR when the app has
one and otherwise `tmp` in the app's own folder, made on first use -- a
Krate app's paths are its sandbox's, so it needs `fs.mkdir:tmp` and
`fs.write:tmp/**`. Patch it in with

    [patch.crates-io]
    tempfile = { path = "OUT_DIR/tempfile-3.27.0" }
"""

import glob
import os
import shutil
import sys

VERSION = "3.27.0"
OLD = """        .unwrap_or_else(env::temp_dir)"""
NEW = """        .unwrap_or_else(platform_temp_dir)"""
FN = '''
/// std's answer, except on WASI, where std::env::temp_dir panics: there it is
/// $TMPDIR, or `tmp` in the program's own directory, made on first use
/// (crates/tempfile-krate/make-tempfile.py in Krate).
fn platform_temp_dir() -> PathBuf {
    #[cfg(target_os = "wasi")]
    {
        let dir = env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("tmp"));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
    #[cfg(not(target_os = "wasi"))]
    {
        env::temp_dir()
    }
}
'''


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    hits = glob.glob(os.path.join(home, "registry/src/*", f"tempfile-{VERSION}"))
    if not hits:
        sys.exit(f"tempfile {VERSION} is not in cargo's registry; `cargo fetch` a project using it")
    out = os.path.join(os.path.abspath(sys.argv[1]), f"tempfile-{VERSION}")
    if os.path.exists(out):
        shutil.rmtree(out)
    shutil.copytree(hits[0], out)
    for f in glob.glob(os.path.join(out, ".cargo*")):
        os.remove(f)
    path = os.path.join(out, "src/env.rs")
    text = open(path).read()
    if text.count(OLD) != 1:
        sys.exit(f"{path}: its temp_dir changed; expected {OLD.strip()!r} once")
    open(path, "w").write(text.replace(OLD, NEW) + FN)
    print(out)


if __name__ == "__main__":
    main()
