/* Company sign-in by OpenID Connect (IC-948, Addendum 3 F, Addendum 4 F1-F3).
 *
 * Against a fake issuer written for these tests (fake-oidc.mjs): a real
 * HTTP server on this machine with its own keys and JWKS. No real provider
 * is ever called.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-sso.test.mjs
 */
import { test, after } from "node:test";
import assert from "node:assert";
import { orgEnv, call, acme, worker } from "./org-tools.mjs";
import { startIssuer } from "./fake-oidc.mjs";
import { verifyIdToken } from "../src/org-sso.js";
import { generateKeyPairSync, createSign } from "node:crypto";

const HUB = "http://127.0.0.1:8787";
const SECRET = "client-secret-never-shown";
const issuers = [];
after(async () => {
  for (const i of issuers) await i.close();
});

async function newIssuer() {
  const issuer = await startIssuer({ clientId: "krate-acme", clientSecret: SECRET });
  issuers.push(issuer);
  return issuer;
}

function ssoEnv(issuer, extra = {}) {
  return orgEnv({ PUBLIC_BASE: HUB, ORG_SECRET_KEY: "org-secret-key-for-tests", ORG_DOH_URL: `${issuer.url}/dns-query`, ...extra });
}

const settings = (issuer, more = {}) => ({
  issuer: issuer.url,
  client_id: issuer.clientId,
  client_secret: SECRET,
  domain: "corp.example",
  ...more,
});

/* acme, with company sign-in at the fake issuer for corp.example. */
async function ssoAcme({ autoJoin = "developer", enforce = false, verify = true } = {}) {
  const issuer = await newIssuer();
  const env = ssoEnv(issuer);
  await acme(env);
  const put = await call(env, "alice", "PUT", "/orgs/acme/sso", { json: settings(issuer, { auto_join_role: autoJoin, enforce }) });
  assert.strictEqual(put.status, 200, JSON.stringify(put.body));
  if (verify) {
    const token = await call(env, "alice", "GET", "/orgs/acme/sso/domain-token");
    issuer.txt.set(token.body.name, [token.body.value]);
    const ok = await call(env, "alice", "POST", "/orgs/acme/sso/verify-domain");
    assert.deepStrictEqual([ok.status, ok.body.domain_verified], [200, true], JSON.stringify(ok.body));
  }
  return { env, issuer };
}

const freshNonce = () => [...crypto.getRandomValues(new Uint8Array(16))].map((b) => b.toString(16).padStart(2, "0")).join("");

/* A browser signing in at /login/?org=acme: start at the hub, through the
 * issuer, back to the hub's callback, and where that sends it. */
async function signIn(env, { org = "acme", nonce = freshNonce(), replay = false } = {}) {
  const q = new URLSearchParams({ org });
  if (nonce) q.set("nonce", nonce);
  const start = await worker.fetch(new Request(`${HUB}/login/oidc/start?${q}`), env);
  const toIssuer = start.headers.get("location") || "";
  if (!toIssuer.startsWith("http://127.0.0.1")) {
    const where = new URL(toIssuer);
    return { where, start, error: where.searchParams.get("error") };
  }
  const auth = await fetch(toIssuer, { redirect: "manual" });
  const back = auth.headers.get("location") || "";
  if (!back.startsWith(`${HUB}/login/oidc/callback`)) return { issuerRefused: auth.status, authorize: new URL(toIssuer) };
  const cb = await worker.fetch(new Request(back), env);
  if (replay) await worker.fetch(new Request(back), env).then((r) => (replay.second = r.headers.get("location")));
  const where = new URL(cb.headers.get("location"));
  const out = { where, authorize: new URL(toIssuer), callback: back, nonce };
  if (where.pathname.startsWith("/login/done")) out.hand = new URLSearchParams(where.hash.slice(1));
  else out.error = where.searchParams.get("error");
  return out;
}

const bearer = (token) => ({ authorization: `Bearer ${token}` });
const as = (env, token, method, path, json) =>
  worker
    .fetch(
      new Request(`${HUB}${path}`, {
        method,
        headers: { ...bearer(token), ...(json ? { "content-type": "application/json" } : {}) },
        body: json ? JSON.stringify(json) : undefined,
      }),
      env,
    )
    .then(async (r) => ({ status: r.status, body: await r.json().catch(() => null) }));
