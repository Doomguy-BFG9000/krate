#!/bin/sh
# Rebuild everything the web Player spike serves into ./build (IC-956 Phase 0).
# Nothing it writes is committed. Run from anywhere:
#   sh crates/adapter-web/player/build.sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
export PATH="$HOME/.cargo/bin:$PATH"

# 1. The two apps, as components (the same bytes a .krate carries).
(cd "$repo/apps/krate-diceroll" && cargo component build --release)
(cd "$repo/apps/krate-hello-gui" && cargo component build --release)
dice="$repo/apps/krate-diceroll/target/wasm32-wasip1/release/krate_diceroll.wasm"
hello="$repo/apps/krate-hello-gui/target/wasm32-wasip1/release/krate_hello_gui.wasm"

# 2. jco: each component to core wasm + JS glue. The CLI app and the
#    Worker path use the sync glue; the main-thread GUI path uses JSPI.
cd "$here"
npm ci --no-audit --no-fund
npx jco transpile "$dice" -o build/diceroll-sync --name app --instantiation async --no-nodejs-compat --no-typescript -b 0 -q
npx jco transpile "$hello" -o build/hello-sync --name app --instantiation async --no-nodejs-compat --no-typescript -b 0 -q
npx jco transpile "$hello" -o build/hello-jspi --name app --instantiation async --no-nodejs-compat --no-typescript -b 0 -q \
  --async-mode jspi --async-imports 'krate:ui/events@0.1.0#wait' --async-exports run

# 3. The painter (krate-layout + krate-adapter-common) for wasm32.
(cd "$repo" && cargo build -p krate-adapter-web --target wasm32-unknown-unknown --release)
wasm-bindgen --target web --no-typescript --out-dir build/painter \
  "$repo/target/wasm32-unknown-unknown/release/krate_adapter_web.wasm"

# 4. The comparison: wasmtime/pulley compiled to wasm32, and diceroll
#    precompiled for it (pulley32 -- a wasm32 host is 32-bit).
(cd "$here/../pulley-probe" && cargo build --release --bin precompile \
  && cargo build --release --lib --target wasm32-unknown-unknown)
mkdir -p build/pulley
"$here/../pulley-probe/target/release/precompile" "$dice" build/pulley/diceroll.cwasm
wasm-bindgen --target web --no-typescript --out-dir build/pulley \
  "$here/../pulley-probe/target/wasm32-unknown-unknown/release/krate_pulley_probe.wasm"

# 5. Browsers for Playwright (its own builds; the Mac's Safari is not touched).
npx playwright install --only-shell chromium webkit firefox
