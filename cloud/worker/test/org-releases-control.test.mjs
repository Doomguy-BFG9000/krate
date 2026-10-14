/* Release control and groups, hub side (IC-949, FLEET-CONTRACT.md R, proof
 * item 7): withdraw, the current release per app id and its rollback,
 * groups, and a release's audience -- each in the role matrix, each
 * audited, each refusal a stable reason.
 *
 * The bundles are the real signed fixtures (three versions of one app,
 * dev.krate.bounce: 1.0.0, 1.0.1, 1.0.2), judged by the real validator.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-releases-control.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { orgEnv, call, acme, fixture, sqlStorage } from "./org-tools.mjs";
import { sha256Hex } from "./r2-mock.mjs";
import { Org } from "../src/index.js";

const SEED = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const APP = "dev.krate.bounce";
const V1 = "candidate-v1.krate";
const V2 = "candidate-v2.krate";
const V3 = "root-signed.krate";
const enc = encodeURIComponent;

async function release(env, name) {
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(name) });
  assert.strictEqual(up.status, 201, JSON.stringify(up.body));
  const ok = await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`);
  assert.strictEqual(ok.status, 200, JSON.stringify(ok.body));
  return up.body.digest;
}

/* acme as acme() makes it, plus erin as an admin. */
async function world(extra = {}) {
  const env = orgEnv({ ORG_LEASE_KEY: SEED, ...extra });
  await acme(env);
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: "erin", role: "admin" } });
  assert.strictEqual((await call(env, "erin", "POST", `/orgs/acme/invites/${inv.body.code}/accept`)).status, 200);
  return env;
}

const events = async (env) => (await call(env, "alice", "GET", "/orgs/acme/audit")).body;
const pair = (r) => [r.status, r.body && r.body.error];
const current = (env, who = "alice") => call(env, who, "GET", `/orgs/acme/apps/${enc(APP)}/current`, { headers: { accept: "application/json" } });
const look = (env, who, digest) => call(env, who, "GET", `/a/${digest}`, { headers: { accept: "application/json" } });

/* ---- withdraw ---------------------------------------------------------- */