const auditOf = async (env) => (await call(env, "alice", "GET", "/orgs/acme/audit")).body;

/* ---- the path a person takes ------------------------------------------ */

test("a person at the verified domain signs in through the issuer and joins with the auto-join role", async () => {
  const { env, issuer } = await ssoAcme();
  const r = await signIn(env);
  assert.ok(r.hand, `signed in: ${r.where}`);
  // K-909: the browser's own nonce comes back beside the session, so
  // /login/done can check this browser started it.
  assert.strictEqual(r.hand.get("nonce"), r.nonce);
  assert.strictEqual(r.hand.get("org"), "acme");
  assert.strictEqual(r.hand.get("sso"), "joined");
  // What went to the issuer: PKCE S256, a state, an OIDC nonce of the hub's
  // own (not the browser's), the openid scope.
  const p = r.authorize.searchParams;
  assert.strictEqual(p.get("code_challenge_method"), "S256");
  assert.ok(p.get("code_challenge") && p.get("state") && p.get("nonce"));
  assert.notStrictEqual(p.get("nonce"), r.nonce);
  assert.match(p.get("scope"), /\bopenid\b/);
  const exchange = issuer.tokenRequests.at(-1);
  assert.ok(exchange.code_verifier && exchange.code_verifier.length >= 43, "the verifier was sent");
  const token = r.hand.get("token");
  const show = await as(env, token, "GET", "/orgs/acme");
  assert.strictEqual(show.status, 200, JSON.stringify(show.body));
  assert.deepStrictEqual([show.body.me.principal, show.body.me.role], ["email:erin@corp.example", "developer"]);
  const mine = await as(env, token, "GET", "/orgs");
  assert.deepStrictEqual(mine.body, [{ slug: "acme", name: "Acme", role: "developer" }]);
  const events = (await auditOf(env)).events;
  for (const action of ["sso.set", "sso.verify-domain", "sso.join"]) assert.ok(events.some((e) => e.action === action && e.result === "allowed"), action);
  // Signing in again is the same person, still a member, joined once.
  const again = await signIn(env);
  assert.strictEqual(again.hand.get("sso"), "member");
  assert.strictEqual((await auditOf(env)).events.filter((e) => e.action === "sso.join").length, 1);
  assert.strictEqual((await auditOf(env)).chain_ok, true);
});

test("with no auto-join role a verified sign-in proves the address and joins nobody", async () => {
  const { env } = await ssoAcme({ autoJoin: null });
  const r = await signIn(env);
  assert.strictEqual(r.hand.get("sso"), "no-auto-join");
  assert.strictEqual((await as(env, r.hand.get("token"), "GET", "/orgs/acme")).body.error, "not-a-member");
  // An invitation for that address is accepted with the SSO session.
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { email: "erin@corp.example", role: "reviewer" } });
  const ok = await as(env, r.hand.get("token"), "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  assert.deepStrictEqual([ok.status, ok.body.membership.role], [200, "reviewer"]);
});

test("the sign-in merges into the account that already proved that inbox, never another", async () => {
  const { env } = await ssoAcme();
  // erin proved erin@corp.example by email link earlier: one account,
  // found by that inbox as ensureUser finds it.
  await call(env, "erin@corp.example", "GET", "/orgs");
  const accountId = "mail-erin-corp-example";
  env._kv.set("ident:email:erin@corp.example", accountId);
  const r = await signIn(env);
  const userId = env._kv.get(`session:${r.hand.get("token")}`);
  assert.strictEqual(userId, accountId, "the same account");
});

/* ---- F1: the secret ---------------------------------------------------- */

