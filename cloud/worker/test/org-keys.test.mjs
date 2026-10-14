/* The organisation keyring, hub side (F-316): devices registered under
 * principals, the key made once on an owner's device, passed device to
 * device for active members without any owner action, every grant in the
 * audit, stale on removal and rotated by a holder, the recovery envelope
 * for owners and admins. The hub stores only public keys and sealed
 * envelopes.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-keys.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { orgEnv, call, acme } from "./org-tools.mjs";
import { device, register, initKey, memberDevice, recovery, b64, hexOf } from "./org-keys-tools.mjs";
import { randomBytes } from "node:crypto";

const enc = encodeURIComponent;
const pair = (r) => [r.status, r.body && r.body.error];
const view = (env, who, dev) => call(env, who, "GET", `/orgs/acme/keys?device=${dev.id}`);
const auditOf = async (env) => (await call(env, "alice", "GET", "/orgs/acme/audit")).body;

async function world() {
  const env = orgEnv();
  await acme(env);
  return env;
}

test("a device registers under its own principal, by the id its key gives", async () => {
  const env = await world();
  const d = device();
  assert.strictEqual((await register(env, "carol", d)).status, 200);
  // The id must be the key's.
  const wrong = { ...device(), id: "0123456789abcdef" };
  assert.deepStrictEqual(pair(await register(env, "carol", wrong)), [400, "bad-request"]);
  // Nobody else takes Carol's device.
  assert.deepStrictEqual(pair(await register(env, "bob", d)), [403, "role-cannot"]);
  assert.deepStrictEqual(pair(await register(env, "stranger", device())), [403, "not-a-member"]);
  const v = await view(env, "carol", d);
  assert.deepStrictEqual([v.body.current, v.body.device, v.body.envelope], [null, { device_id: d.id, state: "active" }, null]);
});

test("the key is made once, by an owner, on a registered device, with a recovery envelope", async () => {
  const env = await world();
  const bobs = device();
  await register(env, "bob", bobs);
  const args = (dev) => ({ device_id: dev.id, public_key: hexOf(randomBytes(32)), envelope: b64(300), recovery: recovery() });
  assert.deepStrictEqual(pair(await call(env, "bob", "POST", "/orgs/acme/keys/init", { json: args(bobs) })), [403, "role-cannot"]);
  const alice = device();
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", "/orgs/acme/keys/init", { json: args(alice) })), [404, "device-unknown"]);
  await register(env, "alice", alice);
  const noRecovery = { ...args(alice), recovery: undefined };
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", "/orgs/acme/keys/init", { json: noRecovery })), [400, "bad-request"]);
  const weak = { ...args(alice), recovery: { ...recovery(), params: { alg: "argon2id", salt: "00".repeat(16), m_kib: 8, t: 1, p: 1 } } };
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", "/orgs/acme/keys/init", { json: weak })), [400, "bad-request"], "weak Argon2id refused");
  assert.strictEqual((await call(env, "alice", "POST", "/orgs/acme/keys/init", { json: args(alice) })).status, 201);
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", "/orgs/acme/keys/init", { json: args(alice) })), [409, "key-exists"]);
  const v = await view(env, "alice", alice);
  assert.strictEqual(v.body.current.version, 1);
  assert.strictEqual(v.body.envelope.version, 1);
  // Nothing private on the hub: only public halves and sealed blobs.
  const store = env.ORGS.object("acme");
  const columns = (t) => store.rows(`PRAGMA table_info(${t})`).map((c) => c.name);
  assert.deepStrictEqual(columns("key_versions"), ["version", "public_key", "state", "created_at", "created_by", "created_device", "stale_since"]);
  assert.deepStrictEqual(columns("key_envelopes"), ["device_id", "version", "blob", "granted_by", "granted_device", "at"]);
});

test("a new member's device gets the key from another member's device, with no owner action, and the audit names both", async () => {
  const env = await world();
  const alice = await initKey(env);
  // Carol (a developer) gets it from Alice's device...
  const carol = await memberDevice(env, "carol", alice);
  // ...and Dan (a recipient), who joins later, from Carol's: no owner involved.
  const dan = device();
  await register(env, "dan", dan, "Dan's PC");
  assert.strictEqual((await view(env, "dan", dan)).body.envelope, null, "waiting");
  const cv = await view(env, "carol", carol);
  assert.deepStrictEqual(cv.body.requests.map((r) => [r.device_id, r.principal]), [[dan.id, "github:dan"]]);
  assert.strictEqual(cv.body.requests[0].public_key, dan.pub);
  // Someone without the key is told of nobody.
  const bobs = device();
  await register(env, "bob", bobs);
  assert.deepStrictEqual((await view(env, "bob", bobs)).body.requests, []);
  // A device that does not hold the key cannot grant it.
  assert.deepStrictEqual(
    pair(await call(env, "bob", "POST", "/orgs/acme/keys/grants", { json: { device_id: dan.id, version: 1, blob: b64(300), by_device: bobs.id } })),
    [403, "key-not-held"],
  );
  // Nor can someone grant from a device that is not theirs.
  assert.deepStrictEqual(
    pair(await call(env, "bob", "POST", "/orgs/acme/keys/grants", { json: { device_id: dan.id, version: 1, blob: b64(300), by_device: carol.id } })),
    [403, "key-not-held"],
  );
  const g = await call(env, "carol", "POST", "/orgs/acme/keys/grants", { json: { device_id: dan.id, version: 1, blob: b64(300), by_device: carol.id } });
  assert.strictEqual(g.status, 200, JSON.stringify(g.body));
  const dv = await view(env, "dan", dan);
  assert.deepStrictEqual([dv.body.envelope.version, dv.body.envelope.granted_by, dv.body.envelope.granted_device], [1, "carol", carol.id]);
  // An envelope is only ever given to its own device's owner.
  assert.strictEqual((await call(env, "carol", "GET", `/orgs/acme/keys?device=${dan.id}`)).body.envelope, null);
  // The audit: who holds the key, from whom.
  const a = await auditOf(env);
  const grants = a.events.filter((e) => e.action === "key.grant").map((e) => [e.actor, e.target, e.reason]);
  assert.deepStrictEqual(grants, [
    ["github:alice", `device:${carol.id}`, `v1 to github:carol by ${alice.id}`],
    ["github:carol", `device:${dan.id}`, `v1 to github:dan by ${carol.id}`],
  ]);
  assert.strictEqual(a.chain_ok, true);
  // The dashboard's list: every device and what it holds.
  const devices = (await view(env, "alice", alice)).body.devices.map((d) => [d.display, d.holds]);
  assert.deepStrictEqual(devices, [["alice", 1], ["carol", 1], ["dan", 1], ["bob", null]]);
});

test("removing a member removes their devices' hold and makes the key stale; a holder rotates it to every remaining device", async () => {
  const env = await world();
  const alice = await initKey(env);
  const carol = await memberDevice(env, "carol", alice);
  const dan = await memberDevice(env, "dan", alice);
  assert.strictEqual((await call(env, "alice", "DELETE", `/orgs/acme/members/${enc("github:dan")}`)).status, 204);
  const v = await view(env, "carol", carol);
  assert.strictEqual(v.body.current.state, "stale");
  assert.deepStrictEqual(v.body.seal_to.map((d) => d.device_id).sort(), [alice.id, carol.id].sort(), "the removed member's device is not among them");
  assert.ok(v.body.recovery && v.body.recovery.public_key, "the recovery key, to re-seal for it");
  // The removed member reads nothing.
  assert.deepStrictEqual(pair(await view(env, "dan", dan)), [403, "removed"]);
  // A rotation must reach every device that held the key, and the recovery envelope.
  const next = hexOf(randomBytes(32));
  const short = { version: 2, public_key: next, by_device: carol.id, envelopes: { [carol.id]: b64(300) }, recovery_blob: b64(200) };
  assert.deepStrictEqual(pair(await call(env, "carol", "POST", "/orgs/acme/keys/rotate", { json: short })), [400, "bad-request"]);
  const noRecovery = { ...short, envelopes: { [carol.id]: b64(300), [alice.id]: b64(300) }, recovery_blob: undefined };
  assert.deepStrictEqual(pair(await call(env, "carol", "POST", "/orgs/acme/keys/rotate", { json: noRecovery })), [400, "bad-request"]);
  const full = { ...noRecovery, recovery_blob: b64(200), envelopes: { ...noRecovery.envelopes, [dan.id]: b64(300) } };
  const r = await call(env, "carol", "POST", "/orgs/acme/keys/rotate", { json: full });
  assert.strictEqual(r.status, 200, JSON.stringify(r.body));
  const after = await view(env, "alice", alice);
  assert.deepStrictEqual([after.body.current.version, after.body.current.state, after.body.envelope.version], [2, "current", 2]);
  // Dan's device got nothing, even though an envelope was offered for it.
  const store = env.ORGS.object("acme");
  assert.strictEqual(store.row("SELECT * FROM key_envelopes WHERE device_id = ?", dan.id), null);
  // Not stale any more: rotating again is refused.
  assert.strictEqual((await call(env, "carol", "POST", "/orgs/acme/keys/rotate", { json: { ...full, version: 3 } })).body.error, "key-rotated");
  const lines = (await auditOf(env)).events.map((e) => e.action);
  for (const action of ["key.device.remove", "key.stale", "key.rotate"]) assert.ok(lines.includes(action), action);
});

test("the recovery envelope is for owners and admins, its read is audited, and it restores only your own device", async () => {
  const env = await world();
  await initKey(env);
  assert.deepStrictEqual(pair(await call(env, "carol", "GET", "/orgs/acme/keys/recovery")), [403, "role-cannot"]);
  const rec = await call(env, "alice", "GET", "/orgs/acme/keys/recovery");
  assert.strictEqual(rec.status, 200);
  assert.strictEqual(rec.body.params.alg, "argon2id");
  // Alice's fresh laptop, restored from the code.
  const fresh = device();
  await register(env, "alice", fresh, "Alice's new laptop");
  const g = await call(env, "alice", "POST", "/orgs/acme/keys/grants", { json: { device_id: fresh.id, version: 1, blob: b64(300), by_device: "recovery" } });
  assert.strictEqual(g.status, 200, JSON.stringify(g.body));
  // Not someone else's device, and not by a developer.
  const carols = device();
  await register(env, "carol", carols);
  assert.deepStrictEqual(
    pair(await call(env, "alice", "POST", "/orgs/acme/keys/grants", { json: { device_id: carols.id, version: 1, blob: b64(300), by_device: "recovery" } })),
    [403, "role-cannot"],
  );
  assert.deepStrictEqual(
    pair(await call(env, "carol", "POST", "/orgs/acme/keys/grants", { json: { device_id: carols.id, version: 1, blob: b64(300), by_device: "recovery" } })),
    [403, "role-cannot"],
  );
  const lines = (await auditOf(env)).events.map((e) => `${e.action} ${e.reason || ""}`);
  assert.ok(lines.includes("key.recovery.read "), lines.join("\n"));
  assert.ok(lines.some((l) => l.startsWith("key.recover v1 to github:alice by recovery code")), lines.join("\n"));
});
