/* Company connections, hub side (IC-952, ORG-FIRST-PLAN.md "IC-952
 * contract"): an owner or admin sets up an API's address and credential
 * once; the credential is write-only, encrypted at rest with the org secret
 * key, never in an answer, an audit event or the store in the clear; a
 * device gets it only for a member inside the connection's audience, for a
 * released (not withdrawn) version of an app the connection allows that
 * asked for it; each use is counted per day into the audit.
 *
 * The bundles are real signed and marked fixtures (fixtures/org-connections,
 * written by crates/validator-wasm/tests/org_fixtures.rs), judged by the
 * real validator: com.acme.ledger 1.0.0 asks for net.connection:finance-api,
 * 1.0.1 does not, and com.acme.other asks too.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-connections.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { orgEnv, call } from "./org-tools.mjs";
import { normalBase, normalAllow, credentialOf } from "../src/org-connections.js";

const here = dirname(fileURLToPath(import.meta.url));
const FIX = join(here, "fixtures", "org-connections");
const fixture = (name) => new Uint8Array(readFileSync(join(FIX, name)));
const KEYS = JSON.parse(readFileSync(join(FIX, "keys.json"), "utf8"));
const SECRET = "Bearer sk-hub-test-only-5f4e3d2c1b0a99887766";
const ROTATED = "Bearer sk-hub-test-rotated-0011223344556677";
const BASE = "https://api.acme.example/finance";
const DAY = 24 * 60 * 60;
const pair = (r) => [r.status, r.body && r.body.error];

const finance = (extra = {}) => ({
  name: "finance-api",
  base: BASE,
  header: { name: "Authorization", value: SECRET },
  allow: { methods: ["GET"], paths: ["/invoices"] },
  ...extra,
});

async function release(env, name) {
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(name) });
  assert.strictEqual(up.status, 201, JSON.stringify(up.body));
  const ok = await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`);
  assert.strictEqual(ok.status, 200, JSON.stringify(ok.body));
  return up.body.digest;
}

/* acme with its own root (the connection fixtures'), alice owner, erin
 * admin, bob reviewer, carol developer, dan recipient, and the three apps
 * released. */
async function world(extra = {}) {
  const env = orgEnv({ ORG_SECRET_KEY: "org-secret-key-for-tests", ...extra });
  const ok = (r, what) => {
    if (r.status >= 300) throw new Error(`${what}: ${r.status} ${JSON.stringify(r.body)}`);
    return r;
  };
  ok(await call(env, "alice", "POST", "/orgs", { json: { slug: "acme", name: "Acme" } }), "create");
  ok(await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: KEYS.root } }), "root");
  for (const [login, role] of [["erin", "admin"], ["bob", "reviewer"], ["carol", "developer"], ["dan", "recipient"]]) {
    const inv = ok(await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: login, role } }), `invite ${login}`);
    ok(await call(env, login, "POST", `/orgs/acme/invites/${inv.body.code}/accept`), `accept ${login}`);
  }
  const ledger = await release(env, "ledger.krate");
  const plain = await release(env, "ledger-plain.krate");
  const other = await release(env, "other.krate");
  return { env, ledger, plain, other };
}

