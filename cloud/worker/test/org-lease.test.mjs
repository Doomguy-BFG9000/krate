/* Offline access, hub side (IC-948, Addendum 3 G, Addendum 4 G1-G2).
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-lease.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { orgEnv, call, acme, fixture } from "./org-tools.mjs";
import {
  LEASE_CLASSES,
  LEASE_MAX_SECONDS,
  leaseSigningBytes,
  signLease,
  verifyLease,
} from "../src/org-lease.js";

// RFC 8032's first Ed25519 test key: its public half is known, so the
// key the hub derives from the seed can be checked against the RFC.
const SEED = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const RFC_PUBLIC = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
const DAY = 86400;

async function released(extra = {}) {
  const env = orgEnv({ ORG_LEASE_KEY: SEED, ...extra });
  await acme(env);
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  assert.strictEqual(up.status, 201, JSON.stringify(up.body));
  assert.strictEqual((await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`)).status, 200);
  return { env, digest: up.body.digest };
}

const audit = async (env) => (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events;

test("/lease-key serves the public half of ORG_LEASE_KEY", async () => {
  const env = orgEnv({ ORG_LEASE_KEY: SEED });
  const r = await call(env, null, "GET", "/lease-key");
  assert.strictEqual(r.status, 200);
  assert.strictEqual(r.body.alg, "Ed25519");
  assert.strictEqual(r.body.format, "krate-lease-v1");
  assert.strictEqual(r.body.public_key, RFC_PUBLIC);
  assert.match(r.body.key_id, /^[0-9a-f]{16}$/);
  const off = await call(orgEnv(), null, "GET", "/lease-key");
  assert.deepStrictEqual([off.status, off.body.error], [503, "leases-off"]);
});

test("a member and a recipient each get a signed lease that verifies against /lease-key", async () => {
  const { env, digest } = await released();
  const key = (await call(env, null, "GET", "/lease-key")).body;
  for (const [who, principal] of [["carol", "github:carol"], ["dan", "github:dan"], ["alice", "github:alice"]]) {
    const before = Math.floor(Date.now() / 1000);
    const r = await call(env, who, "GET", `/orgs/acme/leases/${digest}`);
    assert.strictEqual(r.status, 200, JSON.stringify(r.body));
    const { lease } = r.body;
    assert.deepStrictEqual(Object.keys(lease).sort(), ["class", "digest", "expires_at", "issued_at", "lease_id", "org", "principal", "v"]);
    assert.deepStrictEqual([lease.v, lease.org, lease.digest, lease.principal, lease.class], [1, "acme", digest, principal, "days-7"]);
    assert.ok(lease.issued_at >= before && lease.issued_at <= before + 2);
    assert.strictEqual(lease.expires_at - lease.issued_at, 7 * DAY);
    assert.match(lease.lease_id, /^[0-9a-f]{32}$/);
    assert.deepStrictEqual([r.body.alg, r.body.key_id], ["Ed25519", key.key_id]);
    assert.match(r.body.signature, /^[0-9a-f]{128}$/);
    assert.strictEqual(await verifyLease(r.body, key.public_key), true, who);
    // One changed byte and it does not.
    assert.strictEqual(await verifyLease({ ...r.body, lease: { ...lease, expires_at: lease.expires_at + 1 } }, key.public_key), false);
    assert.strictEqual(await verifyLease({ ...r.body, lease: { ...lease, principal: "github:mallory" } }, key.public_key), false);
  }
  // Audited as offline access issued, until a date.
  const issued = (await audit(env)).filter((e) => e.action === "lease.issue" && e.result === "allowed");
  assert.strictEqual(issued.length, 3);
  assert.match(issued[0].reason, /^days-7 until \d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/);
  assert.deepStrictEqual(issued.map((e) => e.actor), ["github:carol", "github:dan", "github:alice"]);
});

test("refused: a removed member, a stranger, no session, and a digest that is not a release", async () => {
  const { env, digest } = await released();
  assert.strictEqual((await call(env, "alice", "DELETE", `/orgs/acme/members/${encodeURIComponent("github:dan")}`)).status, 204);
  const removed = await call(env, "dan", "GET", `/orgs/acme/leases/${digest}`);
  assert.deepStrictEqual([removed.status, removed.body.error], [403, "removed"]);
  const stranger = await call(env, "stranger", "GET", `/orgs/acme/leases/${digest}`);
  assert.deepStrictEqual([stranger.status, stranger.body.error], [403, "not-a-member"]);
  const nobody = await call(env, null, "GET", `/orgs/acme/leases/${digest}`);
  assert.deepStrictEqual([nobody.status, nobody.body.error], [401, "not-signed-in"]);
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v2.krate") });
  const waiting = await call(env, "carol", "GET", `/orgs/acme/leases/${up.body.digest}`);
  assert.deepStrictEqual([waiting.status, waiting.body.error], [404, "digest-mismatch"]);
  assert.strictEqual((await call(env, "carol", "GET", "/orgs/acme/leases/nothex")).body.error, "digest-mismatch");
  const events = await audit(env);
  assert.ok(events.some((e) => e.action === "lease.issue" && e.actor === "github:dan" && e.reason === "removed"));
  assert.ok(events.some((e) => e.action === "lease.issue" && e.actor === "github:stranger" && e.reason === "not-a-member"));
  assert.ok(!events.some((e) => e.action === "lease.issue" && e.result === "allowed"), "no lease was issued");
  assert.strictEqual(env.ORGS.object("acme").ctx.storage.db.prepare("SELECT COUNT(*) AS n FROM leases").get().n, 0);
});

test("a removal racing an issue: either the lease came first, or it is refused -- never a lease after the removal", async () => {
  let leaseFirst = 0;
  let removalFirst = 0;
  for (let round = 0; round < 24; round++) {
    const { env, digest } = await released();
    const remove = () => call(env, "alice", "DELETE", `/orgs/acme/members/${encodeURIComponent("github:dan")}`);
    const lease = () => call(env, "dan", "GET", `/orgs/acme/leases/${digest}`);
    // The removal starts `round` turns of the event loop after the issue,
    // so the two meet the store in every order the loop allows.
    const later = async (n, f) => {
      for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r));
      return f();
    };
    const [a, b] = await Promise.all([lease(), later(round, remove)]);
    assert.strictEqual(b.status, 204);
    const events = await audit(env);
    const removedAt = events.find((e) => e.action === "member.remove" && e.result === "allowed").seq;
    const issued = events.filter((e) => e.action === "lease.issue" && e.result === "allowed");
    if (a.status === 200) {
      leaseFirst += 1;
      assert.strictEqual(issued.length, 1);
      assert.ok(issued[0].seq < removedAt, "the lease was issued before the removal committed");
    } else {
      removalFirst += 1;
      assert.strictEqual(a.body.error, "removed");
      assert.strictEqual(issued.length, 0);
    }
    // And after it, always refused.
    assert.strictEqual((await lease()).body.error, "removed");
  }
  assert.strictEqual(leaseFirst + removalFirst, 24);
  // Both orders really happened, or this proved nothing.
  assert.ok(leaseFirst > 0 && removalFirst > 0, `lease first ${leaseFirst}, removal first ${removalFirst}`);
});

test("an owner sets the class per app name; the default is days-7; nothing exceeds 30 days", async () => {
  const { env, digest } = await released();
  // The table itself: three classes, none past the bound.
  assert.deepStrictEqual(Object.keys(LEASE_CLASSES).sort(), ["days-30", "days-7", "online"]);
  assert.strictEqual(LEASE_MAX_SECONDS, 30 * DAY);
  for (const seconds of Object.values(LEASE_CLASSES)) assert.ok(seconds <= LEASE_MAX_SECONDS);
  // G1: the default on the organisation, the class on each release.
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme")).body.org.offline_default, "days-7");
  assert.strictEqual((await call(env, "dan", "GET", "/orgs/acme/releases")).body[0].offline, "days-7");
  // No other class can be chosen.
  for (const klass of ["days-60", "days-31", "days-365", "forever", 30, "DAYS-7", null]) {
    const r = await call(env, "alice", "PUT", "/orgs/acme/apps/Bounce/offline", { json: { class: klass } });
    assert.deepStrictEqual([r.status, r.body.error], [400, "bad-request"], String(klass));
    const d = await call(env, "alice", "PUT", "/orgs/acme/offline-default", { json: { class: klass } });
    assert.deepStrictEqual([d.status, d.body.error], [400, "bad-request"], String(klass));
  }
  // Only an owner chooses (R25).
  for (const who of ["bob", "carol", "dan"]) {
    assert.strictEqual((await call(env, who, "PUT", "/orgs/acme/apps/Bounce/offline", { json: { class: "days-30" } })).body.error, "role-cannot", who);
    assert.strictEqual((await call(env, who, "PUT", "/orgs/acme/offline-default", { json: { class: "days-30" } })).body.error, "role-cannot", who);
  }
  // days-30 for this app: 30 days exactly.
  const set = await call(env, "alice", "PUT", "/orgs/acme/apps/bounce/offline", { json: { class: "days-30" } });
  assert.deepStrictEqual([set.status, set.body], [200, { name: "bounce", class: "days-30" }]);
  assert.strictEqual((await call(env, "dan", "GET", "/orgs/acme/releases")).body[0].offline, "days-30", "matched by name, any case");
  const long = await call(env, "dan", "GET", `/orgs/acme/leases/${digest}`);
  assert.strictEqual(long.body.lease.expires_at - long.body.lease.issued_at, 30 * DAY);
  // G2: the private release's JSON says the terms a lease issued now gets.
  const look = await call(env, "dan", "GET", `/a/${digest}`, { headers: { accept: "application/json" } });
  assert.strictEqual(look.body.offline.class, "days-30");
  assert.ok(Math.abs(look.body.offline.until - (Math.floor(Date.now() / 1000) + 30 * DAY)) < 5);
  // online: no lease at all, and until is null.
  await call(env, "alice", "PUT", "/orgs/acme/apps/Bounce/offline", { json: { class: "online" } });
  const online = await call(env, "dan", "GET", `/orgs/acme/leases/${digest}`);
  assert.deepStrictEqual([online.status, online.body], [200, { class: "online", lease: null }]);
  assert.deepStrictEqual((await call(env, "dan", "GET", `/a/${digest}`, { headers: { accept: "application/json" } })).body.offline, { class: "online", until: null });
  // The organisation default covers apps nobody chose for.
  const dflt = await call(env, "alice", "PUT", "/orgs/acme/offline-default", { json: { class: "days-30" } });
  assert.deepStrictEqual(dflt.body, { offline_default: "days-30" });
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme")).body.org.offline_default, "days-30");
  const events = await audit(env);
  assert.ok(events.some((e) => e.action === "offline.set" && e.reason === "default->days-30"));
  assert.ok(events.some((e) => e.action === "offline.default" && e.reason === "days-7->days-30"));
});

test("the signer refuses a lease outside the bound, whatever produced it", async () => {
  const env = { ORG_LEASE_KEY: SEED };
  const now = 1_800_000_000;
  const ok = { v: 1, org: "acme", digest: "a".repeat(64), principal: "github:dan", class: "days-30", issued_at: now, expires_at: now + 30 * DAY, lease_id: "b".repeat(32) };
  assert.ok(await signLease(env, ok));
  for (const bad of [
    { ...ok, expires_at: now + 31 * DAY },
    { ...ok, class: "days-60", expires_at: now + 60 * DAY },
    { ...ok, class: "days-7", expires_at: now + 30 * DAY },
    { ...ok, class: "online", expires_at: now },
    { ...ok, v: 2 },
  ]) {
    await assert.rejects(() => signLease(env, bad), /outside the bound/, JSON.stringify(bad));
  }
});

test("a lease asked for again within the hour is the same lease, and costs no audit event", async () => {
  const { env, digest } = await released();
  const a = await call(env, "dan", "GET", `/orgs/acme/leases/${digest}`);
  const b = await call(env, "dan", "GET", `/orgs/acme/leases/${digest}`);
  assert.deepStrictEqual(a.body, b.body);
  assert.strictEqual((await audit(env)).filter((e) => e.action === "lease.issue").length, 1);
});

test("no ORG_LEASE_KEY: refused before anything is recorded", async () => {
  const { env, digest } = await released({ ORG_LEASE_KEY: "" });
  const r = await call(env, "dan", "GET", `/orgs/acme/leases/${digest}`);
  assert.deepStrictEqual([r.status, r.body.error], [503, "leases-off"]);
  assert.strictEqual(env.ORGS.object("acme").ctx.storage.db.prepare("SELECT COUNT(*) AS n FROM leases").get().n, 0);
});

test("the signing bytes are the canonical form in the contract's appendix", async () => {
  const lease = {
    v: 1,
    org: "acme",
    digest: "0".repeat(63) + "1",
    principal: "email:erin@corp.example",
    class: "days-7",
    issued_at: 1800000000,
    expires_at: 1800604800,
    lease_id: "00112233445566778899aabbccddeeff",
  };
  const text = new TextDecoder().decode(leaseSigningBytes(lease));
  assert.strictEqual(
    text,
    'krate-lease-v1\n{"class":"days-7","digest":"0000000000000000000000000000000000000000000000000000000000000001","expires_at":1800604800,"issued_at":1800000000,"lease_id":"00112233445566778899aabbccddeeff","org":"acme","principal":"email:erin@corp.example","v":1}',
  );
  // Ed25519 is deterministic: this signature is the test vector the CLI's
  // Rust check is held to (RFC 8032 key 1).
  const signed = await signLease({ ORG_LEASE_KEY: SEED }, lease);
  assert.strictEqual(signed.signature, VECTOR_SIGNATURE);
  assert.strictEqual(await verifyLease(signed, RFC_PUBLIC), true);
});

// Checked against a second implementation (Python's cryptography package)
// when it was written.
const VECTOR_SIGNATURE =
  "bd8f037118eb8edab4d6f1c55032dda0cd5e27f358e1772cc34e7a642f9a868e727613baa8eaa690241403eed28d989a58a016eb13ec80190c8fc92979a38904";