test("the client secret is encrypted at rest, never answered, never audited, never logged", async () => {
  const errors = [];
  const original = console.error;
  console.error = (...a) => errors.push(a.join(" "));
  try {
    const { env, issuer } = await ssoAcme();
    const view = await call(env, "alice", "GET", "/orgs/acme/sso");
    assert.deepStrictEqual(view.body, {
      issuer: issuer.url,
      client_id: "krate-acme",
      has_secret: true,
      domain: "corp.example",
      domain_verified: true,
      verified_at: view.body.verified_at,
      auto_join_role: "developer",
      enforce: false,
    });
    assert.ok(Number.isInteger(view.body.verified_at));
    // Nowhere in the organisation's store, in any table, as plain text.
    const db = env.ORGS.object("acme").ctx.storage.db;
    const tables = db.prepare("SELECT name FROM sqlite_master WHERE type = 'table'").all().map((t) => t.name);
    for (const t of tables) {
      const dump = JSON.stringify(db.prepare(`SELECT * FROM ${t}`).all());
      assert.ok(!dump.includes(SECRET), `the secret is not in ${t}`);
    }
    const stored = db.prepare("SELECT secret FROM sso").get().secret;
    assert.match(stored, /"iv":/, "it is the AES-GCM ciphertext");
    // Not in KV either.
    for (const [k, v] of env._kv) assert.ok(!String(v).includes(SECRET), `not in KV ${k}`);
    // A sign-in still works, so the hub decrypts it to use it ...
    const ok = await signIn(env);
    assert.ok(ok.hand);
    assert.strictEqual(issuer.tokenRequests.at(-1).client_secret, SECRET);
    // ... and a failing one logs why without the secret, the code or a token.
    issuer.tamper = (claims) => {
      claims.aud = "someone-else";
    };
    const bad = await signIn(env);
    assert.strictEqual(bad.error, "sso-failed");
    const logged = errors.join("\n");
    assert.match(logged, /wrong-audience/);
    assert.ok(!logged.includes(SECRET));
    assert.ok(!logged.includes(new URL(bad.callback).searchParams.get("code")));
    // Leaving client_secret out of an update keeps the one held.
    const update = await call(env, "alice", "PUT", "/orgs/acme/sso", { json: { ...settings(issuer), client_secret: undefined, auto_join_role: "reviewer" } });
    assert.deepStrictEqual([update.status, update.body.has_secret, update.body.auto_join_role], [200, true, "reviewer"]);
    issuer.tamper = null;
    assert.ok((await signIn(env)).hand, "the kept secret still works");
  } finally {
    console.error = original;
  }
});

test("a secret sealed under another ORG_SECRET_KEY does not open, and the sign-in fails", async () => {
  const { env } = await ssoAcme();
  env.ORG_SECRET_KEY = "a-different-key";
  assert.strictEqual((await signIn(env)).error, "sso-failed");
});

test("only an owner reads or changes company sign-in", async () => {
  const { env, issuer } = await ssoAcme();
  for (const who of ["bob", "carol", "dan"]) {
    assert.strictEqual((await call(env, who, "GET", "/orgs/acme/sso")).body.error, "role-cannot", who);
    assert.strictEqual((await call(env, who, "PUT", "/orgs/acme/sso", { json: settings(issuer) })).body.error, "role-cannot", who);
    assert.strictEqual((await call(env, who, "GET", "/orgs/acme/sso/domain-token")).body.error, "role-cannot", who);
  }
  assert.strictEqual((await call(env, "stranger", "GET", "/orgs/acme/sso")).body.error, "not-a-member");
  assert.strictEqual((await call(env, null, "GET", "/orgs/acme/sso")).status, 401);
  // An organisation with none answers null.
  const plain = ssoEnv(issuer);
  await acme(plain);
  const none = await call(plain, "alice", "GET", "/orgs/acme/sso");
  assert.deepStrictEqual([none.status, none.body], [200, null]);
});

/* ---- F2: the domain proof --------------------------------------------- */

