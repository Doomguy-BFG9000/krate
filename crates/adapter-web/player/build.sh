#!/bin/sh
# Rebuild everything the web Player proofs need (IC-956 Phase 1). Nothing it
# writes is committed. Run from anywhere:
#   sh crates/adapter-web/player/build.sh
#   cd crates/adapter-web/player && node phase1.mjs ../../../evidence/web-player/phase1.json 10
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
export PATH="$HOME/.cargo/bin:$PATH"

# 1. The proof apps, as components (the same bytes a .krate carries).
for app in krate-curl krate-hello-gui krate-bigscroll krate-convert krate-checklist krate-orgstore-probe; do
  (cd "$repo/apps/$app" && cargo component build --release)
done

# 2. What the hub runs and serves: the validator, the transpiler, the Player.
sh "$repo/scripts/build-hub-validator.sh"
sh "$repo/scripts/build-web-player.sh"

# 3. The bundles the proofs open, and the pinned browsers (Playwright's own
#    builds; the Mac's Safari is not touched).
cd "$here"
npm ci --no-audit --no-fund
(cd "$repo/cloud/worker" && npm ci --no-audit --no-fund)
node prepare.mjs
npx playwright install --only-shell chromium webkit firefox

# Phase 0's pulley comparison (evidence/web-player/README.md) still builds
# on its own: crates/adapter-web/pulley-probe.
