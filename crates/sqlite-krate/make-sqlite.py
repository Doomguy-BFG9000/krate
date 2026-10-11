#!/usr/bin/env python3
"""Make SQLite safe for a threaded Krate app: libsqlite3-sys 0.35.0 and rusqlite 0.37.0 (IC-932).

    python3 crates/sqlite-krate/make-sqlite.py OUT_DIR

copies libsqlite3-sys 0.35.0 and rusqlite 0.37.0 from cargo's registry to
OUT_DIR and changes one choice in each.

libsqlite3-sys: its build script.
For every wasm32-wasi target it compiles the bundled SQLite with
SQLITE_THREADSAFE=0 -- no mutexes at all -- which is right where WASI has no
threads and unsound in a threaded Krate app (wasm32-wasip1-threads), where a
program such as Veloren's server opens connections on several threads. On
that target the patched crate builds SQLite serialized (SQLITE_THREADSAFE=1)
over wasi-libc's pthreads; every other target is unchanged.

rusqlite: it skips its check that SQLite is thread-safe on any wasm32 ("no
threading"); on wasm with atomics it now makes the check, as natively, so a
single-threaded SQLite in a threaded app is refused rather than raced.

Patch them in with

    [patch.crates-io]
    libsqlite3-sys = { path = "OUT_DIR/libsqlite3-sys-0.35.0" }
    rusqlite = { path = "OUT_DIR/rusqlite-0.37.0" }

Locking is SQLite's own WASI choice and stays: the unix-dotfile VFS, so one
connection holds a database at a time and a second reader waits or gets
SQLITE_BUSY. A Krate app's files are its own, so nothing outside it contends.
"""

import glob
import os
import shutil
import sys

VERSION = "0.35.0"
OLD = """            cfg.flag("-USQLITE_THREADSAFE")
                .flag("-DSQLITE_THREADSAFE=0")"""
NEW = """            // Krate (crates/sqlite-krate/make-libsqlite3.py): a threaded app
            // has real threads, so SQLite keeps its mutexes there.
            let threads = env::var("TARGET").is_ok_and(|v| v == "wasm32-wasip1-threads");
            cfg.flag("-USQLITE_THREADSAFE")
                .flag(if threads {
                    "-DSQLITE_THREADSAFE=1"
                } else {
                    "-DSQLITE_THREADSAFE=0"
                })"""


RUSQLITE = "0.37.0"
NO_THREADS = 'all(target_arch = "wasm32", not(target_feature = "atomics"))'


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


def replace(path, old, new, count):
    text = open(path).read()
    if text.count(old) != count:
        sys.exit(f"{path}: expected {count} of {old.strip()[:60]!r}, found {text.count(old)}")
    open(path, "w").write(text.replace(old, new))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = copy("libsqlite3-sys", VERSION)
    replace(os.path.join(out, "build.rs"), OLD, NEW, 1)
    print(out)
    out = copy("rusqlite", RUSQLITE)
    conn = os.path.join(out, "src/inner_connection.rs")
    replace(conn, '#[cfg(target_arch = "wasm32")]\nfn ensure_safe_sqlite_threading_mode',
            f'#[cfg({NO_THREADS})]\nfn ensure_safe_sqlite_threading_mode', 1)
    replace(conn, '#[cfg(not(any(target_arch = "wasm32")))]\nfn ensure_safe_sqlite_threading_mode',
            f'#[cfg(not({NO_THREADS}))]\nfn ensure_safe_sqlite_threading_mode', 1)
    print(out)


if __name__ == "__main__":
    main()
