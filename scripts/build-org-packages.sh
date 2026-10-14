#!/usr/bin/env bash
# Build the packages an IT team deploys, from a krate binary that is already
# built: a macOS installer package (.pkg), and Linux .deb and .rpm.
#
#   scripts/build-org-packages.sh macos <target> <krate-binary> <out-dir> [options]
#   scripts/build-org-packages.sh linux <target> <krate-binary> <out-dir> [options]
#
# Options:
#   --tooling PATH         cargo-component, placed beside krate (both OSes)
#   --opener PATH          "Krate Opener.app", placed beside krate (macOS)
#   --player PATH          "Krate Player.app", installed in /Applications (macOS)
#   --installer-identity NAME
#                          a "Developer ID Installer: ..." identity in the
#                          keychain; the .pkg is signed with it (macOS)
#   --keychain PATH        the keychain holding those identities
#   --app-identity NAME    a "Developer ID Application: ..." identity; signs
#                          the copy of cargo-component that goes in the .pkg,
#                          so a notarized package carries no unsigned code
#
# Version from KRATE_VERSION (v0.5.5 or v0.5.5-rc6 are fine), else the
# workspace manifest.
#
# What lands where, and why:
#
#   macOS  /usr/local/bin/krate, cargo-component, Krate Opener.app
#          /Applications/Krate Player.app
#     The same places scripts/install.sh uses. Krate finds cargo-component and
#     the gift opener beside its own binary, so they go in one directory.
#     The package runs no install scripts.
#
#   Linux  /usr/lib/krate/krate, /usr/lib/krate/cargo-component
#          /usr/bin/krate -> /usr/lib/krate/krate
#          /usr/share/applications/dev.krate.open.desktop  (double-click)
#          /usr/share/mime/packages/krate.xml               (.krate type)
#     Linux resolves the running binary through the symlink, so "beside its
#     own binary" is /usr/lib/krate and cargo-component stays off PATH. The
#     desktop and MIME files are what scripts/install-krate-desktop.sh writes
#     per person, here machine-wide. No install scripts: dpkg and rpm refresh
#     the desktop and MIME caches from their own triggers.
#
# Neither package writes /etc/krate/policy.toml or any policy: a policy is the
# organisation's to place (see docs/book/src/running-in-an-organisation.md).
#
# Output names carry the version and the target, like every other release
# file. An unsigned macOS package says so in its name:
#   krate-<v>-<target>.pkg   or   krate-<v>-<target>-unsigned.pkg
#   krate-<v>-<target>.deb   krate-<v>-<target>.rpm
set -euo pipefail

# CI on an x86_64 Linux runner: put a pinned, checksum-verified nfpm in DIR.
#   scripts/build-org-packages.sh install-nfpm DIR
# nfpm only arranges files, so the x86_64 tool packages arm64 binaries too.
NFPM_VERSION=2.48.0
NFPM_SHA256=6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750
if [[ "${1:-}" == install-nfpm ]]; then
  dir="${2:?usage: scripts/build-org-packages.sh install-nfpm DIR}"
  mkdir -p "$dir"
  tmp="$(mktemp -d)"
  curl -fsSL -o "$tmp/nfpm.tar.gz" \
    "https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}/nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz"
  echo "${NFPM_SHA256}  $tmp/nfpm.tar.gz" | sha256sum -c -
  tar -xzf "$tmp/nfpm.tar.gz" -C "$tmp" nfpm
  install -m 0755 "$tmp/nfpm" "$dir/nfpm"
  rm -rf "$tmp"
  "$dir/nfpm" --version | grep -i version || true
  exit 0
fi

usage() { sed -n '2,24p' "$0" >&2; exit 2; }
kind="${1:-}"; target="${2:-}"; binary="${3:-}"; out="${4:-}"
[[ -n "$kind" && -n "$target" && -n "$binary" && -n "$out" ]] || usage
shift 4
tooling="" opener="" player="" installer_identity="" app_identity="" keychain=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tooling) tooling="$2"; shift 2 ;;
    --opener) opener="$2"; shift 2 ;;
    --player) player="$2"; shift 2 ;;
    --installer-identity) installer_identity="$2"; shift 2 ;;
    --app-identity) app_identity="$2"; shift 2 ;;
    --keychain) keychain="$2"; shift 2 ;;
    *) echo "build-org-packages: unknown option $1" >&2; usage ;;
  esac
