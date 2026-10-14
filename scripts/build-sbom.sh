#!/usr/bin/env bash
# Write the CycloneDX SBOMs a release ships, for one target.
#
#   scripts/build-sbom.sh <target-triple> <out-dir> [--no-speech] [--studio]
#
# Writes, into <out-dir>:
#   krate-<version>-<target>.cdx.json         the krate CLI and every crate it links
#   krate-studio-<version>-<target>.cdx.json  Studio's Rust shell (with --studio)
#   krate-studio-ui-<version>.cdx.json        the JavaScript bundled into Studio's
#                                             interface (with --studio)
#
# Version from KRATE_VERSION (a tag such as v0.5.5 is fine), else the
# workspace manifest. Needs cargo-cyclonedx (cargo install cargo-cyclonedx
# --locked --version 0.5.7) and, for --studio, node and npx.
#
# The SBOM lists what the release BUILD links for that target and feature
# set, read from Cargo.lock by `cargo metadata` -- nothing is compiled, so it
# runs on any machine for any target. --no-speech matches the targets the
# release builds without speech-to-text (see release.yml); the default adds
# the `speech` feature, as the release does.
set -euo pipefail

target="${1:?usage: scripts/build-sbom.sh <target-triple> <out-dir> [--no-speech] [--studio]}"
out="${2:?usage: scripts/build-sbom.sh <target-triple> <out-dir> [--no-speech] [--studio]}"
shift 2
speech=1
studio=0
for arg in "$@"; do
  case "$arg" in
    --no-speech) speech=0 ;;
    --studio) studio=1 ;;
    *) echo "build-sbom: unknown argument $arg" >&2; exit 2 ;;
  esac
done

root="$(cd "$(dirname "$0")/.." && pwd)"
version="${KRATE_VERSION:-}"
if [[ -z "$version" ]]; then
  version="$(awk -F'"' '/^version = / { print $2; exit }' "$root/Cargo.toml")"
fi
version="${version#v}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"

cargo cyclonedx --version >/dev/null 2>&1 || {
  echo "build-sbom: cargo-cyclonedx is not installed:" >&2
  echo "  cargo install cargo-cyclonedx --locked --version 0.5.7" >&2
  exit 1
}

# cargo-cyclonedx writes one file beside EVERY workspace member's Cargo.toml,
# whichever manifest it is pointed at. Only the CLI's is wanted; the rest are
# removed so a run leaves the tree as it found it. The file name is unusual
# on purpose, so the cleanup cannot touch anything that was there before.
stamp="krate-sbom-$$"
cleanup() {
  find "$root/crates" "$root/apps" "$root/studio" -maxdepth 4 -name "${stamp}*.cdx.json" -delete 2>/dev/null || true
}
trap cleanup EXIT

features=()
if [[ "$speech" == 1 ]]; then
  features=(--features speech)
fi

cargo cyclonedx --manifest-path "$root/crates/cli/Cargo.toml" \
  --format json --spec-version 1.5 --target "$target" \
  ${features[@]+"${features[@]}"} --override-filename "${stamp}-cli.cdx" --quiet
mv "$root/crates/cli/${stamp}-cli.cdx.json" "$out/krate-${version}-${target}.cdx.json"

if [[ "$studio" == 1 ]]; then
  # Studio is its own workspace (studio/Cargo.toml), so this writes one file.
  cargo cyclonedx --manifest-path "$root/studio/Cargo.toml" \
    --format json --spec-version 1.5 --target "$target" \
    --override-filename "${stamp}-studio.cdx" --quiet
  mv "$root/studio/${stamp}-studio.cdx.json" "$out/krate-studio-${version}-${target}.cdx.json"

  # Studio's interface is plain files plus one bundle, studio/ui/vendor/
  # codemirror.js, built by scripts/codemirror/build.sh from the versions
  # pinned in its lockfile. That lockfile is the JavaScript's whole bill of
  # materials. Read from a copy so nothing is written into the repository.
  js="$(mktemp -d)"
  cp "$root/scripts/codemirror/package.json" "$root/scripts/codemirror/package-lock.json" "$js/"
  (cd "$js" && npx --yes @cyclonedx/cyclonedx-npm@6.0.1 --package-lock-only \
    --spec-version 1.5 --output-format JSON \
    --output-file "$out/krate-studio-ui-${version}.cdx.json" package.json >/dev/null)
  rm -rf "$js"
fi

python3 - "$out" <<'PY'
import json, pathlib, sys
for f in sorted(pathlib.Path(sys.argv[1]).glob("*.cdx.json")):
    d = json.loads(f.read_text())
    assert d.get("bomFormat") == "CycloneDX", f
    top = d["metadata"]["component"]
    print(f"{f.name}: CycloneDX {d['specVersion']}, {top['name']} {top.get('version', '')}, {len(d.get('components', []))} components")
PY
