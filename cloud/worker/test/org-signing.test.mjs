/* Signing nobody has to think about (IC-958): the signing ring held by
 * owners' and admins' devices, publishers approved in the background,
 * withdrawal on removal, and solo release -- through the hub's real store,
 * with the real sealing and signing: docs/landing/kit/org-device.js (what a
 * browser that is an owner's device runs) over the validator module built
 * from crates/bundle and crates/orgkeys, with node's WebCrypto.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-signing.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join as joinPath } from "node:path";
import { orgEnv, call, tokenFor, worker } from "./org-tools.mjs";
import "../../../docs/landing/kit/org-device.js";

const here = dirname(fileURLToPath(import.meta.url));
const S = globalThis.KrateSigning;
const MODULE = readFileSync(joinPath(here, "..", "src", "validator.wasm"));
const BOUNCE = new Uint8Array(readFileSync(joinPath(here, "..", "..", "..", "evidence", "ported", "bounce.krate")));
const HUB = "https://hub.example";
const SEED = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const enc = encodeURIComponent;
const pair = (r) => [r.status, r.body && r.body.error];

// The library talks to the hub with fetch: here, the worker itself.
// Anything else (the GitHub stub) goes where it went before.
let current = null;
const before = globalThis.fetch;
// /validator.wasm is a static file the hub serves beside the Worker
// (wrangler.toml [assets]); here, the module itself.
globalThis.fetch = (url, init) =>
  String(url) === `${HUB}/validator.wasm`
    ? Promise.resolve(new Response(MODULE, { headers: { "content-type": "application/wasm" } }))
    : String(url).startsWith(`${HUB}/`)
      ? worker.fetch(new Request(url, init), current)
      : before(url, init);

async function world({ members = [] } = {}) {
  const env = orgEnv({ ORG_LEASE_KEY: SEED });
  current = env;
  const r = await call(env, "alice", "POST", "/orgs", { json: { slug: "acme", name: "Acme" } });
  assert.strictEqual(r.status, 201, JSON.stringify(r.body));
  const mod = await S.loadModule(MODULE);
  for (const [login, role] of members) await join(env, login, role);
  return { env, mod };
}

async function join(env, login, role) {
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: login, role } });
  assert.strictEqual(inv.status, 201, JSON.stringify(inv.body));
  const ok = await call(env, login, "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  assert.strictEqual(ok.status, 200, JSON.stringify(ok.body));
}

/// `who`'s browser (or computer): a device made by WebCrypto, kept in memory.
async function deviceOf(name) {
  return S.openDevice({ store: S.memoryStore(), name });
}

const as = (env, who, mod, device, extra = {}) => ({ hub: HUB, token: tokenFor(env, who), slug: "acme", mod, device, ...extra });
const auditOf = async (env) => (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events;
const actions = async (env) => (await auditOf(env)).map((e) => e.action);
const store = (env) => env.ORGS.object("acme").sql;

/// A release key as a developer's Krate makes it (WebCrypto Ed25519 here).
async function releaseKey() {
  const k = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const jwk = await crypto.subtle.exportKey("jwk", k.privateKey);
  const hex = (b64u) => Buffer.from(b64u, "base64url").toString("hex");
  return { seed: hex(jwk.d), public: hex(jwk.x) };
}

/// `who` sends `bytes` signed with their release key and slip.
async function sendDelegated(env, mod, who, key, delegation, signedAt = Math.floor(Date.now() / 1000)) {
  const signed = mod.sign({ org: "acme", signed_at: signedAt, seed: key.seed, public: key.public, delegation }, BOUNCE);
  return call(env, who, "POST", "/orgs/acme/candidates", { bytes: signed.bytes });
}

test("an owner alone sets up in the browser, signs as the organisation and releases alone, marked as such", async () => {
  const { env, mod } = await world();
  const alice = await deviceOf("Alice's browser");
  const made = await S.setUp(as(env, "alice", mod, alice));
  assert.match(made.recoveryCode, /^[0-9A-Z]{5}(-[0-9A-Z]{1,5}){5}$/);
  const org = (await call(env, "alice", "GET", "/orgs/acme")).body.org;
  assert.match(org.root_key, /^[0-9a-f]{64}$/, "the browser's signing key is the organisation's");
  assert.deepStrictEqual(
    [org.publishing.two_person, org.publishing.solo_release, org.publishing.signing],
    [false, true, true],
  );
  // Nothing private on the hub: public halves and envelopes only.
  const all = JSON.stringify(store(env).exec("SELECT * FROM sign_ring").toArray()) + JSON.stringify(store(env).exec("SELECT * FROM sign_envelopes").toArray());
  assert.ok(!all.includes(await alice.secret()), "the device's private key never left it");

  const steps = [];
  const out = await S.release(as(env, "alice", mod, alice, { bytes: BOUNCE, name: "Bounce", onStep: (s) => steps.push(s) }));
  assert.deepStrictEqual([out.state, out.solo], ["released", true], JSON.stringify(out));
  assert.deepStrictEqual(steps, ["Signing Bounce...", "Sending it...", "Releasing it..."]);
  assert.strictEqual(out.url, `${HUB}/a/${out.digest}`);
  const rel = (await call(env, "dan", "GET", "/orgs/acme/releases")).status;
  assert.strictEqual(rel, 403, "a stranger sees nothing");
  const releases = (await call(env, "alice", "GET", "/orgs/acme/releases")).body;
  assert.deepStrictEqual(
    releases.map((r) => [r.digest, r.solo, r.released_note, r.uploaded_by, r.approved_by]),
    [[out.digest, true, "released by its sender (no second approver yet)", "github:alice", "github:alice"]],
  );
  // The bytes the hub keeps are signed by the organisation and marked.
  const bytes = (await call(env, "alice", "GET", `/a/${out.digest}`)).bytes;
  const judged = mod.judge(bytes);
  assert.deepStrictEqual([judged.judgement.signature.state, judged.judgement.signature.public_key, judged.judgement.org_mark], ["valid", org.root_key, "acme"]);
  const audit = await auditOf(env);
  const solo = audit.find((e) => e.action === "candidate.approve" && e.target === `candidate:${out.digest}`);
  assert.deepStrictEqual([solo.actor, solo.result, solo.reason], ["github:alice", "allowed", "solo"]);
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme")).body.org.release_rule, "solo");
  for (const a of ["key.init", "root-key.register", "signing.init"]) assert.ok(audit.some((e) => e.action === a), a);
  assert.ok((await call(env, "alice", "GET", "/orgs/acme/audit")).body.chain_ok);
});

test("the two-person rule turns on by itself when a second person who may approve joins, and Settings can turn it on from the start", async () => {
  const { env, mod } = await world();
  const alice = await deviceOf("Alice's browser");
  await S.setUp(as(env, "alice", mod, alice));
  // A developer and an admin do not approve: still solo.
  await join(env, "carol", "developer");
  await join(env, "erin", "admin");
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme/settings/publishing")).body.solo_release, true);
  assert.ok(!(await actions(env)).includes("review.two-person.on"));
  await join(env, "bob", "reviewer");
  const s = (await call(env, "alice", "GET", "/orgs/acme/settings/publishing")).body;
  assert.deepStrictEqual([s.two_person, s.solo_release, s.approvers], [true, false, ["alice", "bob"]]);
  const on = (await auditOf(env)).filter((e) => e.action === "review.two-person.on");
  assert.deepStrictEqual(on.map((e) => e.reason), ["bob can approve now"], "said once");

  const out = await S.release(as(env, "alice", mod, alice, { bytes: BOUNCE, name: "Bounce" }));
  assert.deepStrictEqual([out.state, out.solo, out.approvers], ["waiting-for-approval", false, ["bob"]]);
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", `/orgs/acme/candidates/${out.digest}/approve`, { json: {} })), [403, "self-approval"]);
  const ok = await call(env, "bob", "POST", `/orgs/acme/candidates/${out.digest}/approve`, { json: {} });
  assert.strictEqual(ok.status, 200);
  assert.strictEqual(ok.body.release.solo, undefined);
  assert.deepStrictEqual((await call(env, "alice", "GET", "/orgs/acme/releases")).body.map((r) => r.solo), [false]);

  // From the start: only an owner turns it on, and then even a lone owner waits.
  const second = await world();
  const a2 = await deviceOf("Alice's browser");
  await S.setUp(as(second.env, "alice", second.mod, a2));
  await join(second.env, "erin", "admin");
  assert.deepStrictEqual(pair(await call(second.env, "erin", "PUT", "/orgs/acme/settings/publishing", { json: { two_person_from_start: true } })), [403, "role-cannot"]);
  assert.deepStrictEqual(pair(await call(second.env, "alice", "PUT", "/orgs/acme/settings/publishing", { json: { two_person_from_start: "yes" } })), [400, "bad-request"]);
  const set = await call(second.env, "alice", "PUT", "/orgs/acme/settings/publishing", { json: { two_person_from_start: true } });
  assert.deepStrictEqual([set.status, set.body.two_person, set.body.solo_release], [200, true, false]);
  const waits = await S.release(as(second.env, "alice", second.mod, a2, { bytes: BOUNCE }));
  assert.strictEqual(waits.state, "waiting-for-approval");
  assert.deepStrictEqual(pair(await call(second.env, "alice", "POST", `/orgs/acme/candidates/${waits.digest}/approve`, { json: {} })), [403, "self-approval"]);
  const said = (await auditOf(second.env)).filter((e) => ["settings.two-person", "review.two-person.on"].includes(e.action)).map((e) => e.reason);
  assert.deepStrictEqual(said, ["every release needs a second person", "turned on in Settings"]);
});

test("a developer's release key is approved in the background by an owner's device; the hub checks the slip; the door takes what it signs", async () => {
  const { env, mod } = await world({ members: [["bob", "reviewer"], ["carol", "developer"], ["dan", "recipient"]] });
  const alice = await deviceOf("Alice's browser");
  await S.setUp(as(env, "alice", mod, alice));
  const key = await releaseKey();
  // Only someone who may publish asks.
  assert.deepStrictEqual(pair(await call(env, "bob", "POST", "/orgs/acme/publishers", { json: { public_key: key.public } })), [403, "role-cannot"]);
  assert.deepStrictEqual(pair(await call(env, "carol", "POST", "/orgs/acme/publishers", { json: { public_key: "zz" } })), [400, "bad-request"]);
  const asked = await call(env, "carol", "POST", "/orgs/acme/publishers", { json: { public_key: key.public, name: "Carol's MacBook" } });
  assert.strictEqual(asked.status, 201, JSON.stringify(asked.body));
  assert.deepStrictEqual(
    [asked.body.state, asked.body.can_sign, asked.body.waiting_on, asked.body.delegation, asked.body.by_hand],
    ["waiting", ["alice"], "a-holder-device", null, false],
  );
  // Asking again is the same record; someone else's key is not theirs.
  assert.strictEqual((await call(env, "carol", "POST", "/orgs/acme/publishers", { json: { public_key: key.public } })).status, 200);
  await join(env, "frank", "developer");
  assert.deepStrictEqual(pair(await call(env, "frank", "POST", "/orgs/acme/publishers", { json: { public_key: key.public } })), [403, "role-cannot"]);
  assert.deepStrictEqual(pair(await call(env, "frank", "GET", `/orgs/acme/publishers/${key.public}`)), [404, "not-a-publisher"]);

  // Before any device signs, a forged or wrong slip is refused at the door.
  const ring = (await call(env, "alice", "GET", `/orgs/acme/signing?device=${(await mod.keys({ op: "device_id", public_key: alice.public_key })).device_id}`)).body;
  const aliceId = ring.device.device_id;
  const other = await deviceOf("x");
  const otherRing = mod.keys({ op: "ring_create", org: "acme", ...(await (async () => { const k = await releaseKey(); return { seed: k.seed, public: k.public }; })()), device_public: other.public_key });
  const wrongRoot = mod.keys({ op: "ring_delegate", org: "acme", envelope: otherRing.envelope, device_secret: await other.secret(), key: key.public, at: Math.floor(Date.now() / 1000) });
  assert.deepStrictEqual(
    pair(await call(env, "alice", "POST", `/orgs/acme/publishers/${key.public}/delegation`, { json: { delegation: wrongRoot.delegation, by_device: aliceId } })),
    [422, "slip-invalid"],
  );
  const forged = JSON.parse(JSON.stringify(wrongRoot.delegation));
  forged.delegation.root = ring.ring.public_key;
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", `/orgs/acme/publishers/${key.public}/delegation`, { json: { delegation: forged, by_device: aliceId } })), [422, "slip-invalid"]);
  const mine = mod.keys({ op: "ring_delegate", org: "acme", envelope: ring.envelope.blob, device_secret: await alice.secret(), key: (await releaseKey()).public, at: Math.floor(Date.now() / 1000) });
  assert.deepStrictEqual(
    pair(await call(env, "alice", "POST", `/orgs/acme/publishers/${key.public}/delegation`, { json: { delegation: mine.delegation, by_device: aliceId } })),
    [422, "slip-invalid"],
    "a slip for another key",
  );
  assert.deepStrictEqual(
    pair(await call(env, "carol", "POST", `/orgs/acme/publishers/${key.public}/delegation`, { json: { delegation: mine.delegation, by_device: aliceId } })),
    [403, "role-cannot"],
    "a developer does not sign for the organisation",
  );

  // Alice's device does its duties: Carol may publish, nobody clicked.
  const did = await S.duties(as(env, "alice", mod, alice));
  assert.deepStrictEqual(did.approved, ["carol"]);
  const signed = (await call(env, "carol", "GET", `/orgs/acme/publishers/${key.public}`)).body;
  assert.deepStrictEqual([signed.state, signed.signed_by, signed.signed_device, signed.waiting_on], ["signed", "github:alice", aliceId, null]);
  assert.strictEqual(signed.delegation.delegation.namespace, "acme/*");
  assert.strictEqual(signed.delegation.delegation.root, ring.ring.public_key);
  assert.ok(signed.expires - Math.floor(Date.now() / 1000) > 364 * 86400);
  assert.deepStrictEqual((await S.duties(as(env, "alice", mod, alice))).approved, [], "once");

  const up = await sendDelegated(env, mod, "carol", key, signed.delegation);
  assert.strictEqual(up.status, 201, JSON.stringify(up.body));
  assert.deepStrictEqual([up.body.state, up.body.solo_release, up.body.approvers], ["waiting-for-approval", false, ["alice", "bob"]]);
  assert.strictEqual((await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`, { json: {} })).status, 200);
  const audit = await auditOf(env);
  const d = audit.find((e) => e.action === "publisher.delegate" && e.result === "allowed");
  assert.ok(audit.filter((e) => e.action === "publisher.delegate" && e.result === "denied").length >= 3, "each refused slip is in the audit");
  assert.match(d.reason, new RegExp(`^automatic: carol may publish, signed by ${aliceId}, until \\d{4}-\\d\\d-\\d\\d$`));
  assert.ok(audit.some((e) => e.action === "publisher.request" && e.actor === "github:carol" && e.reason === "Carol's MacBook"));
  const list = (await call(env, "alice", "GET", "/orgs/acme/publishers")).body.publishers;
  assert.deepStrictEqual(list.map((p) => [p.display, p.state]), [["carol", "signed"]]);
  assert.deepStrictEqual((await call(env, "carol", "GET", "/orgs/acme/publishers")).body.me.state, "publisher");
});

test("approve new publishers by hand: one click by an owner or admin, then a device signs it", async () => {
  const { env, mod } = await world({ members: [["bob", "reviewer"], ["carol", "developer"]] });
  const alice = await deviceOf("Alice's browser");
  const { deviceId } = await S.setUp(as(env, "alice", mod, alice));
  assert.strictEqual((await call(env, "alice", "PUT", "/orgs/acme/settings/publishing", { json: { publishers_by_hand: true } })).body.publishers_by_hand, true);
  const key = await releaseKey();
  const asked = (await call(env, "carol", "POST", "/orgs/acme/publishers", { json: { public_key: key.public } })).body;
  assert.deepStrictEqual([asked.state, asked.by_hand, asked.waiting_on], ["waiting", true, "an-approval"]);
  assert.deepStrictEqual((await S.duties(as(env, "alice", mod, alice))).approved, [], "nothing signs before the click");
  const view = (await call(env, "alice", "GET", `/orgs/acme/signing?device=${deviceId}`)).body;
  const slip = mod.keys({ op: "ring_delegate", org: "acme", envelope: view.envelope.blob, device_secret: await alice.secret(), key: key.public, at: Math.floor(Date.now() / 1000) });
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", `/orgs/acme/publishers/${key.public}/delegation`, { json: { delegation: slip.delegation, by_device: deviceId } })), [409, "needs-approval"]);
  assert.deepStrictEqual(pair(await call(env, "bob", "POST", `/orgs/acme/publishers/${key.public}/approve`, { json: {} })), [403, "role-cannot"]);
  const after = await S.approvePublisher(as(env, "alice", mod, alice, { key: key.public }));
  assert.deepStrictEqual([after.state, after.approved_by], ["signed", "github:alice"]);
  const audit = await actions(env);
  assert.ok(audit.includes("settings.publishers") && audit.includes("publisher.approve") && audit.includes("publisher.delegate"));
});

test("removing a developer withdraws their release key: refused at the door at once, then signed as withdrawn by the next owner's device", async () => {
  const { env, mod } = await world({ members: [["bob", "reviewer"], ["carol", "developer"], ["frank", "developer"]] });
  const alice = await deviceOf("Alice's browser");
  await S.setUp(as(env, "alice", mod, alice));
  const key = await releaseKey();
  await call(env, "carol", "POST", "/orgs/acme/publishers", { json: { public_key: key.public } });
  await S.duties(as(env, "alice", mod, alice));
  const slip = (await call(env, "carol", "GET", `/orgs/acme/publishers/${key.public}`)).body.delegation;
  const before = Math.floor(Date.now() / 1000);
  const first = await sendDelegated(env, mod, "carol", key, slip, before);
  assert.strictEqual(first.status, 201);
  assert.strictEqual((await call(env, "bob", "POST", `/orgs/acme/candidates/${first.body.digest}/approve`, { json: {} })).status, 200);

  assert.strictEqual((await call(env, "alice", "DELETE", `/orgs/acme/members/${enc("github:carol")}`)).status, 204);
  assert.deepStrictEqual(pair(await sendDelegated(env, mod, "carol", key, slip)), [403, "removed"], "her next upload");
  // Bytes she signed, sent by someone still here: refused, before any withdrawal is signed.
  const carried = await sendDelegated(env, mod, "frank", key, slip);
  assert.deepStrictEqual(pair(carried), [422, "not-signed-for-this-org"]);
  assert.match(carried.body.message, /belonged to carol, who can no longer publish to acme/);
  const p = (await call(env, "alice", "GET", `/orgs/acme/publishers/${key.public}`)).body;
  assert.deepStrictEqual([p.state, p.closed_reason], ["revoke-due", "left the organisation"]);

  const did = await S.duties(as(env, "alice", mod, alice));
  assert.strictEqual(did.withdrawn, 1);
  assert.strictEqual((await call(env, "alice", "GET", `/orgs/acme/publishers/${key.public}`)).body.state, "revoked");
  const revoked = store(env).exec("SELECT key, compromised_from FROM revoked_keys").toArray();
  assert.deepStrictEqual(revoked.map((r) => r.key), [key.public]);
  assert.ok(revoked[0].compromised_from >= before, "what she released before she left stays released");
  assert.deepStrictEqual((await call(env, "alice", "GET", "/orgs/acme/releases")).body.length, 1);
  const audit = await auditOf(env);
  for (const a of ["publisher.revoke-due", "release-key.revoke"]) assert.ok(audit.some((e) => e.action === a), a);
  assert.match(audit.find((e) => e.action === "release-key.revoke").reason, /^automatic, by [0-9a-f]{16}: carol$/);
  assert.strictEqual((await S.duties(as(env, "alice", mod, alice))).withdrawn, 0, "once");
  // A forged withdrawal list is refused.
  const bad = await call(env, "alice", "POST", "/orgs/acme/signing/revocations", { json: { list: { list: { schema: "krate.revocations.v1", root: "00".repeat(32), issued_at: 1, revocations: [] }, signature: "00" }, by_device: "x" } });
  assert.deepStrictEqual(pair(bad), [422, "slip-invalid"]);
});

test("a SCIM deactivation withdraws publishing and ends the person's keyring devices, as a removal does", async () => {
  const { env, mod } = await world({ members: [["bob", "reviewer"]] });
  const alice = await deviceOf("Alice's browser");
  await S.setUp(as(env, "alice", mod, alice));
  const t = await call(env, "alice", "POST", "/orgs/acme/scim/tokens");
  assert.strictEqual(t.status, 201);
  const scim = async (method, path, payload) => {
    const res = await worker.fetch(new Request(`${HUB}/scim/v2/acme${path}`, { method, headers: { authorization: `Bearer ${t.body.token}`, "content-type": "application/scim+json" }, body: payload && JSON.stringify(payload) }), env);
    return { status: res.status, body: await res.json() };
  };
  const email = "erin@corp.example";
  const made = await scim("POST", "/Users", { schemas: ["urn:ietf:params:scim:schemas:core:2.0:User"], userName: email, active: true });
  assert.strictEqual(made.status, 201, JSON.stringify(made.body));
  assert.strictEqual((await call(env, "alice", "PUT", `/orgs/acme/members/${enc(`email:${email}`)}`, { json: { role: "developer" } })).status, 200);
  const erin = await deviceOf("Erin's laptop");
  await S.duties(as(env, email, mod, erin)); // registers her device
  await S.duties(as(env, "alice", mod, alice)); // passes her the keyring
  const key = await releaseKey();
  await call(env, email, "POST", "/orgs/acme/publishers", { json: { public_key: key.public } });
  await S.duties(as(env, "alice", mod, alice));
  assert.strictEqual((await call(env, email, "GET", `/orgs/acme/publishers/${key.public}`)).body.state, "signed");
  const off = await scim("PATCH", `/Users/${made.body.id}`, { schemas: ["urn:ietf:params:scim:api:messages:2.0:PatchOp"], Operations: [{ op: "replace", value: { active: false } }] });
  assert.strictEqual(off.status, 200, JSON.stringify(off.body));
  assert.strictEqual((await call(env, "alice", "GET", `/orgs/acme/publishers/${key.public}`)).body.state, "revoke-due");
  const devices = store(env).exec("SELECT state FROM key_devices WHERE principal = ?", `email:${email}`).toArray();
  assert.deepStrictEqual(devices.map((d) => d.state), ["removed"], "her keyring device ended too");
  assert.strictEqual(store(env).exec("SELECT state FROM key_versions ORDER BY version DESC LIMIT 1").toArray()[0].state, "stale");
  const audit = await auditOf(env);
  assert.ok(audit.some((e) => e.action === "publisher.revoke-due" && e.actor === "scim"));
  assert.ok(audit.some((e) => e.action === "key.device.remove" && e.actor === "scim"));
  assert.strictEqual((await S.duties(as(env, "alice", mod, alice))).withdrawn, 1);
});

test("the ring passes to a new admin's device in the background, never to a developer's, and leaves with them", async () => {
  const { env, mod } = await world({ members: [["carol", "developer"], ["erin", "admin"]] });
  const alice = await deviceOf("Alice's browser");
  await S.setUp(as(env, "alice", mod, alice));
  const erin = await deviceOf("Erin's PC");
  const carol = await deviceOf("Carol's MacBook");
  await S.duties(as(env, "erin", mod, erin));
  await S.duties(as(env, "carol", mod, carol));
  const did = await S.duties(as(env, "alice", mod, alice));
  assert.deepStrictEqual([did.ringGrants, did.keyGrants], [1, 2]);
  const erinId = mod.keys({ op: "device_id", public_key: erin.public_key }).device_id;
  const carolId = mod.keys({ op: "device_id", public_key: carol.public_key }).device_id;
  const ev = (await call(env, "erin", "GET", `/orgs/acme/signing?device=${erinId}`)).body;
  assert.ok(ev.envelope && ev.may_hold);
  assert.strictEqual(mod.keys({ op: "ring_open", org: "acme", envelope: ev.envelope.blob, device_secret: await erin.secret() }).public_key, ev.ring.public_key);
  const cv = (await call(env, "carol", "GET", `/orgs/acme/signing?device=${carolId}`)).body;
  assert.deepStrictEqual([cv.may_hold, cv.envelope, cv.recovery, cv.publisher_requests], [false, null, null, []]);
  // Erin's device does duties too, as a holder: a second owner device.
  const key = await releaseKey();
  await call(env, "carol", "POST", "/orgs/acme/publishers", { json: { public_key: key.public } });
  assert.deepStrictEqual((await S.duties(as(env, "erin", mod, erin))).approved, ["carol"]);
  // The holders are the people the waiting words name.
  assert.deepStrictEqual(ev.holders.map((h) => h.display).sort(), ["alice", "erin"]);
  // A developer cannot be handed the ring.
  const sneaky = mod.keys({ op: "ring_grant", org: "acme", envelope: ev.envelope.blob, device_secret: await erin.secret(), to_public: carol.public_key });
  assert.deepStrictEqual(pair(await call(env, "erin", "POST", "/orgs/acme/signing/grants", { json: { device_id: carolId, version: 1, blob: sneaky.blob, by_device: erinId } })), [404, "device-unknown"]);
  // Erin leaves: her envelope goes, and the audit says she had held it.
  assert.strictEqual((await call(env, "alice", "DELETE", `/orgs/acme/members/${enc("github:erin")}`)).status, 204);
  assert.deepStrictEqual(store(env).exec("SELECT device_id FROM sign_envelopes WHERE device_id = ?", erinId).toArray(), []);
  assert.ok((await auditOf(env)).some((e) => e.action === "signing.device.remove" && e.target === `device:${erinId}`));
  assert.ok((await auditOf(env)).some((e) => e.action === "signing.grant" && e.reason.startsWith("v1 to erin by ")));
});

test("the signing key is made once, by an owner, on a registered device; a key registered by hand is kept", async () => {
  const { env, mod } = await world({ members: [["bob", "reviewer"]] });
  const bob = await deviceOf("Bob");
  const k = await releaseKey();
  const ringOf = async (device) => mod.keys({ op: "ring_create", org: "acme", seed: k.seed, public: k.public, device_public: device.public_key });
  const idOf = (device) => mod.keys({ op: "device_id", public_key: device.public_key }).device_id;
  await S.duties(as(env, "bob", mod, bob));
  const rb = await ringOf(bob);
  assert.deepStrictEqual(pair(await call(env, "bob", "POST", "/orgs/acme/signing/init", { json: { device_id: idOf(bob), version: 1, public_key: rb.public_key, envelope: rb.envelope } })), [403, "role-cannot"]);
  const alice = await deviceOf("Alice");
  const ra = await ringOf(alice);
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", "/orgs/acme/signing/init", { json: { device_id: idOf(alice), version: 1, public_key: ra.public_key, envelope: ra.envelope } })), [404, "device-unknown"]);
  // The organisation's key was registered by hand with another key.
  const other = await releaseKey();
  assert.strictEqual((await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: other.public } })).status, 200);
  await S.duties(as(env, "alice", mod, alice));
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", "/orgs/acme/signing/init", { json: { device_id: idOf(alice), version: 1, public_key: ra.public_key, envelope: ra.envelope } })), [409, "signing-key-exists"]);
  // The same key as the one registered by hand joins the ring.
  assert.strictEqual((await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: k.public } })).status, 200);
  const made = await call(env, "alice", "POST", "/orgs/acme/signing/init", { json: { device_id: idOf(alice), version: 1, public_key: ra.public_key, envelope: ra.envelope } });
  assert.strictEqual(made.status, 201, JSON.stringify(made.body));
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", "/orgs/acme/signing/init", { json: { device_id: idOf(alice), version: 1, public_key: ra.public_key, envelope: ra.envelope } })), [409, "signing-key-exists"]);
  // Replaced by hand later: the ring no longer signs for the organisation.
  assert.strictEqual((await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: other.public } })).status, 200);
  const v = (await call(env, "alice", "GET", `/orgs/acme/signing?device=${idOf(alice)}`)).body;
  assert.deepStrictEqual([v.ring.current, v.settings.signing], [false, false]);
});

test("a browser whose data was cleared gets the organisation's keys back from the recovery code, and signs again", async () => {
  const { env, mod } = await world();
  const first = await deviceOf("Alice's browser");
  const { recoveryCode } = await S.setUp(as(env, "alice", mod, first));
  const fresh = await deviceOf("Alice's browser, again");
  await assert.rejects(S.release(as(env, "alice", mod, fresh, { bytes: BOUNCE })), (e) => e.reason === "signing-key-not-held");
  await assert.rejects(S.restore(as(env, "alice", mod, fresh, { code: "0000000000000000000000000A" })), /recovery code/);
  const back = await S.restore(as(env, "alice", mod, fresh, { code: recoveryCode.toLowerCase() }));
  assert.deepStrictEqual(back, { keyring: true, signing: true });
  const out = await S.release(as(env, "alice", mod, fresh, { bytes: BOUNCE }));
  assert.strictEqual(out.state, "released");
  const audit = await actions(env);
  assert.ok(audit.includes("signing.recovery.read") && audit.includes("signing.recover") && audit.includes("key.recover"));
});

test("the dashboard's shapes: publishers by role, approve by principal, the two switches, device kinds, and KrateOrgDevice", async () => {
  const { env, mod } = await world({ members: [["carol", "developer"], ["erin", "admin"]] });
  const D = globalThis.KrateOrgDevice;
  const alicesBrowser = S.memoryStore();
  D.configure({ hub: HUB, token: tokenFor(env, "alice"), store: alicesBrowser });
  const first = await D.ready("acme", { name: "Alice's Chrome" });
  assert.strictEqual(first.made, true);
  assert.match(first.recoveryCode, /^[0-9A-Z-]{31}$/);
  assert.deepStrictEqual(await D.ready("acme"), { made: false, holds: true, recoveryCode: null }, "ready again: nothing new");
  const signed = await D.sign("acme", BOUNCE);
  const j = mod.judge(signed).judgement;
  assert.deepStrictEqual([j.signature.state, j.org_mark], ["valid", "acme"]);
  // A developer's browser is not a holder.
  D.configure({ hub: HUB, token: tokenFor(env, "carol"), store: S.memoryStore() });
  await assert.rejects(D.ready("acme"), (e) => e.reason === "not-holder");
  await assert.rejects(D.sign("acme", BOUNCE), (e) => e.reason === "not-holder");
  // An admin's browser registers; it holds once a holder passes it on.
  D.configure({ hub: HUB, token: tokenFor(env, "erin"), store: S.memoryStore() });
  assert.deepStrictEqual(await D.ready("acme"), { made: false, holds: false, recoveryCode: null });
  D.configure({ hub: HUB, token: tokenFor(env, "alice"), store: alicesBrowser });
  const keys = (await call(env, "alice", "GET", "/orgs/acme/keys?device=x")).body.devices;
  assert.deepStrictEqual([...new Set(keys.map((d) => d.kind))], ["browser"]);

  const r1 = await call(env, "alice", "PUT", "/orgs/acme/publishers/by-hand", { json: { by_hand: true }, key: "k1" });
  assert.strictEqual(r1.status, 200);
  const k = await releaseKey();
  await call(env, "carol", "POST", "/orgs/acme/publishers", { json: { public_key: k.public } });
  const owners = (await call(env, "alice", "GET", "/orgs/acme/publishers")).body;
  assert.deepStrictEqual([owners.by_hand, owners.waiting.map((w) => w.principal), owners.approvers], [true, ["github:carol"], ["github:alice"]]);
  assert.ok(Number.isInteger(owners.waiting[0].asked_at) && owners.waiting[0].approved_by === undefined);
  const mine = (await call(env, "carol", "GET", "/orgs/acme/publishers")).body;
  assert.deepStrictEqual([mine.by_hand, mine.me.state, mine.waiting], [true, "waiting", undefined]);
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", `/orgs/acme/publishers/${enc("github:dan")}/approve`, { json: {}, key: "k2" })), [404, "not-a-publisher"]);
  const ok = await call(env, "alice", "POST", `/orgs/acme/publishers/${enc("github:carol")}/approve`, { json: {}, key: "k3" });
  assert.deepStrictEqual([ok.status, ok.body.state, ok.body.approved_by], [200, "approved", "github:alice"]);
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme/publishers")).body.waiting[0].approved_by, "github:alice");
  await D.ready("acme"); // this browser's share signs it
  assert.strictEqual((await call(env, "carol", "GET", "/orgs/acme/publishers")).body.me.state, "publisher");

  const rule = await call(env, "alice", "PUT", "/orgs/acme/release-rule", { json: { two_person: true }, key: "k4" });
  assert.strictEqual(rule.status, 200);
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme")).body.org.release_rule, "two-person");
  assert.deepStrictEqual(pair(await call(env, "erin", "PUT", "/orgs/acme/release-rule", { json: { two_person: false }, key: "k5" })), [403, "role-cannot"]);
});