done

root="$(cd "$(dirname "$0")/.." && pwd)"
version="${KRATE_VERSION:-}"
if [[ -z "$version" ]]; then
  version="$(awk -F'"' '/^version = / { print $2; exit }' "$root/Cargo.toml")"
fi
version="${version#v}"
[[ -f "$binary" ]] || { echo "build-org-packages: no binary at $binary" >&2; exit 1; }
for optional in "$tooling" "$opener" "$player"; do
  [[ -z "$optional" || -e "$optional" ]] || { echo "build-org-packages: $optional does not exist" >&2; exit 1; }
done
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

macos() {
  [[ "$(uname -s)" == Darwin ]] || { echo "build-org-packages: a .pkg is built on macOS" >&2; exit 1; }
  case "$target" in
    aarch64-apple-darwin) hosts="arm64" ;;
    # An Intel build also runs on Apple silicon, through Rosetta.
    x86_64-apple-darwin) hosts="x86_64,arm64" ;;
    *) echo "build-org-packages: $target is not a macOS target" >&2; exit 1 ;;
  esac
  local stage="$work/root" bin
  bin="$stage/usr/local/bin"
  mkdir -p "$bin"
  # Single binaries are copied without extended attributes: their signature
  # is inside the file, and an attribute in the payload becomes a ._ file
  # beside it. Bundles are copied WITH them, because a signed shell script
  # (the Player's launcher, the gift opener's `open`) keeps its signature in
  # extended attributes, and pkgbuild carries them into the payload.
  local file_copy=(ditto --noextattr --noqtn --noacl)
  "${file_copy[@]}" "$binary" "$bin/krate"
  chmod 0755 "$bin/krate"
  if [[ -n "$tooling" ]]; then
    "${file_copy[@]}" "$tooling" "$bin/cargo-component"
    chmod 0755 "$bin/cargo-component"
    if [[ -n "$app_identity" ]]; then
      codesign --force --options runtime --timestamp --sign "$app_identity" \
        ${keychain:+--keychain "$keychain"} "$bin/cargo-component"
    fi
  fi
  [[ -z "$opener" ]] || ditto "$opener" "$bin/Krate Opener.app"
  if [[ -n "$player" ]]; then
    mkdir -p "$stage/Applications"
    ditto "$player" "$stage/Applications/Krate Player.app"
  fi

  # Bundles install where the package says, not wherever Launch Services
  # last saw an app with the same identifier: an admin's "install to
  # /Applications" must mean exactly that.
  local plist="$work/components.plist"
  pkgbuild --analyze --root "$stage" "$plist" >/dev/null
  local n=0
  while /usr/libexec/PlistBuddy -c "Print :$n" "$plist" >/dev/null 2>&1; do
    /usr/libexec/PlistBuddy -c "Delete :$n:BundleIsRelocatable" "$plist" >/dev/null 2>&1 || true
    /usr/libexec/PlistBuddy -c "Add :$n:BundleIsRelocatable bool false" "$plist"
    n=$((n + 1))
  done

  pkgbuild --root "$stage" --component-plist "$plist" \
    --identifier tech.krate.cli --version "$version" \
    --install-location / "$work/krate-component.pkg" >/dev/null

  # A product archive (what productbuild makes) is what Intune, Jamf and
  # Kandji expect, and it is the only kind that can say which Macs it is for.
  productbuild --synthesize --package "$work/krate-component.pkg" "$work/distribution.xml" >/dev/null
  python3 - "$work/distribution.xml" "$hosts" "$version" <<'PY'
import sys, xml.etree.ElementTree as ET
path, hosts, version = sys.argv[1:]
tree = ET.parse(path)
doc = tree.getroot()
opts = doc.find("options")
if opts is None:
    opts = ET.SubElement(doc, "options")
opts.set("hostArchitectures", hosts)
opts.set("customize", "never")
opts.set("require-scripts", "false")
title = doc.find("title")
if title is None:
    title = ET.SubElement(doc, "title")