test("the domain is proved by a TXT record at _krate-verify, read through DNS over HTTPS", async () => {
  const { env, issuer } = await ssoAcme({ verify: false });
  const token = await call(env, "alice", "GET", "/orgs/acme/sso/domain-token");
  assert.strictEqual(token.body.name, "_krate-verify.corp.example");
  assert.match(token.body.value, /^krate-verify=[a-z0-9]{24}$/);
  // Nothing there yet, then the wrong value, then a record split in two
  // strings the way long TXT records are.
  assert.strictEqual((await call(env, "alice", "POST", "/orgs/acme/sso/verify-domain")).body.error, "domain-not-verified");
  issuer.txt.set(token.body.name, ["krate-verify=somebody-elses-value"]);
  const wrong = await call(env, "alice", "POST", "/orgs/acme/sso/verify-domain");
  assert.deepStrictEqual([wrong.status, wrong.body.error], [409, "domain-not-verified"]);
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme/sso")).body.domain_verified, false);
  issuer.txt.set(token.body.name, ["v=spf1 -all", token.body.value]);
  const ok = await call(env, "alice", "POST", "/orgs/acme/sso/verify-domain");
  assert.deepStrictEqual([ok.status, ok.body.domain_verified], [200, true]);
  const events = (await auditOf(env)).events;
  assert.strictEqual(events.filter((e) => e.action === "sso.verify-domain" && e.result === "denied").length, 2);
  // A new domain starts unproved, with a new value to publish.
  const moved = await call(env, "alice", "PUT", "/orgs/acme/sso", { json: settings(issuer, { domain: "corp2.example" }) });
  assert.strictEqual(moved.body.domain_verified, false);
  const next = await call(env, "alice", "GET", "/orgs/acme/sso/domain-token");
  assert.notStrictEqual(next.body.value, token.body.value);
});

test("a non-owner's verify-domain makes no DNS query at all", async () => {
  const { env, issuer } = await ssoAcme({ verify: false });
  let queries = 0;
  const original = globalThis.fetch;
  globalThis.fetch = (input, init) => {
    if (String(input.url || input).includes("/dns-query")) queries += 1;
    return original(input, init);
  };
  try {
    assert.strictEqual((await call(env, "carol", "POST", "/orgs/acme/sso/verify-domain")).body.error, "role-cannot");
    assert.strictEqual((await call(env, "stranger", "POST", "/orgs/acme/sso/verify-domain")).body.error, "not-a-member");
    assert.strictEqual(queries, 0);
  } finally {
    globalThis.fetch = original;
  }
  void issuer;
});

/* ---- the denial suite --------------------------------------------------- */

for (const [what, tamper, signWith] of [
  ["a token for another client (wrong aud)", (c) => (c.aud = "another-client")],
  ["an expired token", (c) => ((c.exp = Math.floor(Date.now() / 1000) - 600), (c.iat = c.exp - 300))],
  ["a bad signature", null, "other"],
  ["the wrong nonce", (c) => (c.nonce = "not-the-one-the-hub-sent")],
  ["another issuer (wrong iss)", (c) => (c.iss = "https://idp.evil.example")],
  ["alg none", (c, h) => (h.alg = "none")],
  ["a shared-secret algorithm", (c, h) => (h.alg = "HS256")],
  ["several audiences without azp", (c) => (c.aud = ["krate-acme", "another-client"])],
]) {
  test(`refused: ${what}`, async () => {
    const { env, issuer } = await ssoAcme();
    issuer.tamper = tamper;
    issuer.signWith = signWith || "main";
    const r = await signIn(env);
    assert.strictEqual(r.error, "sso-failed", String(r.where));
    assert.strictEqual(r.where.searchParams.get("org"), "acme");
    assert.ok(!env._kv.has("ident:email:erin@corp.example"), "no account found, made or merged");
    assert.strictEqual((await auditOf(env)).events.filter((e) => e.action === "sso.join").length, 0);
  });
}

test("refused: an email the issuer has not verified", async () => {
  const { env, issuer } = await ssoAcme();
  issuer.person = { ...issuer.person, email_verified: false };
  assert.strictEqual((await signIn(env)).error, "sso-unverified-email");
  issuer.person = { ...issuer.person, email_verified: "true" };
  assert.strictEqual((await signIn(env)).error, "sso-unverified-email", "only the boolean true counts");
  assert.ok(!env._kv.has("ident:email:erin@corp.example"));
});

test("refused: an email off the verified domain", async () => {
  const { env, issuer } = await ssoAcme();
  for (const email of ["erin@other.example", "erin@sub.corp.example", "erin@corp.example.evil", "erin@xcorp.example"]) {
    issuer.person = { ...issuer.person, email };
    assert.strictEqual((await signIn(env)).error, "sso-wrong-domain", email);
    assert.ok(!env._kv.has(`ident:email:${email}`), email);
  }
});