test("withdraw needs a reason and the authority; then downloads, check-ins and lease renewals are refused at once", async () => {
  const env = await world();
  const d1 = await release(env, V1);
  const d2 = await release(env, V2);
  // dan holds offline access to 1.0.1 before it is withdrawn.
  const lease = await call(env, "dan", "GET", `/orgs/acme/leases/${d2}`);
  assert.strictEqual(lease.status, 200, JSON.stringify(lease.body));

  // The reason is required, shown to people, and bounded.
  for (const reason of [undefined, "", "   ", "x".repeat(301)]) {
    const r = await call(env, "alice", "POST", `/orgs/acme/releases/${d2}/withdraw`, { json: { reason } });
    assert.deepStrictEqual(pair(r), [400, "reason-required"], String(reason));
  }
  // The matrix: owner, admin and reviewer withdraw; developer and recipient do not.
  for (const who of ["carol", "dan"]) {
    const r = await call(env, who, "POST", `/orgs/acme/releases/${d2}/withdraw`, { json: { reason: "bad build" } });
    assert.deepStrictEqual(pair(r), [403, "role-cannot"], who);
  }
  const stranger = await call(env, "stranger", "POST", `/orgs/acme/releases/${d2}/withdraw`, { json: { reason: "x" } });
  assert.deepStrictEqual(pair(stranger), [403, "not-a-member"]);

  // Before confirming, the dashboard reads what offline access is out.
  const apps = await call(env, "alice", "GET", "/orgs/acme/apps");
  const r2 = apps.body.apps[0].releases.find((r) => r.digest === d2);
  assert.strictEqual(r2.leases.active, 1);
  assert.strictEqual(r2.leases.until, lease.body.lease.expires_at);
  // A recipient does not see others' offline access.
  assert.strictEqual((await call(env, "dan", "GET", "/orgs/acme/apps")).body.apps[0].releases[0].leases, undefined);

  // A reviewer withdraws it.
  const w = await call(env, "bob", "POST", `/orgs/acme/releases/${d2}/withdraw`, { json: { reason: "crashes on open" } });
  assert.strictEqual(w.status, 200, JSON.stringify(w.body));
  assert.strictEqual(w.body.release.state, "withdrawn");
  assert.deepStrictEqual(w.body.current, { app_id: APP, digest: d1 }, "withdrawing the current one makes the newest remaining current");
  assert.deepStrictEqual(w.body.leases, { active: 1, until: lease.body.lease.expires_at }, "the issued lease holds until it runs out");

  // At once: the download, the check-in (the JSON look) and the renewal.
  const dl = await call(env, "dan", "GET", `/a/${d2}`);
  assert.deepStrictEqual(pair(dl), [410, "release-withdrawn"]);
  assert.match(dl.body.message, /Acme withdrew Bounce 1\.0\.1 on \d{4}-\d\d-\d\d: "crashes on open"/);
  assert.deepStrictEqual(pair(await look(env, "dan", d2)), [410, "release-withdrawn"]);
  assert.deepStrictEqual(pair(await call(env, "dan", "GET", `/orgs/acme/leases/${d2}`)), [410, "release-withdrawn"]);
  // Everyone, owners too: the bytes are gone, not forbidden to some.
  assert.deepStrictEqual(pair(await call(env, "alice", "GET", `/a/${d2}`)), [410, "release-withdrawn"]);
  // And a stranger still learns nothing about the digest.
  assert.deepStrictEqual(pair(await call(env, "stranger", "GET", `/a/${d2}`)), [403, "not-a-member"]);
  // The other version still opens.
  assert.strictEqual((await call(env, "dan", "GET", `/a/${d1}`)).status, 200);
  // The lease that was issued is still on record: offline it holds.
  const held = env.ORGS.object("acme").ctx.storage.db.prepare("SELECT COUNT(*) AS n FROM leases WHERE digest = ?").get(d2).n;
  assert.strictEqual(held, 1);

  // Not undone: withdrawing again, and approving again, are refused.
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", `/orgs/acme/releases/${d2}/withdraw`, { json: { reason: "again" } })), [410, "release-withdrawn"]);
  const again = await call(env, "bob", "POST", `/orgs/acme/candidates/${d2}/approve`);
  assert.deepStrictEqual(pair(again), [410, "release-withdrawn"]);
  assert.match(again.body.message, /send a new version/);
  // The same bytes sent again stay what they are: withdrawn.
  const resent = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(V2) });
  assert.strictEqual(resent.body.state, "withdrawn");
  assert.deepStrictEqual(pair(await call(env, "dan", "GET", `/a/${d2}`)), [410, "release-withdrawn"]);

  // A waiting candidate is not a release to withdraw.
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(V3) });
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", `/orgs/acme/releases/${up.body.digest}/withdraw`, { json: { reason: "x" } })), [404, "digest-mismatch"]);

  // The releases list leaves it out; the history keeps it, with its reason.
  const list = (await call(env, "dan", "GET", "/orgs/acme/releases")).body;
  assert.deepStrictEqual(list.map((r) => r.digest), [d1]);
  const hist = (await call(env, "dan", "GET", "/orgs/acme/apps")).body.apps[0];
  const gone = hist.releases.find((r) => r.digest === d2);
  assert.deepStrictEqual([gone.state, gone.withdraw_reason, gone.withdrawn_by], ["withdrawn", "crashes on open", "github:bob"]);

  // Audited, and the chain holds.
  const a = await events(env);
  assert.strictEqual(a.chain_ok, true);
  const did = a.events.filter((e) => e.result === "allowed").map((e) => [e.action, e.target, e.reason]);
  assert.ok(did.some(([act, t, r]) => act === "release.withdraw" && t === `release:${d2}` && r === "crashes on open"));
  assert.ok(did.some(([act, t, r]) => act === "release.current" && t === `app:${APP}` && r === `${d1} after withdrawing ${d2}`));
  const refused = a.events.filter((e) => e.result === "denied").map((e) => `${e.action} ${e.actor} ${e.reason}`);
  for (const want of [
    "release.withdraw github:carol role-cannot",
    "release.withdraw github:alice reason-required",
    "release.download github:dan release-withdrawn",
    "lease.issue github:dan release-withdrawn",
    "candidate.approve github:bob release-withdrawn",
  ]) {
    assert.ok(refused.includes(want), want);
  }
});

/* ---- current release and rollback ------------------------------------- */