const use = (env, who, digest, name = "finance-api") => call(env, who, "GET", `/orgs/acme/connections/${name}/credential/${digest}`);
const events = async (env) => (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events;
/* Every value in every table of acme's store, as one string. */
function storeText(env) {
  const db = env.ORGS.object("acme").ctx.storage.db;
  const tables = db.prepare("SELECT name FROM sqlite_master WHERE type = 'table'").all().map((t) => t.name);
  return tables.map((t) => JSON.stringify(db.prepare(`SELECT * FROM ${t}`).all())).join("\n");
}

/* ---- setting one up ----------------------------------------------------- */

test("only an owner or admin sets one up, and the credential never comes back", async () => {
  const { env } = await world();
  for (const who of ["bob", "carol", "dan"]) {
    assert.deepStrictEqual(pair(await call(env, who, "POST", "/orgs/acme/connections", { json: finance() })), [403, "role-cannot"], who);
  }
  assert.deepStrictEqual(pair(await call(env, "stranger", "POST", "/orgs/acme/connections", { json: finance() })), [403, "not-a-member"]);
  const made = await call(env, "erin", "POST", "/orgs/acme/connections", { json: finance() });
  assert.strictEqual(made.status, 201, JSON.stringify(made.body));
  const c = made.body.connection;
  assert.deepStrictEqual([c.name, c.base, c.allow, c.audience, c.apps], ["finance-api", BASE, { methods: ["GET"], paths: ["/invoices"] }, null, null]);
  assert.deepStrictEqual([c.credential.kind, c.credential.header, c.credential.set_by], ["header", "Authorization", "github:erin"]);
  assert.ok(!JSON.stringify(made.body).includes("sk-hub"), "the answer never carries it");
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance() })), [409, "connection-taken"]);

  // Who sees what exists: the people who build and approve the apps.
  for (const who of ["alice", "erin", "carol", "bob"]) {
    const list = await call(env, who, "GET", "/orgs/acme/connections");
    assert.strictEqual(list.status, 200, who);
    assert.strictEqual(list.body.connections[0].name, "finance-api");
    assert.ok(!JSON.stringify(list.body).includes("sk-hub"), who);
  }
  assert.deepStrictEqual(pair(await call(env, "dan", "GET", "/orgs/acme/connections")), [403, "role-cannot"]);
  assert.deepStrictEqual(pair(await call(env, null, "GET", "/orgs/acme/connections")), [401, "not-signed-in"]);

  // Encrypted at rest: nowhere in the store in the clear, the audit
  // included, nor in what the Idempotency-Key is bound to.
  const kept = storeText(env);
  assert.ok(!kept.includes("sk-hub"), "the store holds no credential in the clear");
  assert.ok(!kept.includes(Buffer.from(SECRET).toString("base64")), "nor its base64");
  const sealed = JSON.parse(env.ORGS.object("acme").ctx.storage.db.prepare("SELECT sealed FROM connections").get().sealed);
  assert.ok(sealed.iv && sealed.body, "AES-GCM ciphertext");
  const audit = (await events(env)).filter((e) => e.action.startsWith("connection."));
  assert.deepStrictEqual(audit.map((e) => [e.actor, e.action, e.target, e.result]), [
    ["github:bob", "connection.manage", "connection:finance-api", "denied"],
    ["github:carol", "connection.manage", "connection:finance-api", "denied"],
    ["github:dan", "connection.manage", "connection:finance-api", "denied"],
    ["github:stranger", "connection.manage", "connection:finance-api", "denied"],
    ["github:erin", "connection.create", "connection:finance-api", "allowed"],
    ["github:alice", "connection.manage", "connection:finance-api", "denied"],
    ["github:dan", "connection.view", "org:acme", "denied"],
  ]);
});

test("a connection must be complete, exact, and safe to send", async () => {
  const { env } = await world();
  const refused = async (body, words) => {
    const r = await call(env, "alice", "POST", "/orgs/acme/connections", { json: { ...finance(), ...body } });
    assert.strictEqual(r.status, 400, `${JSON.stringify(body)} -> ${r.status} ${JSON.stringify(r.body)}`);
    assert.strictEqual(r.body.error, "connection-invalid");
    if (words) assert.match(r.body.message, words);
    assert.ok(!JSON.stringify(r.body).includes("sk-hub"), "a refusal never echoes the credential");
  };
  await refused({ name: "Finance API" }, /lower-case/);
  await refused({ base: "http://api.acme.example/finance" }, /https/);
  await refused({ base: "https://user:pw@api.acme.example/x" }, /user or password/);
  await refused({ base: "https://api.acme.example/x?key=1" }, /no \? or #/);
  await refused({ base: "https://api.acme.example/a/../b" });
  await refused({ base: "not a url" }, /full URL/);
  await refused({ header: { name: "Host", value: "x" } }, /connection itself sets/);
  await refused({ header: { name: "Authorization", value: "Bearer a\r\nX-Evil: 1" } }, /one line/);
  await refused({ header: { name: "Bad Name", value: "x" } }, /one word/);
  await refused({ header: undefined }, /credential/);
  await refused({ allow: { methods: ["TRACE"] } }, /method is one of/);
  await refused({ allow: { methods: ["GET"], paths: ["/a/../b"] } }, /\.\./);
  await refused({ allow: { methods: ["GET"], paths: ["/a%2fb"] } });
  const group = await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance({ audience: { groups: ["finance"] } }) });
  assert.deepStrictEqual(pair(group), [404, "no-such-group"]);
  const app = await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance({ apps: { ids: ["Not An Id!"] } }) });
  assert.deepStrictEqual(pair(app), [400, "connection-invalid"]);
  // Nothing was made by any of it.
  assert.deepStrictEqual((await call(env, "alice", "GET", "/orgs/acme/connections")).body.connections, []);

  // The parts on their own.
  assert.strictEqual(normalBase("https://API.Acme.example:8443/finance/").base, "https://api.acme.example:8443/finance");
  assert.ok(normalBase("http://127.0.0.1:9000/x", { PUBLIC_BASE: "https://hub.krate.tech" }).problem, "plain http only on a local proof hub");
  assert.strictEqual(normalBase("http://127.0.0.1:9000/x", { PUBLIC_BASE: "http://127.0.0.1:8787" }).base, "http://127.0.0.1:9000/x");
  assert.ok(normalBase("http://10.0.0.5/x", { PUBLIC_BASE: "http://127.0.0.1:8787" }).problem, "and only to this machine");
  assert.deepStrictEqual(normalAllow(undefined).allow, { methods: ["GET"], paths: ["/"] }, "GET on everything by default");
  assert.deepStrictEqual(normalAllow({ methods: ["post", "get"], paths: ["invoices/", "/rates"] }).allow, { methods: ["GET", "POST"], paths: ["/invoices", "/rates"] });
  const basic = credentialOf({ basic: { user: "svc", password: "pw" } });
  assert.deepStrictEqual(basic, { kind: "basic", header_name: "Authorization", value: `Basic ${Buffer.from("svc:pw").toString("base64")}` });
  assert.ok(credentialOf({ basic: { user: "a:b", password: "x" } }).problem);
});

