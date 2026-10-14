/* Two holes found building the organisation pilot (IC-946), closed:
 *
 * 1. A request with no session must not grow an organisation's audit.
 *    Only authenticated principals are audited; anonymous hits on
 *    /orgs/<slug>/* and on private /a/<hash> are refused at the door and
 *    counted by the anonymous wall (RateGate), never by the store.
 * 2. The nightly scrub must know an organisation's private bytes are
 *    owned. They have no listing, alias or channel on purpose, so the
 *    scrub read them as orphans; and its repair must never delete them.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-anonymous-and-scrub.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { RateGate, judgeBundle } from "../src/index.js";
import { orgEnv, call, acme, fixture, worker } from "./org-tools.mjs";
import { sha256Hex } from "./r2-mock.mjs";
import "./github-stub.mjs";

/* The REAL RateGate behind a namespace stand-in, as rate-limit.test.mjs. */
function gateNamespace() {
  const objects = new Map();
  const ns = {
    requests: 0,
    idFromName: (name) => name,
    get(id) {
      if (!objects.has(id)) objects.set(id, new RateGate({}));
      const gate = objects.get(id);
      return {
        fetch: (url, init) => {
          ns.requests++;
          return gate.fetch(new Request(url, init));
        },
      };
    },
  };
  return ns;
}

const auditCount = (env) => env.ORGS.object("acme").ctx.storage.db.prepare("SELECT COUNT(*) AS n FROM audit").get().n;

async function released(env, name) {
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(name) });
  assert.strictEqual(up.status, 201, JSON.stringify(up.body));
  assert.strictEqual((await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`)).status, 200);
  return up.body.digest;
}

test("1,000 requests with no session add zero audit events, reach no store, and are rate-limited", async () => {
  const env = orgEnv({ RATE_GATE: gateNamespace() });
  await acme(env);
  const x = await released(env, "candidate-v1.krate");
  const before = auditCount(env);
  const storeRequests = env.ORGS.requests;
  const objects = env.ORGS.objects.size;

  const shapes = [
    ["GET", `/a/${x}`],
    ["HEAD", `/a/${x}`],
    ["GET", `/a/${x}?dl=1`],
    ["GET", "/orgs/acme"],
    ["GET", "/orgs/acme/audit"],
    ["GET", "/orgs/acme/releases"],
    ["POST", "/orgs/acme/invites"],
    ["POST", `/orgs/acme/candidates/${x}/approve`],
    ["DELETE", "/orgs/acme/members/alice"],
    ["GET", "/orgs/somewhere-new"],
  ];
  const counts = { refused: 0, limited: 0, other: 0 };
  for (let i = 0; i < 1000; i++) {
    const [method, path] = shapes[i % shapes.length];
    const headers = { "cf-connecting-ip": "203.0.113.7" };
    // Half send no Authorization, half a token nobody issued: both are
    // "no session" to the hub.
    if (i % 2) headers.authorization = "Bearer krs_made-up";
    const res = await worker.fetch(new Request(`https://hub.example${path}`, { method, headers }), env);
    if (res.status === 429) counts.limited += 1;
    else if (res.status === 401 || res.status === 403) counts.refused += 1;
    else counts.other += 1;
  }
  assert.strictEqual(counts.other, 0, JSON.stringify(counts));
  // 20 a minute per caller; a minute boundary during the run allows 20 more.
  assert.ok(counts.refused >= 20 && counts.refused <= 40, JSON.stringify(counts));
  assert.ok(counts.limited >= 960, `rate-limited: ${JSON.stringify(counts)}`);
  assert.strictEqual(auditCount(env), before, "zero audit events added");
  assert.strictEqual(env.ORGS.requests, storeRequests, "the organisation's store was never asked");
  assert.strictEqual(env.ORGS.objects.size, objects, "no organisation object was created by looking");
  assert.ok(env.RATE_GATE.requests >= 1000, "the anonymous wall counted them");

  // A refused anonymous request says why, and another caller has its own count.
  const fresh = await worker.fetch(
    new Request(`https://hub.example/a/${x}`, { headers: { "cf-connecting-ip": "198.51.100.9" } }),
    env,
  );
  assert.strictEqual(fresh.status, 403);
  assert.strictEqual((await fresh.json()).error, "not-signed-in");
  // The members are unaffected by the flood.
  assert.strictEqual((await call(env, "dan", "GET", `/a/${x}`)).status, 200);
  // And a signed-in stranger IS audited: authenticated attempts are the record.
  const stranger = await call(env, "stranger", "GET", `/a/${x}`);
  assert.strictEqual(stranger.body.error, "not-a-member");
  assert.strictEqual(auditCount(env), before + 2, "dan's download and the stranger's refusal, nothing else");
});