test("refused: a domain that is not verified -- no account, no principal, no join", async () => {
  const { env, issuer } = await ssoAcme({ verify: false });
  // The issuer is the owner's choice, so it could say anything about
  // anyone: before the domain proof, nothing it says finds an account.
  issuer.person = { ...issuer.person, email: "dan@corp.example" };
  const r = await signIn(env);
  assert.strictEqual(r.error, "domain-not-verified");
  assert.ok(![...env._kv.keys()].some((k) => k.startsWith("ident:")), "no account made or merged");
  assert.strictEqual((await auditOf(env)).events.filter((e) => e.action === "sso.join").length, 0);
});

test("refused: auto-join as owner or admin, at the settings and again at the join", async () => {
  const issuer = await newIssuer();
  const env = ssoEnv(issuer);
  await acme(env);
  for (const role of ["owner", "admin", "superuser"]) {
    const r = await call(env, "alice", "PUT", "/orgs/acme/sso", { json: settings(issuer, { auto_join_role: role }) });
    assert.deepStrictEqual([r.status, r.body.error], [403, "role-cannot"], role);
  }
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme/sso")).body, null, "nothing was stored");
  // A stored value that somehow says owner still joins nobody as owner.
  await call(env, "alice", "PUT", "/orgs/acme/sso", { json: settings(issuer, { auto_join_role: "developer" }) });
  const t = await call(env, "alice", "GET", "/orgs/acme/sso/domain-token");
  issuer.txt.set(t.body.name, [t.body.value]);
  await call(env, "alice", "POST", "/orgs/acme/sso/verify-domain");
  env.ORGS.object("acme").ctx.storage.db.prepare("UPDATE sso SET auto_join_role = 'owner'").run();
  const r = await signIn(env);
  assert.strictEqual(r.hand.get("sso"), "no-auto-join");
  assert.strictEqual((await as(env, r.hand.get("token"), "GET", "/orgs/acme")).body.error, "not-a-member");
});

test("refused: a member an owner removed is not brought back by signing in", async () => {
  const { env } = await ssoAcme();
  const first = await signIn(env);
  assert.strictEqual(first.hand.get("sso"), "joined");
  assert.strictEqual((await call(env, "alice", "DELETE", `/orgs/acme/members/${encodeURIComponent("email:erin@corp.example")}`)).status, 204);
  const again = await signIn(env);
  assert.strictEqual(again.hand.get("sso"), "removed");
  assert.strictEqual((await as(env, again.hand.get("token"), "GET", "/orgs/acme")).body.error, "removed");
});

test("refused: a start with no browser nonce, an unknown organisation, a replayed callback", async () => {
  const { env, issuer } = await ssoAcme();
  assert.strictEqual((await signIn(env, { nonce: "" })).where.searchParams.get("error"), "sso-failed");
  assert.strictEqual((await signIn(env, { org: "nobody-here" })).where.searchParams.get("error"), "sso-not-set-up");
  // The hub refuses a second use of the same callback itself, even at an
  // issuer sloppy enough to take the same code twice.
  issuer.reusableCodes = true;
  const replay = {};
  const r = await signIn(env, { replay });
  assert.ok(r.hand);
  assert.strictEqual(new URL(replay.second).searchParams.get("error"), "sso-failed", "a callback works once");
});

