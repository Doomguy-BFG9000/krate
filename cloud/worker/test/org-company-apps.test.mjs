/* The company's apps on every device, hub side (IC-953): what
 * GET /orgs/<slug>/apps answers each person.
 *
 * A member sees exactly the apps whose current release is in their
 * audience and not withdrawn, each with what a device needs to list and
 * open it in one look: name, app id, version, size, what it can do (the
 * IC-950 words), offline class and the app's own link. A withdrawn app is
 * gone from the list; a rollback lists the older version and the link
 * opens it. Owners, admins and reviewers still see everything.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-company-apps.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { orgEnv, call, acme, fixture } from "./org-tools.mjs";

const SEED = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const APP = "dev.krate.bounce";
const enc = encodeURIComponent;

async function release(env, name) {
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(name) });
  assert.strictEqual(up.status, 201, JSON.stringify(up.body));
  const ok = await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`);
  assert.strictEqual(ok.status, 200, JSON.stringify(ok.body));
  return up.body.digest;
}

const apps = async (env, who) => {
  const r = await call(env, who, "GET", "/orgs/acme/apps");
  assert.strictEqual(r.status, 200, `${who}: ${JSON.stringify(r.body)}`);
  return r.body.apps;
};
const opens = async (env, who) =>
  (await call(env, who, "GET", `/orgs/acme/apps/${enc(APP)}/current`, { headers: { accept: "application/json" } })).body.digest;

test("a member's list is the current release they may open, with what a device needs to show and open it", async () => {
  const env = orgEnv({ ORG_LEASE_KEY: SEED });
  await acme(env);
  const d1 = await release(env, "candidate-v1.krate");
  const d2 = await release(env, "candidate-v2.krate");
  await call(env, "alice", "PUT", "/orgs/acme/apps/Bounce/offline", { json: { class: "days-30" } });

  const [entry] = await apps(env, "dan");
  const waiting = (await call(env, "alice", "GET", "/orgs/acme/candidates")).body.find((c) => c.digest === d2);
  assert.deepStrictEqual(
    {
      app_id: entry.app_id,
      name: entry.name,
      version: entry.version,
      digest: entry.digest,
      current: entry.current,
      size: entry.size,
      offline: entry.offline,
      link: entry.link,
    },
    {
      app_id: APP,
      name: "Bounce",
      version: "1.0.1",
      digest: d2,
      current: d2,
      size: fixture("candidate-v2.krate").length,
      offline: "days-30",
      link: `https://hub.example/orgs/acme/apps/${enc(APP)}/current`,
    },
  );
  // What it can do: the same words the approver saw (IC-950).
  assert.ok(Array.isArray(entry.capabilities));
  assert.deepStrictEqual(entry.capabilities, waiting.capabilities.map((c) => ({ cap: c.cap, words: c.words })));
  // The older approved version is still there to open, by digest.
  assert.deepStrictEqual(entry.releases.map((r) => r.digest), [d2, d1]);
});

test("withdrawn is gone, a rollback lists and opens the older one, and an audience narrows the list", async () => {
  const env = orgEnv({ ORG_LEASE_KEY: SEED });
  await acme(env);
  const d1 = await release(env, "candidate-v1.krate");
  const d2 = await release(env, "candidate-v2.krate");

  // Rollback: an owner makes 1.0.0 current. Every member's list says so,
  // and the app's link opens it.
  assert.strictEqual((await call(env, "alice", "PUT", `/orgs/acme/apps/${enc(APP)}/current`, { json: { digest: d1 } })).status, 200);
  assert.deepStrictEqual((await apps(env, "dan")).map((a) => [a.version, a.digest]), [["1.0.0", d1]]);
  assert.strictEqual(await opens(env, "dan"), d1);

  // Only for finance, which dan is not in: the app leaves his list
  // entirely, though 1.0.1 is still for everyone.
  assert.strictEqual((await call(env, "alice", "POST", "/orgs/acme/groups", { json: { name: "finance" } })).status, 201);
  assert.strictEqual((await call(env, "alice", "PUT", `/orgs/acme/releases/${d1}/audience`, { json: { groups: ["finance"] } })).status, 200);
  assert.deepStrictEqual(await apps(env, "dan"), [], "the current release is not his, so the app is not on his list");
  // In finance: back on the list.
  assert.strictEqual((await call(env, "alice", "PUT", `/orgs/acme/groups/finance/members/${enc("github:dan")}`)).status, 200);
  assert.deepStrictEqual((await apps(env, "dan")).map((a) => a.digest), [d1]);
  // Owners and reviewers see it whatever the audience.
  assert.deepStrictEqual((await apps(env, "bob")).map((a) => a.digest), [d1]);

  // Withdraw the current one: the newest remaining (1.0.1) is current.
  assert.strictEqual((await call(env, "bob", "POST", `/orgs/acme/releases/${d1}/withdraw`, { json: { reason: "loses data" } })).status, 200);
  const [now] = await apps(env, "dan");
  assert.deepStrictEqual([now.version, now.digest], ["1.0.1", d2]);
  assert.ok(!now.releases.some((r) => r.state === "withdrawn"), "no withdrawn version on a member's list");
  // Withdraw that too: nothing of the app is left on a member's list...
  assert.strictEqual((await call(env, "bob", "POST", `/orgs/acme/releases/${d2}/withdraw`, { json: { reason: "retired" } })).status, 200);
  assert.deepStrictEqual(await apps(env, "dan"), []);
  assert.deepStrictEqual(await apps(env, "carol"), []);
  // ...while the owner still sees its whole history, with no current.
  const [all] = await apps(env, "alice");
  assert.deepStrictEqual([all.current, all.version, all.releases.map((r) => r.state)], [null, null, ["withdrawn", "withdrawn"]]);
});

test("a stranger is refused the list, and a removed member sees nothing", async () => {
  const env = orgEnv({ ORG_LEASE_KEY: SEED });
  await acme(env);
  await release(env, "candidate-v1.krate");
  const stranger = await call(env, "stranger", "GET", "/orgs/acme/apps");
  assert.deepStrictEqual([stranger.status, stranger.body.error], [403, "not-a-member"]);
  assert.strictEqual((await call(env, "alice", "DELETE", `/orgs/acme/members/${enc("github:dan")}`)).status, 204);
  const gone = await call(env, "dan", "GET", "/orgs/acme/apps");
  assert.deepStrictEqual([gone.status, gone.body.error], [403, "removed"]);
});