test("no ORG_SECRET_KEY: nothing can be stored or handed out", async () => {
  const { env, ledger } = await world();
  const made = await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance() });
  assert.strictEqual(made.status, 201);
  delete env.ORG_SECRET_KEY;
  assert.deepStrictEqual(pair(await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance({ name: "other" }) })), [503, "connections-off"]);
  assert.deepStrictEqual(pair(await use(env, "dan", ledger)), [503, "connections-off"]);
  // A different key does not open what the first sealed.
  env.ORG_SECRET_KEY = "a-different-key";
  const r = await use(env, "dan", ledger);
  assert.deepStrictEqual(pair(r), [503, "connections-off"]);
  assert.match(r.body.message, /does not open/);
});

/* ---- handing it to a device -------------------------------------------- */

test("the credential goes only to a member inside the audience, for a release of an allowed app that asked for it", async () => {
  const { env, ledger, plain, other } = await world();
  await call(env, "alice", "POST", "/orgs/acme/groups", { json: { name: "finance" } });
  await call(env, "alice", "PUT", "/orgs/acme/groups/finance/members/github%3Adan");
  const made = await call(env, "alice", "POST", "/orgs/acme/connections", {
    json: finance({ audience: { groups: ["finance"] }, apps: { ids: ["com.acme.ledger"] } }),
  });
  assert.strictEqual(made.status, 201, JSON.stringify(made.body));

  // Dan, in finance, opening com.acme.ledger 1.0.0, which asked for it.
  const got = await use(env, "dan", ledger);
  assert.strictEqual(got.status, 200, JSON.stringify(got.body));
  assert.deepStrictEqual(got.body, {
    name: "finance-api",
    base: BASE,
    allow: { methods: ["GET"], paths: ["/invoices"] },
    header: { name: "Authorization", value: SECRET },
  });
  assert.strictEqual(got.headers.get("cache-control"), "private, no-store");

  // Each refusal, with its stable reason; past standing, the address so the
  // device can word it, and never the credential.
  const no = async (who, digest, want, name) => {
    const r = await use(env, who, digest, name);
    assert.deepStrictEqual(pair(r), want, `${who}: ${JSON.stringify(r.body)}`);
    assert.ok(!JSON.stringify(r.body).includes("sk-hub"), `${who} saw the credential`);
    return r;
  };
  const bob = await no("bob", ledger, [403, "connection-not-in-audience"]);
  assert.strictEqual(bob.body.base, BASE);
  assert.deepStrictEqual(bob.body.groups, ["finance"]);
  assert.match(bob.body.message, /only for the finance group in Acme/);
  const otherApp = await no("dan", other, [403, "app-not-allowed"]);
  assert.match(otherApp.body.message, /add com\.acme\.other/);
  const notAsked = await no("dan", plain, [403, "connection-not-requested"]);
  assert.match(notAsked.body.message, /net\.connection:finance-api/);
  const stranger = await no("stranger", ledger, [403, "not-a-member"]);
  assert.strictEqual(stranger.body.base, undefined, "a stranger learns nothing about the address");
  await no(null, ledger, [401, "not-signed-in"]);
  await no("dan", "0".repeat(64), [404, "digest-mismatch"]);
  await no("dan", ledger, [404, "no-such-connection"], "payroll-api");

  // A waiting candidate is not a release.
  const waiting = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("ledger.krate") });
  assert.strictEqual(waiting.body.state, "released", "the same bytes are the release they already are");

  // The release's own audience applies too.
  await call(env, "alice", "POST", "/orgs/acme/groups", { json: { name: "ops" } });
  await call(env, "alice", "PUT", `/orgs/acme/releases/${ledger}/audience`, { json: { groups: ["ops"] } });
  await no("dan", ledger, [403, "not-in-audience"]);
  await call(env, "alice", "PUT", `/orgs/acme/releases/${ledger}/audience`, { json: { everyone: true } });
  assert.strictEqual((await use(env, "dan", ledger)).status, 200);

  // Withdrawn: refused at once.
  await call(env, "alice", "POST", `/orgs/acme/releases/${ledger}/withdraw`, { json: { reason: "bad build" } });
  await no("dan", ledger, [410, "release-withdrawn"]);

  // Every refusal is in the audit, and no event carries the credential.
  const audit = await events(env);
  assert.ok(audit.filter((e) => e.action === "connection.use" && e.result === "denied").length >= 6);
  assert.ok(!JSON.stringify(audit).includes("sk-hub"));
});