/* ---- the scrub ------------------------------------------------------- */

const SECRET = "test-builder-secret";
async function bigRelease(env, bytes) {
  const sha = await sha256Hex(bytes);
  const up = await (
    await worker.fetch(
      new Request("https://hub.example/publish/big/start", {
        method: "POST",
        headers: { authorization: "Bearer krs_carol", "content-type": "application/json" },
        body: JSON.stringify({ kind: "bundle", size: bytes.byteLength, sha256: sha, org: "acme" }),
      }),
      env,
    )
  ).json();
  const { etag } = await (
    await worker.fetch(
      new Request(`https://hub.example/publish/big/${up.id}/part/1`, {
        method: "PUT",
        headers: { "x-krate-upload": up.token, "content-length": String(bytes.byteLength) },
        body: bytes,
      }),
      env,
    )
  ).json();
  await worker.fetch(
    new Request(`https://hub.example/publish/big/${up.id}/finish`, {
      method: "POST",
      headers: { "x-krate-upload": up.token, "content-type": "application/json" },
      body: JSON.stringify({ parts: [{ part: 1, etag }] }),
    }),
    env,
  );
  const verdict = await (
    await worker.fetch(
      new Request(`https://hub.example/internal/big/${up.id}/verdict`, {
        method: "POST",
        headers: { "x-krate-builder": SECRET, "content-type": "application/json" },
        body: JSON.stringify({ ok: true, sha256: sha, judgement: judgeBundle(bytes).judgement }),
      }),
      env,
    )
  ).json();
  assert.strictEqual(verdict.status, "candidate", JSON.stringify(verdict));
  assert.strictEqual((await call(env, "bob", "POST", `/orgs/acme/candidates/${sha}/approve`)).status, 200);
  return { sha, blob: `big/${up.id}` };
}

const scrub = async (env, repair) => {
  const res = await worker.fetch(
    new Request("https://hub.example/admin/scrub", { method: repair ? "POST" : "GET", headers: { authorization: "Bearer gho_alice" } }),
    env,
  );
  assert.strictEqual(res.status, 200);
  return res.json();
};

test("the scrub counts an organisation's private bytes as owned, and its repair never touches them", async () => {
  const env = orgEnv({
    KRATE_ADMINS: "alice",
    KRATE_BUILDER_SECRET: SECRET,
    BIG_PUBLISHERS: "carol",
    BIG_PART_BYTES: String(5 * 1024 * 1024),
  });
  await acme(env);
  const x = await released(env, "candidate-v1.krate");
  const y = (await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v2.krate") })).body.digest;
  const big = await bigRelease(env, fixture("root-signed.krate"));
  // A genuine orphan beside them, so the scrub is shown still to find one.
  const stray = new Uint8Array([1, 2, 3]);
  const strayKey = await sha256Hex(stray);
  await env.BUNDLES.put(strayKey, stray, { sha256: strayKey });

  const private_ = [x, y, big.sha];
  const report = await scrub(env, false);
  const named = (p) => private_.some((d) => p.key.includes(d));
  assert.deepStrictEqual(report.problems.filter(named), [], JSON.stringify(report.problems));
  assert.strictEqual(report.problems.filter((p) => p.kind === "orphan-object" && named(p)).length, 0, "zero orphans for them");
  assert.ok(report.problems.some((p) => p.kind === "orphan-object" && p.key === strayKey), "a real orphan is still reported");
  assert.strictEqual(report.org_objects, 3, "the release, the waiting candidate and the big release");

  const repaired = await scrub(env, true);
  assert.deepStrictEqual(repaired.problems.filter(named), []);
  for (const key of [x, y]) assert.ok(await env.BUNDLES.head(key), `${key.slice(0, 12)} still stored`);
  assert.ok(await env.BUNDLES.head(big.blob), "the big release's bytes are still stored");
  assert.strictEqual(env._kv.get(`bigkey:${big.sha}`), big.blob, "and its address record");
  assert.ok(await env.BUNDLES.head(strayKey), "a scrub deletes no bytes at all, orphan or not");
  for (const digest of [x, big.sha]) {
    const r = await call(env, "dan", "GET", `/a/${digest}`);
    assert.strictEqual(r.status, 200, "the release still downloads after a repairing scrub");
  }
  assert.strictEqual((await call(env, "stranger", "GET", `/a/${x}`)).status, 403, "and is still private");
});