title.text = f"Krate {version}"
tree.write(path, encoding="utf-8", xml_declaration=True)
PY

  local name="krate-${version}-${target}"
  local sign=()
  if [[ -n "$installer_identity" ]]; then
    sign=(--sign "$installer_identity" --timestamp)
    [[ -z "$keychain" ]] || sign+=(--keychain "$keychain")
  else
    name="${name}-unsigned"
  fi
  rm -f "$out/$name.pkg"
  productbuild --distribution "$work/distribution.xml" --package-path "$work" \
    ${sign[@]+"${sign[@]}"} "$out/$name.pkg" >/dev/null
  if [[ -n "$installer_identity" ]]; then
    pkgutil --check-signature "$out/$name.pkg"
  else
    echo "unsigned: $name.pkg (no Developer ID Installer identity was given)"
  fi
  echo "built $out/$name.pkg"
  # What it installs, so a CI log answers that without anyone unpacking it.
  pkgutil --payload-files "$out/$name.pkg" | grep -v '/Contents/' | sed 's/^/  /'
}

linux() {
  command -v nfpm >/dev/null || { echo "build-org-packages: nfpm is not installed (https://nfpm.goreleaser.com)" >&2; exit 1; }
  local arch
  case "$target" in
    x86_64-unknown-linux-gnu) arch=amd64 ;;
    aarch64-unknown-linux-gnu) arch=arm64 ;;
    *) echo "build-org-packages: $target is not a Linux target" >&2; exit 1 ;;
  esac
  local s="$work/stage"
  mkdir -p "$s"
  cp "$binary" "$s/krate"
  [[ -z "$tooling" ]] || cp "$tooling" "$s/cargo-component"
  cat > "$s/krate.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/x-krate">
    <comment>Krate app bundle</comment>
    <glob pattern="*.krate"/>
    <icon name="application-x-krate"/>
  </mime-type>
</mime-info>
XML
  cat > "$s/dev.krate.open.desktop" <<'DESK'
[Desktop Entry]
Type=Application
Name=Krate
Comment=Open a Krate app after reviewing what it can access
Exec=/usr/bin/krate run %f --consent
Icon=application-x-krate
Terminal=false
NoDisplay=true
MimeType=application/x-krate;
Categories=Utility;
DESK
  cp "$root/docs/landing/krate-document-icon.png" "$s/application-x-krate.png"

  local cfg="$work/nfpm.yaml"
  {
    cat <<YAML
name: krate
arch: ${arch}
platform: linux
version: "${version}"
version_schema: semver
release: "1"
section: utils
priority: optional
maintainer: "Krate Labs <yashraj@krate.tech>"
vendor: Krate Labs
homepage: https://krate.tech
license: MIT OR Apache-2.0
description: |-
  Opens .krate apps: one file that runs on Mac, Windows and Linux.
  Each app runs in a sandbox and asks before it uses files, the network
  or devices.
contents:
  - src: ${s}/krate
    dst: /usr/lib/krate/krate
    file_info: { mode: 0755 }
  - src: /usr/lib/krate/krate
    dst: /usr/bin/krate
    type: symlink
  - src: ${s}/dev.krate.open.desktop
    dst: /usr/share/applications/dev.krate.open.desktop
    file_info: { mode: 0644 }
  - src: ${s}/krate.xml
    dst: /usr/share/mime/packages/krate.xml
    file_info: { mode: 0644 }
  - src: ${s}/application-x-krate.png
    dst: /usr/share/pixmaps/application-x-krate.png
    file_info: { mode: 0644 }
  - src: ${root}/LICENSE-MIT
    dst: /usr/share/doc/krate/LICENSE-MIT
    file_info: { mode: 0644 }
  - src: ${root}/LICENSE-APACHE
    dst: /usr/share/doc/krate/LICENSE-APACHE
    file_info: { mode: 0644 }
YAML
    if [[ -n "$tooling" ]]; then
      cat <<YAML
  - src: ${s}/cargo-component
    dst: /usr/lib/krate/cargo-component
    file_info: { mode: 0755 }
YAML
    fi
  } > "$cfg"

  for fmt in deb rpm; do
    local file="$out/krate-${version}-${target}.${fmt}"
    rm -f "$file"
    nfpm package --config "$cfg" --packager "$fmt" --target "$file" >/dev/null
    echo "built $file"
  done
}

case "$kind" in
  macos) macos ;;
  linux) linux ;;
  *) usage ;;
esac
