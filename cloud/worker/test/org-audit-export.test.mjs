/* Audit export over a signed webhook (IC-955): the real worker and the real
 * Org store over SQLite, delivering through the store's alarm to a real
 * HTTP receiver on this machine that checks every signature itself with
 * node:crypto -- a second implementation from the hub's WebCrypto one.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-audit-export.test.mjs
 */
import { test, after } from "node:test";
import assert from "node:assert";
import { createServer } from "node:http";
import { createHmac, timingSafeEqual } from "node:crypto";
import { orgEnv, call, acme } from "./org-tools.mjs";
import { verifySignature, backoffSeconds, EXPORT_GIVE_UP_SECONDS } from "../src/org-audit-export.js";

const HUB = "http://127.0.0.1:8787";

/* A SIEM's webhook: records each request, checks its signature with the
 * secret it was given, and answers as told: "ok", "fail" (500), "redirect",
 * or "check" (200 only for a good signature, 401 otherwise). */
const receivers = [];
after(async () => {
  for (const r of receivers) await new Promise((res) => r.server.close(res));
});
async function receiver() {
  const r = { got: [], mode: "check", secret: null };
  r.server = createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      const header = req.headers["x-krate-signature"] || "";
      const m = /^t=(\d+),v1=([0-9a-f]{64})$/.exec(header);
      let good = false;
      if (m && r.secret) {
        const want = createHmac("sha256", r.secret).update(`${m[1]}.${body}`).digest();
        good = timingSafeEqual(want, Buffer.from(m[2], "hex"));
      }
      r.got.push({ body, json: JSON.parse(body), header, id: req.headers["x-krate-event-id"], good, t: m ? Number(m[1]) : null });
      if (r.mode === "fail") res.writeHead(500).end("down");
      else if (r.mode === "redirect") res.writeHead(302, { location: "http://127.0.0.1:1/elsewhere" }).end();
      else if (r.mode === "check") res.writeHead(good ? 200 : 401).end(good ? "ok" : "bad signature");
      else res.writeHead(204).end();
    });
  });
  await new Promise((res) => r.server.listen(0, "127.0.0.1", res));
  r.url = `http://127.0.0.1:${r.server.address().port}/krate/audit`;
  receivers.push(r);
  return r;
}

async function world(extra = {}, clock = null) {
  const env = orgEnv({ PUBLIC_BASE: HUB, ORG_SECRET_KEY: "org-secret-key-for-tests", ...extra });
  await acme(env);
  if (clock) at(env.ORGS.object("acme"), clock);
  const r = await receiver();
  const set = await call(env, "alice", "PUT", "/orgs/acme/audit-export", { json: { url: r.url } });
  assert.strictEqual(set.status, 201, JSON.stringify(set.body));
  r.secret = set.body.secret;
  return { env, r, obj: env.ORGS.object("acme"), set };
}

/* The store's alarm, as Cloudflare runs it: when it is due. */
async function runAlarm(obj) {
  obj.ctx.storage.alarmAt = null;
  return obj.alarm();
}
const state = async (env, who = "alice") => (await call(env, who, "GET", "/orgs/acme/audit-export")).body;
const at = (obj, seconds) => {
  obj.now = () => seconds;
};

test("an owner sets the webhook: the secret is shown once, kept encrypted, and only an owner sets it up", async () => {
  const { env, r, obj, set } = await world();
  assert.match(set.body.secret, /^whsec_[A-Za-z0-9_-]{43}$/);
  assert.strictEqual(set.headers.get("cache-control"), "no-store");
  const s = await state(env);
  assert.deepStrictEqual([s.url, s.has_secret, s.secret], [r.url, true, undefined]);
  const all = obj.sql.exec("SELECT * FROM audit_export").toArray();
  assert.ok(!JSON.stringify(all).includes(r.secret), "never stored in the clear");
  assert.ok(!JSON.stringify((await call(env, "alice", "GET", "/orgs/acme/audit")).body).includes(r.secret));
  // Changing the URL keeps the secret and shows nothing new.
  const again = await call(env, "alice", "PUT", "/orgs/acme/audit-export", { json: { url: `${r.url}?k=1` } });
  assert.deepStrictEqual([again.status, again.body.secret], [200, undefined]);
  // An admin watches it; nobody but an owner changes it.
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: "erin", role: "admin" } });
  await call(env, "erin", "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  assert.strictEqual((await call(env, "erin", "GET", "/orgs/acme/audit-export")).status, 200);
  for (const who of ["erin", "bob", "carol"]) {
    assert.strictEqual((await call(env, who, "PUT", "/orgs/acme/audit-export", { json: { url: r.url } })).status, 403, who);
    assert.strictEqual((await call(env, who, "POST", "/orgs/acme/audit-export/test")).status, 403, who);
    assert.strictEqual((await call(env, who, "DELETE", "/orgs/acme/audit-export")).status, 403, who);
  }
  for (const who of ["bob", "dan"]) assert.strictEqual((await call(env, who, "GET", "/orgs/acme/audit-export")).status, 403, who);
});