test("enforce: a member at the domain who skips company sign-in gets sso-required", async () => {
  const { env, issuer } = await ssoAcme({ enforce: true });
  // erin proved her inbox by email link before (one account), then signs
  // in through SSO and joins: two sessions of the same account.
  await call(env, "erin@corp.example", "GET", "/orgs");
  env._kv.set("ident:email:erin@corp.example", "mail-erin-corp-example");
  const viaSso = await signIn(env);
  assert.strictEqual(env._kv.get(`session:${viaSso.hand.get("token")}`), "mail-erin-corp-example", "the same account");
  const ssoToken = viaSso.hand.get("token");
  assert.strictEqual((await as(env, ssoToken, "GET", "/orgs/acme")).status, 200);
  // A second session of the same account, from an email link: no SSO.
  env._kv.set("session:krs_emaillink", "mail-erin-corp-example");
  const skipped = await as(env, "krs_emaillink", "GET", "/orgs/acme");
  assert.deepStrictEqual([skipped.status, skipped.body.error], [403, "sso-required"]);
  assert.match(skipped.body.message, /login\/\?org=acme/);
  // On every route of that organisation, private downloads included.
  const releases = await as(env, "krs_emaillink", "GET", "/orgs/acme/releases");
  assert.strictEqual(releases.body.error, "sso-required");
  // Members not at the domain are untouched: alice, bob (GitHub), and an
  // address elsewhere.
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme")).status, 200);
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { email: "frank@partner.example", role: "recipient" } });
  await call(env, "frank@partner.example", "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  assert.strictEqual((await call(env, "frank@partner.example", "GET", "/orgs/acme")).status, 200);
  // The refusal is audited, and an SSO session for ANOTHER organisation
  // does not count here.
  const events = (await auditOf(env)).events;
  assert.ok(events.some((e) => e.actor === "email:erin@corp.example" && e.reason === "sso-required"));
  const other = await ssoOtherOrg(env, issuer);
  assert.strictEqual((await as(env, other, "GET", "/orgs/acme")).body.error, "sso-required");
  // Switched off, the email-link session works again.
  await call(env, "alice", "PUT", "/orgs/acme/sso", { json: settings(issuer, { auto_join_role: "developer", enforce: false }) });
  assert.strictEqual((await as(env, "krs_emaillink", "GET", "/orgs/acme")).status, 200);
});

/* A second organisation, beta, with the same issuer and domain: a session
 * signed in through beta's SSO. */
async function ssoOtherOrg(env, issuer) {
  await call(env, "carol", "POST", "/orgs", { json: { slug: "beta", name: "Beta" } });
  await call(env, "carol", "PUT", "/orgs/beta/sso", { json: settings(issuer, { auto_join_role: "recipient" }) });
  const t = await call(env, "carol", "GET", "/orgs/beta/sso/domain-token");
  issuer.txt.set(t.body.name, [...(issuer.txt.get(t.body.name) || []), t.body.value]);
  assert.strictEqual((await call(env, "carol", "POST", "/orgs/beta/sso/verify-domain")).status, 200);
  const r = await signIn(env, { org: "beta" });
  return r.hand.get("token");
}

test("enforce is not effective until the domain is verified", async () => {
  const { env } = await ssoAcme({ enforce: true, verify: false });
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { email: "erin@corp.example", role: "developer" } });
  await call(env, "erin@corp.example", "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  assert.strictEqual((await call(env, "erin@corp.example", "GET", "/orgs/acme")).status, 200);
});

test("enforce needs an owner who signs in another way, so a broken sign-in cannot lock every owner out", async () => {
  const issuer = await newIssuer();
  const env = ssoEnv(issuer);
  assert.strictEqual((await call(env, "olga@corp.example", "POST", "/orgs", { json: { slug: "solo", name: "Solo" } })).status, 201);
  const r = await call(env, "olga@corp.example", "PUT", "/orgs/solo/sso", { json: settings(issuer, { enforce: true }) });
  assert.deepStrictEqual([r.status, r.body.error], [409, "sso-needs-break-glass"]);
  const off = await call(env, "olga@corp.example", "PUT", "/orgs/solo/sso", { json: settings(issuer, { enforce: false }) });
  assert.strictEqual(off.status, 200);
});

test("an issuer must be https, and its discovery must name itself", async () => {
  const issuer = await newIssuer();
  const prod = orgEnv({ ORG_SECRET_KEY: "k" }); // PUBLIC_BASE is https://hub.example
  await acme(prod);
  const http = await call(prod, "alice", "PUT", "/orgs/acme/sso", { json: settings(issuer) });
  assert.deepStrictEqual([http.status, http.body.error], [400, "bad-request"], "plain http only for a local proof");
  for (const bad of ["ftp://idp.example", "https://user:pw@idp.example", "https://idp.example/?x=1", "not a url"]) {
    assert.strictEqual((await call(prod, "alice", "PUT", "/orgs/acme/sso", { json: settings(issuer, { issuer: bad }) })).status, 400, bad);
  }
  // Discovery that names a different issuer than the one configured (a
  // mix-up): refused, even though its tokens would say the right one.
  const mixed = await ssoAcme();
  mixed.issuer.discoveryIssuer = "https://someone-else.example";
  assert.strictEqual((await signIn(mixed.env)).error, "sso-failed");
  mixed.issuer.discoveryIssuer = null;
  assert.ok((await signIn(mixed.env)).hand);
  // The issuer configured is not the one discovery names: refused.
  const { env } = await ssoAcme();
  const t = await call(env, "alice", "PUT", "/orgs/acme/sso", { json: settings(issuer, { issuer: `${issuer.url}/` }) });
  assert.strictEqual(t.status, 200);
  assert.strictEqual((await signIn(env)).error, "sso-failed");
});

