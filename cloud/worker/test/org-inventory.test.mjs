/* Inventory, provenance and ownership, hub side (IC-954).
 *
 * The bundles are real signed and marked fixtures (fixtures/org-inventory,
 * written by crates/validator-wasm/tests/org_fixtures.rs), judged by the
 * real validator: two versions of an app made in Krate Studio with an AI
 * (the first keeping its request), and one made by hand.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-inventory.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { orgEnv, call } from "./org-tools.mjs";
import worker from "../src/index.js";
import { CSV_COLUMNS, builtWords, csvCell } from "../src/org-inventory.js";

const here = dirname(fileURLToPath(import.meta.url));
const FIX = join(here, "fixtures", "org-inventory");
const fixture = (name) => new Uint8Array(readFileSync(join(FIX, name)));
const KEYS = JSON.parse(readFileSync(join(FIX, "keys.json"), "utf8"));
const DAY = 86400;
const pair = (r) => [r.status, r.body && r.body.error];
const enc = encodeURIComponent;

async function send(env, who, name) {
  const up = await call(env, who, "POST", "/orgs/acme/candidates", { bytes: fixture(name) });
  assert.strictEqual(up.status, 201, JSON.stringify(up.body));
  return up.body.digest;
}
async function approve(env, digest) {
  const ok = await call(env, "bob", "POST", `/orgs/acme/candidates/${digest}/approve`);
  assert.strictEqual(ok.status, 200, JSON.stringify(ok.body));
}

/* acme with the inventory fixtures' root: alice owner, erin admin, bob
 * reviewer, carol and frank developers, dan recipient. */
async function world() {
  const env = orgEnv({ ORG_SECRET_KEY: "org-secret-key-for-tests" });
  const ok = (r, what) => {
    if (r.status >= 300) throw new Error(`${what}: ${r.status} ${JSON.stringify(r.body)}`);
    return r;
  };
  ok(await call(env, "alice", "POST", "/orgs", { json: { slug: "acme", name: "Acme" } }), "create");
  ok(await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: KEYS.root } }), "root");
  for (const [login, role] of [["erin", "admin"], ["bob", "reviewer"], ["carol", "developer"], ["frank", "developer"], ["dan", "recipient"]]) {
    const inv = ok(await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: login, role } }), `invite ${login}`);
    ok(await call(env, login, "POST", `/orgs/acme/invites/${inv.body.code}/accept`), `accept ${login}`);
  }
  return env;
}

const inventory = async (env, who = "alice") => {
  const r = await call(env, who, "GET", "/orgs/acme/inventory");
  assert.strictEqual(r.status, 200, JSON.stringify(r.body));
  return r.body;
};
const appIn = (inv, id) => inv.apps.find((a) => a.app_id === id);
const look = (env, who, digest) => call(env, who, "GET", `/a/${digest}`, { headers: { accept: "application/json" } });
const events = async (env) => (await call(env, "alice", "GET", "/orgs/acme/audit")).body;

async function at(seconds, fn) {
  const real = Date.now;
  Date.now = () => seconds * 1000;
  try {
    return await fn();
  } finally {
    Date.now = real;
  }
}

test("the build record is judged, kept with the candidate, and shown to the approver", async () => {
  const env = await world();
  const d1 = await send(env, "carol", "expenses-1.krate");
  const hand = await send(env, "frank", "rota.krate");
  const list = (await call(env, "bob", "GET", "/orgs/acme/candidates")).body;
  const one = list.find((c) => c.digest === d1);
  assert.deepStrictEqual(
    [one.provenance.tool, one.provenance.provider, one.provenance.model, one.provenance.prompt, one.provenance.revisions],
    ["krate studio", "claude", "claude-opus-5", "an expenses tracker for finance", 0],
  );
  assert.strictEqual(one.built_words, "Krate Studio with Claude (claude-opus-5)");
  const made = list.find((c) => c.digest === hand);
  assert.strictEqual(made.provenance, null);
  assert.strictEqual(made.built_words, "by hand");
  // Stored in the organisation's own store, with the candidate.
  const row = env.ORGS.object("acme").ctx.storage.db.prepare("SELECT provenance FROM candidates WHERE digest = ?").get(d1);
  assert.strictEqual(JSON.parse(row.provenance).prompt_sha256.length, 64);
});