test("the URL is an https receiver: no plain http, no credentials, no private address; no ORG_SECRET_KEY, no export", async () => {
  const env = orgEnv({ ORG_SECRET_KEY: "k" }); // the production-shaped hub
  await acme(env);
  for (const url of ["http://siem.example.com/x", "https://user:pw@siem.example.com/x", "https://10.0.0.8/x", "https://127.0.0.1/x", "https://[::1]/x", "https://192.168.1.4/x", "ftp://siem.example.com", "", "x".repeat(3000)]) {
    const r = await call(env, "alice", "PUT", "/orgs/acme/audit-export", { json: { url } });
    assert.deepStrictEqual([r.status, r.body.error, url.slice(0, 40)], [400, "bad-request", url.slice(0, 40)]);
  }
  assert.strictEqual((await call(env, "alice", "PUT", "/orgs/acme/audit-export", { json: { url: "https://siem.example.com/krate?key=abc" } })).status, 201);
  // The audit names the host, never the URL's query.
  const ev = (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events.find((e) => e.action === "audit-export.set");
  assert.strictEqual(ev.target, "audit-export:siem.example.com");
  const off = orgEnv();
  await acme(off);
  const r = await call(off, "alice", "PUT", "/orgs/acme/audit-export", { json: { url: "https://siem.example.com/x" } });
  assert.deepStrictEqual([r.status, r.body.error], [503, "audit-export-off"]);
});

test("every audit event is POSTed in order, signed so the receiver verifies it, and the delivery shows on the dashboard", async () => {
  const { env, r, obj } = await world();
  // Things happen: a role change, a refused attempt, the test button.
  await call(env, "alice", "PUT", `/orgs/acme/members/${encodeURIComponent("github:dan")}`, { json: { role: "developer" } });
  await call(env, "dan", "GET", "/orgs/acme/audit");
  const t = await call(env, "alice", "POST", "/orgs/acme/audit-export/test");
  assert.strictEqual(t.status, 202);
  assert.ok(obj.ctx.storage.alarmAt, "the alarm was set for the queue");
  const before = await state(env);
  assert.strictEqual(before.delivery.pending, 4);

  const done = await runAlarm(obj);
  assert.deepStrictEqual(done, { delivered: 4, failed: 0, given_up: 0 });
  const audit = (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events;
  const sent = r.got.map((g) => g.json.event);
  assert.deepStrictEqual(sent.map((e) => e.action), ["audit-export.set", "member.role", "audit.read", "audit-export.test"]);
  // Exactly the stored events, chain fields and all.
  for (const e of sent) assert.deepStrictEqual(e, audit.find((a) => a.seq === e.seq));
  assert.ok(sent.every((e, i) => i === 0 || e.prev_hash === sent[i - 1].hash));
  for (const g of r.got) {
    assert.strictEqual(g.good, true, "node:crypto agrees with the signature");
    assert.strictEqual(await verifySignature(r.secret, g.header, g.body, { now: g.t }), true);
    assert.strictEqual(await verifySignature(r.secret, g.header, g.body.replace("acme", "acmf"), { now: g.t }), false);
    assert.strictEqual(await verifySignature(r.secret, g.header, g.body, { now: g.t + 301 }), false, "an old t is refused");
    assert.strictEqual(g.id, `acme:${g.json.event.seq}`);
    assert.deepStrictEqual([g.json.type, g.json.v, g.json.org], ["krate.audit.event", 1, "acme"]);
  }
  const s = await state(env);
  assert.deepStrictEqual([s.delivery.pending, s.delivery.delivered, s.delivery.last_delivered_seq, s.delivery.last_error], [0, 4, sent[3].seq, null]);
  // Nothing left, so nothing more is sent.
  assert.deepStrictEqual(await runAlarm(obj), { delivered: 0, failed: 0, given_up: 0 });
  assert.strictEqual(r.got.length, 4);
});

test("a receiver that refuses keeps the event queued and retried with backoff, in order, until it takes it", async () => {
  let now = 1_900_000_000;
  const { env, r, obj } = await world({}, now);
  r.mode = "fail";
  await call(env, "alice", "POST", "/orgs/acme/audit-export/test");
  assert.deepStrictEqual(await runAlarm(obj), { delivered: 0, failed: 1, given_up: 0 });
  let s = await state(env);
  assert.strictEqual(s.delivery.pending, 2);
  assert.strictEqual(s.delivery.last_error, "the receiver answered 500");
  assert.strictEqual(s.delivery.next_attempt_at, now + backoffSeconds(1));
  assert.strictEqual(obj.ctx.storage.alarmAt, (now + 30) * 1000, "the alarm wakes for the retry");
  // Not due yet: nothing is sent.
  at(obj, now + 10);
  await runAlarm(obj);
  assert.strictEqual(r.got.length, 1);
  // Due: the same first event again, and only it -- the second waits.
  now += 30;
  at(obj, now);
  await runAlarm(obj);
  assert.deepStrictEqual(r.got.map((g) => [g.json.event.action, g.json.attempt]), [["audit-export.set", 1], ["audit-export.set", 2]]);
  assert.strictEqual((await state(env)).delivery.next_attempt_at, now + 60, "doubled");
  assert.deepStrictEqual([backoffSeconds(1), backoffSeconds(2), backoffSeconds(3), backoffSeconds(20)], [30, 60, 120, 3600]);
  // The receiver is back.
  r.mode = "check";
  now += 60;
  at(obj, now);
  assert.deepStrictEqual(await runAlarm(obj), { delivered: 2, failed: 0, given_up: 0 });
  assert.deepStrictEqual(r.got.slice(2).map((g) => g.json.event.action), ["audit-export.set", "audit-export.test"]);
  s = await state(env);
  assert.deepStrictEqual([s.delivery.pending, s.delivery.delivered], [0, 2]);
});

test("a body whose signature does not verify is refused by the receiver and never counted delivered", async () => {
  const { env, r, obj } = await world();
  r.secret = "whsec_not-the-one-the-hub-holds";
  await runAlarm(obj);
  assert.strictEqual(r.got.length, 1);
  assert.strictEqual(r.got[0].good, false);
  const s = await state(env);
  assert.deepStrictEqual([s.delivery.delivered, s.delivery.pending, s.delivery.last_status], [0, 1, 401]);
});

test("redirects are not followed, and an event nobody takes for 24 hours is given up and counted", async () => {
  const start = 1_900_000_000;
  const { env, r, obj } = await world({}, start);
  await call(env, "alice", "POST", "/orgs/acme/audit-export/test");
  r.mode = "redirect";
  await runAlarm(obj);
  assert.match((await state(env)).delivery.last_error, /redirect \(302\); redirects are not followed/);
  r.mode = "fail";
  at(obj, start + EXPORT_GIVE_UP_SECONDS - 1);
  await runAlarm(obj);
  assert.strictEqual((await state(env)).delivery.given_up, 0);
  at(obj, start + EXPORT_GIVE_UP_SECONDS + 1);
  const done = await runAlarm(obj);
  assert.strictEqual(done.given_up, 2, "the set event and the test event, both queued at start");
  const s = await state(env);
  assert.deepStrictEqual([s.delivery.given_up, s.delivery.pending, s.delivery.delivered], [2, 0, 0]);
});

test("a new secret signs from the next delivery on; switching the export off stops it and drops the queue", async () => {
  const { env, r, obj } = await world();
  const rotated = await call(env, "alice", "POST", "/orgs/acme/audit-export/secret");
  assert.strictEqual(rotated.status, 200);
  assert.notStrictEqual(rotated.body.secret, r.secret);
  await runAlarm(obj);
  assert.ok(r.got.length >= 1);
  assert.ok(r.got.every((g) => !g.good), "the old secret no longer verifies");
  r.secret = rotated.body.secret;
  r.mode = "check";
  // Still queued after the refusals; due again later.
  at(obj, Math.floor(Date.now() / 1000) + 3600);
  await runAlarm(obj);
  assert.strictEqual(r.got[r.got.length - 1].good, true);

  await call(env, "alice", "POST", "/orgs/acme/audit-export/test");
  assert.strictEqual((await call(env, "alice", "DELETE", "/orgs/acme/audit-export")).status, 204);
  const n = r.got.length;
  await runAlarm(obj);
  assert.strictEqual(r.got.length, n, "nothing sent once it is off");
  assert.strictEqual((await state(env)), null);
  const ev = (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events.find((e) => e.action === "audit-export.clear");
  assert.match(ev.reason, /^\d+ not sent$/);
});
