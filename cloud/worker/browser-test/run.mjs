// IC-958: the browser as an organisation owner's first device, in real
// browsers, against a real local hub.
//
// For each browser (Chromium and WebKit): a fresh organisation is made;
// the page makes the organisation's data key and signing key in WebCrypto
// (window.KrateOrgDevice.ready), keeps its device in IndexedDB, signs a
// real .krate through the hub's validator module (sign), and uploads it;
// the hub must take it as the organisation's -- signed by its root and
// marked for it -- and release it alone (solo, nobody else can approve).
// Then the page is reloaded: the device comes back from IndexedDB and
// signs again, and a fresh browser profile (storage cleared) cannot.
//
//   cd cloud/worker/browser-test && npm ci && node run.mjs http://127.0.0.1:8901
//
// The hub: `npx wrangler dev --local ... --var
// KRATE_TEST_IDENTITIES:tok-alice=alice,...` in cloud/worker, after
// scripts/build-hub-validator.sh (which also writes player-dist/validator.wasm).
import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { chromium, webkit } from "playwright";

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "..", "..", "..");
const HUB = (process.argv[2] || "http://127.0.0.1:8901").replace(/\/$/, "");
const ONLY = (process.argv[3] || "chromium,webkit").split(",");
const BOUNCE = readFileSync(join(repo, "evidence", "ported", "bounce.krate"));

const files = {
  "/org-device.html": [join(here, "org-device.html"), "text/html"],
  "/kit/org-device.js": [join(repo, "docs", "landing", "kit", "org-device.js"), "text/javascript"],
  "/bounce.krate": [join(repo, "evidence", "ported", "bounce.krate"), "application/octet-stream"],
};
const server = createServer((req, res) => {
  const f = files[new URL(req.url, "http://x").pathname];
  if (!f) return res.writeHead(404).end();
  res.writeHead(200, { "content-type": f[1], "cache-control": "no-store" }).end(readFileSync(f[0]));
});
await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
const SITE = `http://127.0.0.1:${server.address().port}`;

async function hub(method, path, token, body) {
  const headers = { authorization: `Bearer ${token}`, accept: "application/json" };
  if (method !== "GET") headers["idempotency-key"] = Math.random().toString(16).slice(2);
  if (body !== undefined) headers["content-type"] = "application/json";
  const r = await fetch(HUB + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await r.text();
  let value = null;
  try { value = JSON.parse(text); } catch { value = text; }
  return { status: r.status, body: value };
}

const fail = [];
const check = (ok, what, detail) => {
  if (!ok) fail.push(`${what}: ${JSON.stringify(detail)}`);
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
};

const results = [];
for (const [name, type] of [["chromium", chromium], ["webkit", webkit]].filter(([n]) => ONLY.includes(n))) {
  const slug = `acme-${name}-${Date.now().toString(36)}`;
  const made = await hub("POST", "/orgs", "tok-alice", { slug, name: `Acme (${name})` });
  check(made.status === 201, `${name}: the organisation is made`, made);
  const browser = await type.launch({ headless: true });
  const context = await browser.newContext();
  const page = await context.newPage();
  const logs = [];
  page.on("pageerror", (e) => logs.push(e.message));
  const url = `${SITE}/org-device.html?hub=${encodeURIComponent(HUB)}&token=tok-alice`;
  await page.goto(url);
  const t0 = Date.now();
  const first = await page.evaluate(async (slug) => {
    try {
      const ready = await window.KrateOrgDevice.ready(slug, { name: "Alice's browser" });
      const bytes = new Uint8Array(await (await fetch("/bounce.krate")).arrayBuffer());
      const signed = await window.KrateOrgDevice.sign(slug, bytes);
      return { ready, signed: Array.from(signed) };
    } catch (e) {
      return { error: e.message, reason: e.reason };
    }
  }, slug);
  const setupMs = Date.now() - t0;
  check(!first.error, `${name}: keys made and the app signed in the browser`, { ...first, logs });
  if (first.error) {
    await browser.close();
    continue;
  }
  check(first.ready.made === true && /^[0-9A-Z-]{31}$/.test(first.ready.recoveryCode || ""), `${name}: a recovery code, shown once`, first.ready);

  // The hub takes the browser's signature as the organisation's.
  const bytes = new Uint8Array(first.signed);
  const up = await fetch(`${HUB}/orgs/${slug}/candidates`, {
    method: "POST",
    headers: { authorization: "Bearer tok-alice", "idempotency-key": "up-" + name, "content-type": "application/octet-stream" },
    body: bytes,
  });
  const sent = await up.json();
  check(up.status === 201 && sent.state === "waiting-for-approval" && sent.solo_release === true, `${name}: the hub takes it as ${slug}'s, signed and marked`, sent);
  const released = await hub("POST", `/orgs/${slug}/candidates/${sent.digest}/approve`, "tok-alice", { accept: [] });
  check(released.status === 200 && released.body.release.solo === true, `${name}: released alone, marked solo`, released);
  const org = (await hub("GET", `/orgs/${slug}`, "tok-alice")).body.org;
  const keys = (await hub("GET", `/orgs/${slug}/keys?device=x`, "tok-alice")).body;
  check(org.release_rule === "solo" && /^[0-9a-f]{64}$/.test(org.root_key), `${name}: the organisation's root is the browser-made key`, org);
  check(keys.devices.length === 1 && keys.devices[0].kind === "browser" && keys.devices[0].name === "Alice's browser", `${name}: the browser is registered as a device`, keys.devices);
  const audit = (await hub("GET", `/orgs/${slug}/audit`, "tok-alice")).body;
  const actions = audit.events.map((e) => e.action);
  check(["key.init", "root-key.register", "signing.init", "candidate.publish", "candidate.approve"].every((a) => actions.includes(a)) && audit.chain_ok, `${name}: every step is in the audit`, actions);

  // Reload: the device comes back from IndexedDB and signs again.
  await page.reload();
  const again = await page.evaluate(async (slug) => {
    try {
      const ready = await window.KrateOrgDevice.ready(slug);
      const bytes = new Uint8Array(await (await fetch("/bounce.krate")).arrayBuffer());
      return { ready, signed: (await window.KrateOrgDevice.sign(slug, bytes)).length };
    } catch (e) {
      return { error: e.message, reason: e.reason };
    }
  }, slug);
  check(!again.error && again.ready.made === false && again.ready.holds === true && again.signed > 0, `${name}: after a reload the same device signs again`, again);

  // A new profile (this site's data cleared): not a holder until restored.
  const fresh = await browser.newContext();
  const other = await fresh.newPage();
  await other.goto(url);
  const cleared = await other.evaluate(async (slug) => {
    try {
      await window.KrateOrgDevice.sign(slug, new Uint8Array([1]));
      return { error: null };
    } catch (e) {
      return { error: e.message, reason: e.reason };
    }
  }, slug);
  check(cleared.reason === "not-holder", `${name}: a browser whose data was cleared cannot sign until restored`, cleared);
  results.push({ browser: name, version: browser.version(), slug, setup_and_sign_ms: setupMs });
  await browser.close();
}
server.close();
console.log(JSON.stringify(results));
if (fail.length) {
  console.error(fail.join("\n"));
  process.exit(1);
}