test("the newest approval is current; an older one set current is what the app's link opens; withdrawing them all leaves none", async () => {
  const env = await world();
  const d1 = await release(env, V1);
  const d2 = await release(env, V2);
  const sha = async (r) => sha256Hex(r.bytes);

  assert.strictEqual((await current(env)).body.digest, d2);
  // The link itself hands over the current release's bytes.
  const opened = await call(env, "dan", "GET", `/orgs/acme/apps/${enc(APP)}/current`);
  assert.strictEqual(opened.status, 200);
  assert.strictEqual(await sha(opened), d2);
  const listed = (await call(env, "dan", "GET", "/orgs/acme/releases")).body;
  // (Approved in the same second, the list's order is the digest's.)
  const row = (d) => listed.find((r) => r.digest === d);
  assert.deepStrictEqual([row(d2).app_id, row(d2).current, row(d1).app_id, row(d1).current], [APP, true, APP, false]);

  // Only owners and admins choose.
  for (const who of ["bob", "carol", "dan"]) {
    const r = await call(env, who, "PUT", `/orgs/acme/apps/${enc(APP)}/current`, { json: { digest: d1 } });
    assert.deepStrictEqual(pair(r), [403, "role-cannot"], who);
  }
  // The rollback: an admin sets 1.0.0 back.
  const back = await call(env, "erin", "PUT", `/orgs/acme/apps/${enc(APP)}/current`, { json: { digest: d1 } });
  assert.strictEqual(back.status, 200, JSON.stringify(back.body));
  assert.strictEqual(back.body.current.digest, d1);
  const rolled = await call(env, "dan", "GET", `/orgs/acme/apps/${enc(APP)}/current`);
  assert.strictEqual(await sha(rolled), d1, "the older release is what opens");
  assert.strictEqual(rolled.headers.get("content-disposition"), 'attachment; filename="bounce.krate"');

  // Not a release of this app, or not released: refused, nothing changes.
  const waiting = (await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(V3) })).body.digest;
  assert.deepStrictEqual(pair(await call(env, "alice", "PUT", `/orgs/acme/apps/${enc(APP)}/current`, { json: { digest: waiting } })), [404, "digest-mismatch"]);
  assert.deepStrictEqual(pair(await call(env, "alice", "PUT", "/orgs/acme/apps/other.app/current", { json: { digest: d2 } })), [404, "digest-mismatch"]);
  assert.strictEqual((await current(env)).body.digest, d1);

  // A new approval moves forward again.
  await call(env, "bob", "POST", `/orgs/acme/candidates/${waiting}/approve`);
  assert.strictEqual((await current(env)).body.digest, waiting);
  await call(env, "alice", "PUT", `/orgs/acme/apps/${enc(APP)}/current`, { json: { digest: d1 } });

  // Withdrawing a version that is not current leaves current alone.
  await call(env, "alice", "POST", `/orgs/acme/releases/${d2}/withdraw`, { json: { reason: "superseded" } });
  assert.strictEqual((await current(env)).body.digest, d1);
  // A withdrawn release cannot be made current.
  assert.deepStrictEqual(pair(await call(env, "alice", "PUT", `/orgs/acme/apps/${enc(APP)}/current`, { json: { digest: d2 } })), [410, "release-withdrawn"]);
  // Withdrawing the current one: the newest remaining approval.
  const w1 = await call(env, "alice", "POST", `/orgs/acme/releases/${d1}/withdraw`, { json: { reason: "rolled forward" } });
  assert.strictEqual(w1.body.current.digest, waiting);
  const w3 = await call(env, "alice", "POST", `/orgs/acme/releases/${waiting}/withdraw`, { json: { reason: "the app is retired" } });
  assert.strictEqual(w3.body.current.digest, null, "or none");
  const none = await current(env, "dan");
  assert.deepStrictEqual(pair(none), [410, "release-withdrawn"]);
  assert.match(none.body.message, /the app is retired/);
  assert.deepStrictEqual(pair(await call(env, "stranger", "GET", `/orgs/acme/apps/${enc(APP)}/current`)), [403, "not-a-member"]);

  const a = await events(env);
  assert.strictEqual(a.chain_ok, true);
  const currents = a.events.filter((e) => e.action === "release.current" && e.result === "allowed").map((e) => e.reason);
  assert.deepStrictEqual(currents, [d1, d1, `${waiting} after withdrawing ${d1}`, `none after withdrawing ${waiting}`]);
  assert.ok(a.events.some((e) => e.action === "release.set-current" && e.actor === "github:carol" && e.reason === "role-cannot"));
});