test("the inventory lists every app with its owner, sender, how it was made, what it can do, connections, audience, people and state", async () => {
  const env = await world();
  const d1 = await send(env, "carol", "expenses-1.krate");
  await approve(env, d1);
  // Frank sends the next version: Carol, who sent it first, stays the owner.
  const d2 = await send(env, "frank", "expenses-2.krate");
  await approve(env, d2);
  const rota = await send(env, "frank", "rota.krate");

  // Opens are counted from check-ins, one person once a day.
  const t0 = Math.floor(Date.now() / 1000);
  await at(t0, async () => {
    for (const who of ["dan", "dan", "erin"]) assert.strictEqual((await look(env, who, d2)).status, 200);
    assert.strictEqual((await look(env, "alice", d1)).status, 200);
  });

  const inv = await inventory(env);
  assert.strictEqual(inv.names, true);
  const exp = appIn(inv, "com.acme.expenses");
  assert.deepStrictEqual(
    {
      name: exp.name,
      version: exp.version,
      state: exp.state,
      owner: exp.owner,
      needs_owner: exp.needs_owner,
      last_sent_by: exp.last_sent_by,
      built: [exp.built.tool, exp.built.provider, exp.built.model, exp.built.revisions, exp.built.prompt],
      built_words: exp.built_words,
      connections: exp.connections,
      audience: exp.audience,
      people: exp.people_30_days,
      names: exp.people.map((p) => p.principal),
    },
    {
      name: "Expenses",
      version: "1.0.1",
      state: "released",
      owner: { principal: "github:carol", display: "carol", active: true },
      needs_owner: false,
      last_sent_by: "github:frank",
      built: ["krate studio", "claude", "claude-opus-5", 1, null],
      built_words: "Krate Studio with Claude (claude-opus-5)",
      connections: ["finance-api"],
      audience: null,
      people: 3,
      names: ["github:alice", "github:dan", "github:erin"],
    },
  );
  assert.ok(exp.capabilities.some((c) => c.cap === "net.connection:finance-api" && c.words), JSON.stringify(exp.capabilities));
  const r = appIn(inv, "com.acme.rota");
  assert.deepStrictEqual([r.state, r.built, r.built_words, r.owner.principal, r.people_30_days, r.digest], ["waiting", null, "by hand", "github:frank", 0, rota]);

  // A reviewer reads it without names; a developer or recipient does not read it.
  const bobs = await inventory(env, "bob");
  assert.strictEqual(bobs.names, false);
  assert.strictEqual(appIn(bobs, "com.acme.expenses").people, undefined);
  assert.strictEqual(appIn(bobs, "com.acme.expenses").people_30_days, 3);
  for (const who of ["carol", "dan"]) assert.deepStrictEqual(pair(await call(env, who, "GET", "/orgs/acme/inventory")), [403, "role-cannot"], who);
  assert.deepStrictEqual(pair(await call(env, "stranger", "GET", "/orgs/acme/inventory")), [403, "not-a-member"]);

  // The CSV: the columns in order, one row per app, formulas defused.
  const res = await worker.fetch(
    new Request("https://hub.example/orgs/acme/inventory.csv", { headers: { authorization: "Bearer gho_alice" } }),
    env,
  );
  assert.strictEqual(res.status, 200);
  assert.match(res.headers.get("content-type"), /^text\/csv/);
  assert.match(res.headers.get("content-disposition"), /acme-apps\.csv/);
  const lines = (await res.text()).trim().split("\r\n");
  assert.strictEqual(lines[0], CSV_COLUMNS.join(","));
  assert.deepStrictEqual(lines[0].split(","), [
    "app_id", "name", "version", "state", "owner", "needs_owner", "last_sent_by", "built_with", "model", "can", "connections", "audience", "people_30_days",
  ]);
  assert.strictEqual(lines.length, 3);
  const row = lines.find((l) => l.startsWith("com.acme.expenses,"));
  assert.match(row, /^com\.acme\.expenses,Expenses,1\.0\.1,released,carol,no,frank,Krate Studio with Claude \(claude-opus-5\),claude-opus-5,.*,finance-api,everyone,3$/);
  const dans = await worker.fetch(new Request("https://hub.example/orgs/acme/inventory.csv", { headers: { authorization: "Bearer gho_dan" } }), env);
  assert.strictEqual(dans.status, 403);
});

test("usage keeps the day's people per release for 90 days, and answers daily counts by app and release", async () => {
  const env = await world();
  const d1 = await send(env, "carol", "expenses-1.krate");
  await approve(env, d1);
  const d2 = await send(env, "carol", "expenses-2.krate");
  await approve(env, d2);
  const t0 = Math.floor(Date.now() / 1000);
  // 100 days ago, then yesterday (dan on the older 1.0.0, erin on 1.0.1), today.
  await at(t0 - 100 * DAY, async () => assert.strictEqual((await look(env, "erin", d2)).status, 200));
  await at(t0 - DAY, async () => {
    await look(env, "dan", d1);
    await look(env, "erin", d2);
    await look(env, "erin", d2);
  });
  await at(t0, async () => {
    await look(env, "dan", d2);
  });
  const usage = await at(t0, async () => (await call(env, "alice", "GET", "/orgs/acme/usage?days=7")).body);
  const yesterday = new Date((t0 - DAY) * 1000).toISOString().slice(0, 10);
  const today = new Date(t0 * 1000).toISOString().slice(0, 10);
  assert.deepStrictEqual(
    usage.usage.map((u) => [u.day, u.app_id, u.people, u.releases.map((r) => [r.version, r.people])]),
    [
      [yesterday, "com.acme.expenses", 2, [["1.0.0", 1], ["1.0.1", 1]]],
      [today, "com.acme.expenses", 1, [["1.0.1", 1]]],
    ],
  );
  // Nothing older than 90 days is kept, and nothing but who, which release, which day.
  const db = env.ORGS.object("acme").ctx.storage.db;
  const rows = db.prepare("SELECT * FROM usage_daily").all();
  assert.ok(rows.every((r) => r.day >= new Date((t0 - 90 * DAY) * 1000).toISOString().slice(0, 10)), JSON.stringify(rows));
  assert.deepStrictEqual(Object.keys(rows[0]).sort(), ["app_id", "day", "digest", "principal", "version"]);
  // Owners and admins only.
  for (const who of ["bob", "carol", "dan"]) assert.deepStrictEqual(pair(await call(env, who, "GET", "/orgs/acme/usage")), [403, "role-cannot"], who);
  // A refused look is not an open: a withdrawn release, or one for groups
  // the person is not in.
  await call(env, "alice", "POST", `/orgs/acme/releases/${d1}/withdraw`, { json: { reason: "old" } });
  const before = db.prepare("SELECT COUNT(*) AS n FROM usage_daily").get().n;
  assert.deepStrictEqual(pair(await look(env, "frank", d1)), [410, "release-withdrawn"]);
  assert.strictEqual((await call(env, "alice", "POST", "/orgs/acme/groups", { json: { name: "finance" } })).status, 201);
  assert.strictEqual((await call(env, "alice", "PUT", `/orgs/acme/releases/${d2}/audience`, { json: { groups: ["finance"] } })).status, 200);
  assert.deepStrictEqual(pair(await look(env, "frank", d2)), [403, "not-in-audience"]);
  assert.strictEqual(db.prepare("SELECT COUNT(*) AS n FROM usage_daily").get().n, before);
});