test("a removed member gets nothing, and a removed connection is gone with its credential", async () => {
  const { env, ledger } = await world();
  await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance() });
  assert.strictEqual((await use(env, "dan", ledger)).status, 200);
  assert.strictEqual((await call(env, "alice", "DELETE", "/orgs/acme/members/github%3Adan")).status, 204);
  assert.deepStrictEqual(pair(await use(env, "dan", ledger)), [403, "removed"]);

  assert.strictEqual((await use(env, "carol", ledger)).status, 200);
  for (const who of ["carol", "bob"]) {
    assert.deepStrictEqual(pair(await call(env, who, "DELETE", "/orgs/acme/connections/finance-api")), [403, "role-cannot"], who);
  }
  assert.strictEqual((await call(env, "erin", "DELETE", "/orgs/acme/connections/finance-api")).status, 204);
  const gone = await use(env, "carol", ledger);
  assert.deepStrictEqual(pair(gone), [404, "no-such-connection"]);
  assert.match(gone.body.message, /any more/);
  assert.strictEqual(env.ORGS.object("acme").ctx.storage.db.prepare("SELECT COUNT(*) AS n FROM connections").get().n, 0, "the sealed credential is deleted");
  assert.deepStrictEqual(pair(await call(env, "erin", "DELETE", "/orgs/acme/connections/finance-api")), [404, "no-such-connection"]);
  // Its uses that day were written to the audit as it went.
  const lines = (await events(env)).filter((e) => e.action === "connection.use" && e.result === "allowed");
  assert.deepStrictEqual(lines.map((e) => [e.actor, e.target, e.reason.replace(/\d{4}-\d\d-\d\d/, "DAY")]), [["system:krate", "connection:finance-api", "2 uses on DAY"]]);
});

/* ---- changing one ------------------------------------------------------ */