/* ---- groups and audience ---------------------------------------------- */

test("groups are owners' and admins'; a release for finance refuses others, and owners, admins and reviewers always pass", async () => {
  const env = await world();
  const d1 = await release(env, V1);
  const d2 = await release(env, V2);

  // Groups: the matrix, the name, no twice.
  for (const who of ["bob", "carol", "dan"]) {
    assert.deepStrictEqual(pair(await call(env, who, "POST", "/orgs/acme/groups", { json: { name: "finance" } })), [403, "role-cannot"], who);
  }
  for (const name of ["", "Finance!", "1abc", "a".repeat(40)]) {
    assert.deepStrictEqual(pair(await call(env, "alice", "POST", "/orgs/acme/groups", { json: { name } })), [400, "group-invalid"], name);
  }
  assert.strictEqual((await call(env, "alice", "POST", "/orgs/acme/groups", { json: { name: "finance" } })).status, 201);
  assert.strictEqual((await call(env, "erin", "POST", "/orgs/acme/groups", { json: { name: "ops" } })).status, 201);
  assert.deepStrictEqual(pair(await call(env, "erin", "POST", "/orgs/acme/groups", { json: { name: "finance" } })), [409, "group-taken"]);
  const member = (g, p) => `/orgs/acme/groups/${g}/members/${enc(p)}`;
  assert.strictEqual((await call(env, "erin", "PUT", member("finance", "github:dan"))).status, 200);
  assert.deepStrictEqual(pair(await call(env, "erin", "PUT", member("finance", "github:stranger"))), [403, "not-a-member"]);
  assert.deepStrictEqual(pair(await call(env, "carol", "PUT", member("finance", "github:carol"))), [403, "role-cannot"]);
  assert.deepStrictEqual(pair(await call(env, "erin", "PUT", member("legal", "github:dan"))), [404, "no-such-group"]);
  const groups = (await call(env, "carol", "GET", "/orgs/acme/groups")).body.groups;
  assert.deepStrictEqual(groups.map((g) => [g.name, g.members]), [["finance", ["github:dan"]], ["ops", []]]);
  // A recipient sees only the groups they are in.
  assert.deepStrictEqual((await call(env, "dan", "GET", "/orgs/acme/groups")).body.groups.map((g) => g.name), ["finance"]);

  // Audience: owners and admins; only groups that exist.
  assert.deepStrictEqual(pair(await call(env, "bob", "PUT", `/orgs/acme/releases/${d2}/audience`, { json: { groups: ["finance"] } })), [403, "role-cannot"]);
  assert.deepStrictEqual(pair(await call(env, "alice", "PUT", `/orgs/acme/releases/${d2}/audience`, { json: { groups: ["legal"] } })), [404, "no-such-group"]);
  assert.deepStrictEqual(pair(await call(env, "alice", "PUT", `/orgs/acme/releases/${d2}/audience`, { json: { groups: [] } })), [400, "group-invalid"]);
  const set = await call(env, "alice", "PUT", `/orgs/acme/releases/${d2}/audience`, { json: { groups: ["ops", "finance"] } });
  assert.deepStrictEqual(set.body, { digest: d2, audience: ["finance", "ops"] });

  // carol (a developer in neither group) is refused download, check-in and lease.
  const refused = await call(env, "carol", "GET", `/a/${d2}`);
  assert.deepStrictEqual(pair(refused), [403, "not-in-audience"]);
  assert.strictEqual(refused.body.message, "this app is only for the finance and ops groups in Acme -- ask an admin of Acme to add you");
  assert.deepStrictEqual(refused.body.groups, ["finance", "ops"]);
  assert.deepStrictEqual(pair(await look(env, "carol", d2)), [403, "not-in-audience"]);
  assert.deepStrictEqual(pair(await call(env, "carol", "GET", `/orgs/acme/leases/${d2}`)), [403, "not-in-audience"]);
  assert.deepStrictEqual(pair(await call(env, "carol", "GET", `/orgs/acme/apps/${enc(APP)}/current`)), [403, "not-in-audience"]);
  // She does not see it listed; she still opens the version for everyone.
  assert.deepStrictEqual((await call(env, "carol", "GET", "/orgs/acme/releases")).body.map((r) => r.digest), [d1]);
  assert.strictEqual((await call(env, "carol", "GET", `/a/${d1}`)).status, 200);
  // dan is in finance: he opens it. alice, erin and bob pass whatever the audience.
  for (const who of ["dan", "alice", "erin", "bob"]) {
    assert.strictEqual((await call(env, who, "GET", `/a/${d2}`)).status, 200, who);
    assert.strictEqual((await call(env, who, "GET", `/orgs/acme/leases/${d2}`)).status, 200, who);
  }
  // A stranger still learns nothing.
  assert.deepStrictEqual(pair(await call(env, "stranger", "GET", `/a/${d2}`)), [403, "not-a-member"]);

  // Out of the group, out of the audience; back to everyone, back in.
  assert.strictEqual((await call(env, "erin", "DELETE", member("finance", "github:dan"))).status, 204);
  assert.deepStrictEqual(pair(await call(env, "dan", "GET", `/a/${d2}`)), [403, "not-in-audience"]);
  await call(env, "erin", "PUT", member("ops", "github:carol"));
  assert.strictEqual((await call(env, "carol", "GET", `/a/${d2}`)).status, 200);
  assert.strictEqual((await call(env, "alice", "PUT", `/orgs/acme/releases/${d2}/audience`, { json: { everyone: true } })).body.audience, null);
  assert.strictEqual((await call(env, "dan", "GET", `/a/${d2}`)).status, 200);

  // Someone removed leaves their groups, so an invitation back finds none.
  await call(env, "alice", "PUT", `/orgs/acme/releases/${d2}/audience`, { json: { groups: ["ops"] } });
  assert.strictEqual((await call(env, "alice", "DELETE", `/orgs/acme/members/${enc("github:carol")}`)).status, 204);
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: "carol", role: "developer" } });
  await call(env, "carol", "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  assert.deepStrictEqual(pair(await call(env, "carol", "GET", `/a/${d2}`)), [403, "not-in-audience"]);

  // A waiting candidate can be limited before it is approved.
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(V3) });
  assert.strictEqual((await call(env, "alice", "PUT", `/orgs/acme/releases/${up.body.digest}/audience`, { json: { groups: ["finance"] } })).status, 200);
  await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`);
  assert.deepStrictEqual(pair(await call(env, "carol", "GET", `/a/${up.body.digest}`)), [403, "not-in-audience"]);

  const a = await events(env);
  assert.strictEqual(a.chain_ok, true);
  const did = a.events.filter((e) => e.result === "allowed").map((e) => `${e.action} ${e.target} ${e.reason || ""}`.trim());
  for (const want of [
    "group.create group:finance",
    "group.create group:ops",
    "group.add group:finance/github:dan",
    "group.remove group:finance/github:dan",
    `release.audience release:${d2} groups:finance,ops`,
    `release.audience release:${d2} everyone`,
    "group.remove group:ops/github:carol member removed",
  ]) {
    assert.ok(did.includes(want), want);
  }
  assert.ok(a.events.some((e) => e.action === "release.download" && e.actor === "github:carol" && e.reason === "not-in-audience"));
});

/* ---- an older store ---------------------------------------------------- */

test("a store made before release control gains its columns, and its old releases have a current one", async () => {
  const storage = sqlStorage();
  storage.sql.exec(`CREATE TABLE candidates (
     digest TEXT PRIMARY KEY, name TEXT, version TEXT, namespace TEXT,
     size INTEGER, signer TEXT, big INTEGER NOT NULL DEFAULT 0,
     uploaded_by TEXT NOT NULL, uploaded_at INTEGER NOT NULL,
     state TEXT NOT NULL, approved_by TEXT, approved_at INTEGER)`);
  storage.sql.exec(
    "INSERT INTO candidates (digest, name, version, uploaded_by, uploaded_at, state, approved_by, approved_at) VALUES (?, 'Ledger', '1.0', 'github:carol', 1, 'released', 'github:bob', 2)",
    "a".repeat(64),
  );
  const org = new Org({ storage }, {});
  const columns = storage.db.prepare("PRAGMA table_info(candidates)").all().map((c) => c.name);
  for (const c of ["app_id", "audience", "withdrawn_at", "withdrawn_by", "withdraw_reason"]) assert.ok(columns.includes(c), c);
  // Recorded before app ids were kept: named by its name.
  assert.strictEqual(org.currentOf("ledger"), "a".repeat(64));
});