test("removing the owner flags the app as needing an owner; an owner or admin assigns one; both audited", async () => {
  const env = await world();
  const d1 = await send(env, "carol", "expenses-1.krate");
  await approve(env, d1);
  assert.strictEqual(appIn(await inventory(env), "com.acme.expenses").owner.principal, "github:carol");

  assert.strictEqual((await call(env, "alice", "DELETE", `/orgs/acme/members/${enc("github:carol")}`)).status, 204);
  const inv = await inventory(env);
  const exp = appIn(inv, "com.acme.expenses");
  assert.deepStrictEqual([exp.needs_owner, exp.owner.active, inv.apps[0].app_id], [true, false, "com.acme.expenses"], "needing an owner comes first");

  // Who may assign, and to whom.
  const set = (who, principal, app = "com.acme.expenses") =>
    call(env, who, "PUT", `/orgs/acme/apps/${enc(app)}/owner`, { json: { principal } });
  for (const who of ["bob", "frank", "dan"]) assert.deepStrictEqual(pair(await set(who, "github:frank")), [403, "role-cannot"], who);
  assert.deepStrictEqual(pair(await set("erin", "github:carol")), [403, "not-a-member"], "a removed member cannot own an app");
  assert.deepStrictEqual(pair(await set("erin", "github:stranger")), [403, "not-a-member"]);
  assert.deepStrictEqual(pair(await set("erin", "github:frank", "com.acme.nothing")), [404, "no-such-app"]);
  const ok = await set("erin", "github:frank");
  assert.strictEqual(ok.status, 200, JSON.stringify(ok.body));
  const after = appIn(await inventory(env), "com.acme.expenses");
  assert.deepStrictEqual([after.needs_owner, after.owner.principal], [false, "github:frank"]);
  // Owning is not a permission: Frank's role is unchanged.
  const people = (await call(env, "alice", "GET", "/orgs/acme")).body.members;
  assert.strictEqual(people.find((m) => m.principal === "github:frank").role, "developer");

  const a = await events(env);
  assert.strictEqual(a.chain_ok, true);
  const did = a.events.filter((e) => e.action === "app.owner").map((e) => [e.actor, e.target, e.result, e.reason]);
  assert.deepStrictEqual(did.filter((d) => d[2] === "allowed"), [
    ["github:alice", "app:com.acme.expenses", "allowed", "needs an owner: github:carol was removed"],
    ["github:erin", "app:com.acme.expenses", "allowed", "github:frank"],
  ]);
  assert.ok(did.some((d) => d[0] === "github:bob" && d[2] === "denied"));
});

test("a spreadsheet cell never runs as a formula, and quotes what needs quoting", () => {
  assert.strictEqual(csvCell("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
  assert.strictEqual(csvCell("+1"), "'+1");
  assert.strictEqual(csvCell("-1"), "'-1");
  assert.strictEqual(csvCell("@sum"), "'@sum");
  assert.strictEqual(csvCell("a, b"), "\"a, b\"");
  assert.strictEqual(csvCell(null), "");
  assert.strictEqual(csvCell(3), "3");
});

test("the words for how an app was made are the same everywhere", () => {
  assert.strictEqual(builtWords(null), "by hand");
  assert.strictEqual(builtWords({ tool: "krate create", provider: null }), "krate create, from a built-in template");
  assert.strictEqual(builtWords({ tool: "krate create", provider: "author-cmd" }), "krate create with a command of the builder's own");
  assert.strictEqual(builtWords({ tool: "krate studio", provider: "codex", model: null }), "Krate Studio with Codex");
});