test("no ORG_SECRET_KEY: a secret cannot be stored", async () => {
  const issuer = await newIssuer();
  const env = ssoEnv(issuer, { ORG_SECRET_KEY: "" });
  await acme(env);
  const r = await call(env, "alice", "PUT", "/orgs/acme/sso", { json: settings(issuer) });
  assert.deepStrictEqual([r.status, r.body.error], [503, "sso-off"]);
});

/* ---- the ID token check on its own ------------------------------------- */

test("verifyIdToken accepts ES256, and checks azp against the client", async () => {
  const { privateKey, publicKey } = generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = { ...publicKey.export({ format: "jwk" }), kid: "ec-1", use: "sig" };
  const now = Math.floor(Date.now() / 1000);
  const make = (claims) => {
    const enc = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
    const input = `${enc({ alg: "ES256", kid: "ec-1" })}.${enc(claims)}`;
    const s = createSign("SHA256");
    s.update(input);
    return `${input}.${s.sign({ key: privateKey, dsaEncoding: "ieee-p1363" }).toString("base64url")}`;
  };
  const base = { iss: "https://idp.example", aud: "c1", sub: "s", nonce: "n", iat: now, exp: now + 60 };
  const opts = { jwks: { keys: [jwk] }, issuer: "https://idp.example", clientId: "c1", nonce: "n", now };
  assert.ok((await verifyIdToken(make(base), opts)).claims);
  assert.strictEqual((await verifyIdToken(make({ ...base, aud: ["c1", "c2"], azp: "c1" }), opts)).claims.sub, "s");
  assert.strictEqual((await verifyIdToken(make({ ...base, azp: "c2" }), opts)).problem, "wrong-audience");
  assert.strictEqual((await verifyIdToken(make({ ...base, nbf: now + 600 }), opts)).problem, "not-yet-valid");
  assert.strictEqual((await verifyIdToken(make({ ...base, exp: now - 61 }), opts)).problem, "expired");
  assert.ok((await verifyIdToken(make({ ...base, exp: now - 30 }), opts)).claims, "a minute of clock skew");
  assert.strictEqual((await verifyIdToken(make(base), { ...opts, jwks: { keys: [{ ...jwk, kid: "other" }] } })).problem, "no-matching-key");
  assert.strictEqual((await verifyIdToken("a.b", opts)).problem, "not-a-jwt");
  // Never `none`, never a shared-secret algorithm, whatever the header says.
  for (const alg of ["none", "HS256", "RS512", undefined]) {
    const enc = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
    const forged = `${enc({ alg, kid: "ec-1" })}.${enc(base)}.`;
    assert.strictEqual((await verifyIdToken(forged, opts)).problem, "alg-not-allowed", String(alg));
  }
});

test("the store itself joins nobody at a domain that is not verified", async () => {
  const { env } = await ssoAcme({ verify: false });
  const stub = env.ORGS.get(env.ORGS.idFromName("org:acme"));
  const ask = async (command) => (await stub.fetch("https://org/command", { method: "POST", body: JSON.stringify({ slug: "acme", ...command }) })).json();
  const r = await ask({ op: "sso-join", principals: ["email:erin@corp.example", "sso:acme"], args: { principal: "email:erin@corp.example" } });
  assert.deepStrictEqual(r.body, { joined: false, state: "off-domain" });
  assert.strictEqual(env.ORGS.object("acme").member("email:erin@corp.example"), null);
});