test("a new credential is the one handed out next; a new address needs a new credential", async () => {
  const { env, ledger } = await world();
  await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance() });
  assert.strictEqual((await use(env, "dan", ledger)).body.header.value, SECRET);

  for (const who of ["carol", "dan"]) {
    const r = await call(env, who, "PUT", "/orgs/acme/connections/finance-api/credential", { json: { header: { name: "Authorization", value: ROTATED } } });
    assert.deepStrictEqual(pair(r), [403, "role-cannot"], who);
  }
  const replaced = await call(env, "alice", "PUT", "/orgs/acme/connections/finance-api/credential", { json: { header: { name: "Authorization", value: ROTATED } } });
  assert.strictEqual(replaced.status, 200, JSON.stringify(replaced.body));
  assert.ok(!JSON.stringify(replaced.body).includes("sk-hub"));
  assert.strictEqual((await use(env, "dan", ledger)).body.header.value, ROTATED, "the next run gets the new one");
  assert.ok(!storeText(env).includes("sk-hub"), "the new one is sealed too, and the old one is gone");

  // The allow, audience and apps change without the credential.
  const widened = await call(env, "alice", "PUT", "/orgs/acme/connections/finance-api", { json: { allow: { methods: ["GET", "POST"], paths: ["/"] } } });
  assert.strictEqual(widened.status, 200, JSON.stringify(widened.body));
  assert.deepStrictEqual(widened.body.connection.allow, { methods: ["GET", "POST"], paths: ["/"] });
  // Sending it somewhere new does not: an admin who never knew the secret
  // must not be able to point it at a server of theirs.
  const moved = await call(env, "erin", "PUT", "/orgs/acme/connections/finance-api", { json: { base: "https://evil.example/collect" } });
  assert.deepStrictEqual(pair(moved), [400, "credential-required"]);
  assert.strictEqual((await use(env, "dan", ledger)).body.base, BASE, "nothing moved");
  const movedWith = await call(env, "erin", "PUT", "/orgs/acme/connections/finance-api", {
    json: { base: "https://api2.acme.example/finance", header: { name: "X-Api-Key", value: "k-new" } },
  });
  assert.strictEqual(movedWith.status, 200, JSON.stringify(movedWith.body));
  const now = (await use(env, "dan", ledger)).body;
  assert.deepStrictEqual([now.base, now.header], ["https://api2.acme.example/finance", { name: "X-Api-Key", value: "k-new" }]);

  const audit = (await events(env)).filter((e) => e.result === "allowed" && /^connection\.(create|change|credential)/.test(e.action));
  assert.deepStrictEqual(audit.map((e) => [e.actor, e.action, e.reason]), [
    ["github:alice", "connection.create", BASE],
    ["github:alice", "connection.credential", null],
    ["github:alice", "connection.change", "allow GET,POST /"],
    ["github:erin", "connection.change", "address https://api2.acme.example/finance; credential replaced"],
  ]);
});

test("the same Idempotency-Key is the same request, and binding it keeps no trace of the credential", async () => {
  const { env } = await world();
  const once = await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance(), key: "k-1" });
  assert.strictEqual(once.status, 201);
  const again = await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance(), key: "k-1" });
  assert.strictEqual(again.status, 201, "replayed, not connection-taken");
  const other = await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance({ header: { name: "Authorization", value: ROTATED } }), key: "k-1" });
  assert.deepStrictEqual(pair(other), [409, "idempotency-key-reused"]);
  assert.ok(!storeText(env).includes("sk-hub"));
});

/* ---- counting ---------------------------------------------------------- */

test("each use is counted per day, and each finished day becomes one audit line", async () => {
  const { env, ledger } = await world();
  await call(env, "alice", "POST", "/orgs/acme/connections", { json: finance() });
  for (let i = 0; i < 3; i += 1) assert.strictEqual((await use(env, "dan", ledger)).status, 200);
  const today = (await call(env, "alice", "GET", "/orgs/acme/connections")).body.connections[0].uses;
  assert.deepStrictEqual(today, { today: 3, last_30_days: 3 });
  assert.strictEqual((await events(env)).filter((e) => e.action === "connection.use").length, 0, "no line until the day is over");

  // The next day: the first command writes yesterday's line.
  const store = env.ORGS.object("acme");
  const real = store.now.bind(store);
  store.now = () => real() + DAY;
  assert.strictEqual((await use(env, "dan", ledger)).status, 200);
  const lines = (await events(env)).filter((e) => e.action === "connection.use");
  assert.strictEqual(lines.length, 1);
  assert.strictEqual(lines[0].actor, "system:krate");
  assert.match(lines[0].reason, /^3 uses on \d{4}-\d\d-\d\d$/);
  const next = (await call(env, "alice", "GET", "/orgs/acme/connections")).body.connections[0].uses;
  assert.deepStrictEqual(next, { today: 1, last_30_days: 4 });
  // The chain still holds.
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme/audit")).body.chain_ok, true);
});
