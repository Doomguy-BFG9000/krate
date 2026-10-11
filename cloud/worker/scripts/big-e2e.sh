#!/bin/sh
# The big-file path end to end, entirely on this machine (IC-945):
# `wrangler dev` with local R2 and KV, the build service on a local port,
# and the CLI publishing through them. Nothing reaches the live hub, and
# nothing here spends anybody's AI key -- judging is `krate judge`.
#
#   sh cloud/worker/scripts/big-e2e.sh /abs/path/to/krate [bundle.krate]
#
# Without a bundle it packs a 150 MB one (three 64 MiB parts) that runs as
# a CLI app. With one (Veloren.krate, say) it publishes that and checks the
# download, without running it.
set -eu
KRATE=${1:?the krate binary to test, by absolute path}
BUNDLE=${2:-}
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
W="$ROOT/cloud/worker"
T=$(mktemp -d "${TMPDIR:-/tmp}/krate-big-e2e.XXXXXX")
SECRET=e2e-builder-secret
HUB=http://127.0.0.1:8787
PIDS=""
cleanup() {
  for p in $PIDS; do kill "$p" 2>/dev/null || true; done
  # wrangler's runtime is its own process; make sure it is gone too.
  pkill -f "$W/node_modules/@cloudflare/workerd" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM
say() { printf '\n== %s\n' "$*"; }

[ -s "$W/src/validator.wasm" ] || sh "$ROOT/scripts/build-hub-validator.sh"
[ -x "$W/node_modules/.bin/wrangler" ] || (cd "$W" && npm install --no-audit --no-fund)

if curl -s -o /dev/null "$HUB/health" 2>/dev/null; then
  echo "something is already answering on $HUB -- stop it first"; exit 1
fi
say "the hub: wrangler dev, local R2 and KV in $T/state"
(cd "$W" && exec ./node_modules/.bin/wrangler dev --local --ip 127.0.0.1 --port 8787 \
  --persist-to "$T/state" \
  --var "KRATE_BUILDER_SECRET:$SECRET" --var BIG_PUBLISHERS:alice \
  --var KRATE_BUILDER_URL:http://127.0.0.1:8790 --var "PUBLIC_BASE:$HUB" \
  > "$T/wrangler.log" 2>&1) &
PIDS="$PIDS $!"
i=0
until curl -s -o /dev/null "$HUB/health" 2>/dev/null; do
  i=$((i + 1)); [ $i -gt 120 ] && { tail -30 "$T/wrangler.log"; exit 1; }
  sleep 1
done
kv() { (cd "$W" && ./node_modules/.bin/wrangler kv key put "$1" "$2" --binding APPS --local --persist-to "$T/state" >/dev/null 2>&1); }
kv session:krs_alice u1
kv user:u1 '{"login":"alice","name":"Alice","avatar_url":""}'

say "the build service on :8790"
mkdir -p "$T/builder"
PORT=8790 KRATE_HUB=$HUB KRATE_BUILDER_SECRET=$SECRET KRATE_BIN=$KRATE \
  KRATE_STATE_DIR="$T/builder" node "$ROOT/cloud/builder/src/server.js" > "$T/builder.log" 2>&1 &
PIDS="$PIDS $!"
i=0
until curl -s -o /dev/null http://127.0.0.1:8790/health 2>/dev/null; do
  i=$((i + 1)); [ $i -gt 30 ] && { tail -30 "$T/builder.log"; exit 1; }
  sleep 1
done

mkdir -p "$T/home/.krate"
printf '{"login":"alice","name":"Alice","avatar_url":null,"token":"krs_alice"}' > "$T/home/.krate/github.json"
RUNS=yes
if [ -z "$BUNDLE" ]; then
  say "packing a 150 MB app"
  mkdir -p "$T/app/assets"
  cp "$ROOT/crates/bundle/tests/fixtures/minimal-run.wasm" "$T/app/code.wasm"
  printf '[app]\nid = "dev.krate.e2e-big"\nname = "E2E Big"\nversion = "1.0.0"\nentry = "code.wasm"\nworld = "krate:app/cli@0.1.0"\n' > "$T/app/manifest.toml"
  head -c 157286400 /dev/urandom > "$T/app/assets/noise.bin"
  BUNDLE="$T/e2e-big.krate"
  HOME="$T/home" "$KRATE" pack "$T/app/code.wasm" --manifest "$T/app/manifest.toml" -o "$BUNDLE" > /dev/null
else
  RUNS=no
fi
SIZE=$(wc -c < "$BUNDLE" | tr -d ' ')
SHA=$(shasum -a 256 "$BUNDLE" | cut -d' ' -f1)
echo "bundle: $BUNDLE ($SIZE bytes, $SHA)"

say "krate publish through the big-file path"
START=$(date +%s)
HOME="$T/home" "$KRATE" publish "$BUNDLE" --hub "$HUB" --shot /nonexistent.png | tee "$T/publish.log"
echo "publish took $(( $(date +%s) - START )) s"
grep -q "krate run $HUB/a/" "$T/publish.log"

say "served only after the verdict, byte for byte, marked big"
curl -s -D "$T/headers" -o "$T/download.krate" "$HUB/a/$SHA?dl=1"
grep -qi '^x-krate-tier: big' "$T/headers"
[ "$(shasum -a 256 "$T/download.krate" | cut -d' ' -f1)" = "$SHA" ]
echo "download matches its address"

say "the source and notices, attached beside it"
mkdir -p "$T/src/app" && printf 'the exact source\n' > "$T/src/app/README.md"
tar --zstd -cf "$T/source.tar.zst" -C "$T/src" app
printf 'Credits and licences\n' > "$T/notices.txt"
HOME="$T/home" "$KRATE" publish "$BUNDLE" --hub "$HUB" --shot /nonexistent.png \
  --source "$T/source.tar.zst" --notices "$T/notices.txt" | tee "$T/attach.log"
curl -s -o "$T/source.back" "$HUB/a/$SHA/source"
cmp "$T/source.tar.zst" "$T/source.back"
curl -s "$HUB/a/$SHA/notices" | grep -q 'Credits and licences'
echo "source and notices served"

if [ "$RUNS" = yes ]; then
  say "krate run <link>: saved to a file with progress, run from it"
  KRATE_CACHE_DIR="$T/cache" "$KRATE" run "$HUB/a/$SHA" --insecure-http --auto-grant 2>&1 | tail -5
  [ -s "$T/cache/downloads/$SHA.krate" ]
fi

say "a refused upload is removed, not listed"
# Straight through the API: the CLI refuses junk before it uploads.
python3 - "$HUB" <<'PY'
import hashlib, json, os, sys, time, urllib.request
hub = sys.argv[1]
junk = b"not a zip at all" + os.urandom(6 * 1024 * 1024)
sha = hashlib.sha256(junk).hexdigest()
def call(method, path, body=None, headers=None):
    req = urllib.request.Request(hub + path, data=body, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(req) as res:
            return res.status, res.read()
    except urllib.error.HTTPError as err:
        return err.code, err.read()
code, raw = call("POST", "/publish/big/start", json.dumps({"kind": "bundle", "size": len(junk), "sha256": sha}).encode(),
                 {"authorization": "Bearer krs_alice", "content-type": "application/json"})
assert code == 200, (code, raw)
up = json.loads(raw)
code, raw = call("PUT", f"/publish/big/{up['id']}/part/1", junk, {"x-krate-upload": up["token"]})
assert code == 200, (code, raw)
etag = json.loads(raw)["etag"]
code, raw = call("POST", f"/publish/big/{up['id']}/finish", json.dumps({"parts": [{"part": 1, "etag": etag}]}).encode(),
                 {"x-krate-upload": up["token"], "content-type": "application/json"})
assert code == 202, (code, raw)
for _ in range(100):
    code, raw = call("GET", f"/publish/big/{up['id']}", None, {"x-krate-upload": up["token"]})
    status = json.loads(raw)
    if status["status"] != "judging":
        break
    time.sleep(0.5)
print(status)
assert status["status"] == "refused", status
code, _ = call("GET", f"/a/{sha}?dl=1")
assert code == 404, code
print("refused, removed and not served")
PY

say "PASS (state kept in $T)"
trap - EXIT
cleanup
