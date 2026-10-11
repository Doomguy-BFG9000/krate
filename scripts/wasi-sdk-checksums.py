#!/usr/bin/env python3
"""Print the PINNED_SHA256 table in crates/cli/src/wasi_sdk.rs (IC-941).

    python3 scripts/wasi-sdk-checksums.py [--tag wasi-sdk-34] [--version 34.0]

Downloads each wasi-sdk release archive Krate may fetch (about 110-150 MB
each, five of them) from github.com/WebAssembly/wasi-sdk and prints its
SHA-256 in the form the table takes. Run it only where downloading them is
allowed; Krate itself never fetches an archive whose digest is not pinned.
"""

import argparse
import hashlib
import urllib.request

ASSETS = ["arm64-macos", "x86_64-macos", "arm64-linux", "x86_64-linux", "x86_64-windows"]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", default="wasi-sdk-34")
    parser.add_argument("--version", default="34.0")
    args = parser.parse_args()
    for asset in ASSETS:
        name = f"wasi-sdk-{args.version}-{asset}.tar.gz"
        url = f"https://github.com/WebAssembly/wasi-sdk/releases/download/{args.tag}/{name}"
        digest = hashlib.sha256()
        with urllib.request.urlopen(url) as response:
            while chunk := response.read(1 << 20):
                digest.update(chunk)
        print(f'    ("{name}", Some("{digest.hexdigest()}")),')


if __name__ == "__main__":
    main()
