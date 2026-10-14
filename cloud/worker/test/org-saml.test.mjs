/* Company sign-in by SAML 2.0 (IC-955): the real worker, the real Org store
 * and the real verifier (@node-saml/node-saml over xml-crypto), against a
 * fake identity provider on this machine (fake-saml.mjs) whose assertions
 * are signed with xml-crypto and a test key. No real provider is called.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-saml.test.mjs
 */
import { test, after } from "node:test";
import assert from "node:assert";
import { createServer } from "node:http";
import { orgEnv, call, acme, worker } from "./org-tools.mjs";
import { IDP, FORGER, IDP_ENTITY, requestOf, response } from "./fake-saml.mjs";

const HUB = "http://127.0.0.1:8787";
const IDP_SSO = "https://idp.corp.example/app/krate/sso/saml";

/* DNS over HTTPS for the domain proof: answers the TXT records a test set. */
const txt = new Map();
const doh = createServer((req, res) => {
  const u = new URL(req.url, "http://x");
  const values = txt.get(u.searchParams.get("name")) || [];
  res.writeHead(200, { "content-type": "application/dns-json" });
  res.end(JSON.stringify({ Status: 0, Answer: values.map((v) => ({ type: 16, data: `"${v}"` })) }));
});
await new Promise((ok) => doh.listen(0, "127.0.0.1", ok));
after(() => new Promise((ok) => doh.close(ok)));

async function samlAcme({ verify = true, autoJoin = "developer" } = {}) {
  const env = orgEnv({ PUBLIC_BASE: HUB, ORG_SECRET_KEY: "k", ORG_DOH_URL: `http://127.0.0.1:${doh.address().port}/dns-query` });
  await acme(env);
  const put = await call(env, "alice", "PUT", "/orgs/acme/saml", {
    json: { idp_entity_id: IDP_ENTITY, idp_sso_url: IDP_SSO, idp_cert: IDP.cert, domain: "corp.example" },
  });
  assert.strictEqual(put.status, 200, JSON.stringify(put.body));
  if (autoJoin) {
    const sso = await call(env, "alice", "PUT", "/orgs/acme/sso", { json: { domain: "corp.example", auto_join_role: autoJoin } });
    assert.strictEqual(sso.status, 200, JSON.stringify(sso.body));
  }
  if (verify) {
    const token = await call(env, "alice", "GET", "/orgs/acme/sso/domain-token");
    txt.set(token.body.name, [token.body.value]);
    const ok = await call(env, "alice", "POST", "/orgs/acme/sso/verify-domain");
    assert.strictEqual(ok.status, 200, JSON.stringify(ok.body));
  }
  return env;
}

const nonce = () => [...crypto.getRandomValues(new Uint8Array(16))].map((b) => b.toString(16).padStart(2, "0")).join("");

/* The browser's trip: the site's door, the hub's redirect to the IdP, and
 * the IdP's signed post back. `shape` changes what the IdP signs. */
async function signIn(env, shape = {}, { via = "oidc", before = null } = {}) {
  const n = nonce();
  const door = via === "oidc" ? "/login/oidc/start" : "/login/saml/start";
  let res = await worker.fetch(new Request(`${HUB}${door}?org=acme&nonce=${n}`), env);
  if (via === "oidc") {
    assert.strictEqual(res.status, 302);
    assert.strictEqual(new URL(res.headers.get("location")).pathname, "/login/saml/start", "the one door sends SAML organisations on");
    res = await worker.fetch(new Request(res.headers.get("location")), env);
  }
  const to = res.headers.get("location") || "";
  if (!to.startsWith(IDP_SSO)) return { error: new URL(to).searchParams.get("error") };
  const r = requestOf(to);
  const body = response({ email: "erin@corp.example", audience: r.issuer, recipient: r.acs, inResponseTo: r.id, ...(typeof shape === "function" ? shape(r) : shape) });
  const post = (b, relay = r.relayState) =>
    worker.fetch(
      new Request(r.acs, {
        method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({ SAMLResponse: b, RelayState: relay }).toString(),
      }),
      env,
    );
  if (before) await before();
  const back = await post(body);
  const where = new URL(back.headers.get("location"));
  const out = { request: r, body, post, nonce: n, problem: back.headers.get("x-krate-saml-problem") };
  if (where.pathname.startsWith("/login/done")) out.hand = new URLSearchParams(where.hash.slice(1));
  else out.error = where.searchParams.get("error");
  out.sso = where.searchParams.get("sso") || (where.hash.match(/sso=([^&]+)/) || [])[1];
  out.location = where.toString();
  return out;
}

const asToken = (env, token, method, path) =>
  worker.fetch(new Request(`${HUB}${path}`, { method, headers: { authorization: `Bearer ${token}` } }), env).then(async (r) => ({ status: r.status, body: await r.json().catch(() => null) }));

test("an owner sets SAML: the identity provider, its certificate fingerprinted, the SP's entity id and ACS URL to copy", async () => {
  const env = await samlAcme({ verify: false });
  const s = await call(env, "alice", "GET", "/orgs/acme/saml");
  assert.strictEqual(s.status, 200);
  assert.deepStrictEqual(s.body.sp, { entity_id: `${HUB}/saml/acme`, acs_url: `${HUB}/login/saml/acs/acme`, metadata_url: `${HUB}/saml/acme` });
  assert.strictEqual(s.body.idp.entity_id, IDP_ENTITY);
  assert.match(s.body.idp.certificates[0].sha256, /^[0-9a-f]{64}$/);
  assert.strictEqual(s.body.domain, "corp.example");
  assert.strictEqual(s.body.domain_verified, false);
  // The SP metadata an IdP's set-up page imports.
  const md = await worker.fetch(new Request(`${HUB}/saml/acme`), env);
  const xml = await md.text();
  assert.match(xml, /entityID="http:\/\/127\.0\.0\.1:8787\/saml\/acme"/);
  assert.match(xml, /Location="http:\/\/127\.0\.0\.1:8787\/login\/saml\/acs\/acme"/);
  // Only an owner; a certificate must be one.
  for (const who of ["bob", "carol", "dan"]) assert.strictEqual((await call(env, who, "PUT", "/orgs/acme/saml", { json: { idp_entity_id: "x", idp_sso_url: IDP_SSO, idp_cert: IDP.cert } })).status, 403, who);
  const bad = await call(env, "alice", "PUT", "/orgs/acme/saml", { json: { idp_entity_id: IDP_ENTITY, idp_sso_url: IDP_SSO, idp_cert: "not a certificate" } });
  assert.deepStrictEqual([bad.status, bad.body.error], [400, "bad-request"]);
  const http = await call(env, "alice", "PUT", "/orgs/acme/saml", { json: { idp_entity_id: IDP_ENTITY, idp_sso_url: "http://idp.corp.example/sso", idp_cert: IDP.cert } });
  assert.strictEqual(http.status, 400);
  // The bare base64 an IdP page shows is read as the same certificate.
  const bare = IDP.cert.replace(/-----[A-Z ]+-----/g, "").replace(/\s+/g, "");
  const again = await call(env, "alice", "PUT", "/orgs/acme/saml", { json: { idp_entity_id: IDP_ENTITY, idp_sso_url: IDP_SSO, idp_cert: bare } });
  assert.strictEqual(again.body.idp.certificates[0].sha256, s.body.idp.certificates[0].sha256);
  // A change with no certificate keeps the stored one.
  const kept = await call(env, "alice", "PUT", "/orgs/acme/saml", { json: { idp_entity_id: IDP_ENTITY, idp_sso_url: `${IDP_SSO}?v=2` } });
  assert.deepStrictEqual([kept.status, kept.body.idp.sso_url, kept.body.idp.certificates[0].sha256], [200, `${IDP_SSO}?v=2`, s.body.idp.certificates[0].sha256]);
  await call(env, "alice", "PUT", "/orgs/acme/saml", { json: { idp_entity_id: IDP_ENTITY, idp_sso_url: IDP_SSO } });
  // Nothing works before the domain is proved.
  assert.strictEqual((await signIn(env)).error, "domain-not-verified");
});

test("a signed assertion for this organisation signs the person in, as email:<address>, joining with the auto-join role", async () => {
  const env = await samlAcme();
  const r = await signIn(env);
  assert.ok(r.hand, `signed in: ${r.location} ${r.problem}`);
  assert.strictEqual(r.hand.get("nonce"), r.nonce, "the browser's K-909 nonce rides back");
  assert.strictEqual(r.sso, "joined");
  assert.match(r.request.xml, /AuthnRequest/);
  assert.strictEqual(r.request.issuer, `${HUB}/saml/acme`);
  const me = await asToken(env, r.hand.get("token"), "GET", "/orgs/acme");
  assert.strictEqual(me.status, 200, JSON.stringify(me.body));
  assert.deepStrictEqual([me.body.me.principal, me.body.me.role, me.body.me.added_by], ["email:erin@corp.example", "developer", "sso:acme"]);
  const ev = (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events.find((e) => e.action === "sso.join");
  assert.strictEqual(ev.actor, "email:erin@corp.example");
});

test("forged, tampered, unsigned, expired, wrong-audience, wrong-recipient, wrong-issuer and replayed assertions are each refused", async () => {
  const env = await samlAcme();
  const cases = [
    ["signed by another key", { signer: FORGER }, "bad-signature"],
    ["unsigned", { sign: false }, "bad-signature"],
    // The contract asks for a signed assertion: a signed envelope around an
    // unsigned one is not enough.
    ["unsigned assertion in a signed Response", { sign: false, signResponse: true }, "bad-signature"],
    ["changed after signing", { tamper: (x) => x.replace("erin@corp.example</saml:NameID>", "alice@corp.example</saml:NameID>") }, "bad-signature"],
    ["expired", { now: Date.now() - 2 * 3600_000 }, "expired"],
    ["not yet valid", { now: Date.now() + 2 * 3600_000 }, "expired"],
    ["for another organisation's SP", { audience: `${HUB}/saml/beta` }, "wrong-audience"],
    ["posted to another ACS", (r) => ({ recipient: r.acs.replace("/acme", "/beta") }), "wrong-recipient"],
    ["from another identity provider", { issuer: "http://evil.example/idp" }, "wrong-issuer"],
    ["at another domain", { email: "mallory@evil.example" }, null],
  ];
  for (const [label, shape, problem] of cases) {
    const r = await signIn(env, shape);
    assert.ok(!r.hand, `${label}: must not sign in`);
    assert.strictEqual(r.error, problem ? "sso-failed" : "sso-wrong-domain", label);
    if (problem) assert.strictEqual(r.problem, problem, label);
  }
  // A good one, then the same Response again: refused the second time, whether
  // posted with its own relay state or with a fresh sign-in's.
  const good = await signIn(env);
  assert.ok(good.hand);
  const again = await good.post(good.body);
  assert.strictEqual(new URL(again.headers.get("location")).searchParams.get("error"), "sso-failed");
  const fresh = await worker.fetch(new Request(`${HUB}/login/saml/start?org=acme&nonce=${nonce()}`), env);
  const relay = requestOf(fresh.headers.get("location")).relayState;
  const replay = await good.post(good.body, relay);
  assert.strictEqual(new URL(replay.headers.get("location")).searchParams.get("error"), "sso-failed");
  assert.ok(["unknown-request", "replayed"].includes(replay.headers.get("x-krate-saml-problem")), replay.headers.get("x-krate-saml-problem"));
  // The same assertion ID inside a new, validly signed Response is a replay.
  const id = "_a-reused-assertion-id";
  assert.ok((await signIn(env, { assertionId: id })).hand);
  const reused = await signIn(env, { assertionId: id });
  assert.deepStrictEqual([reused.error, reused.problem], ["sso-failed", "replayed"]);
  // An assertion answering a request this hub never made.
  const stray = await signIn(env, { inResponseTo: "_never-asked" });
  assert.deepStrictEqual([stray.error, stray.problem], ["sso-failed", "unknown-request"]);
});

test("a domain that stops being proved while a sign-in is at the identity provider refuses it at the ACS", async () => {
  const env = await samlAcme();
  // The owner changes the company domain while the person is at the
  // identity provider: the new domain starts unproved.
  const r = await signIn(env, {}, { before: () => call(env, "alice", "PUT", "/orgs/acme/sso", { json: { domain: "other.example" } }) });
  assert.strictEqual(r.error, "domain-not-verified");
});

test("someone removed is not brought back by signing in, and enforce counts a SAML sign-in as company sign-in", async () => {
  const env = await samlAcme();
  const first = await signIn(env);
  const token = first.hand.get("token");
  assert.strictEqual((await call(env, "alice", "PUT", "/orgs/acme/sso", { json: { domain: "corp.example", auto_join_role: "developer", enforce: true } })).status, 200);
  assert.strictEqual((await asToken(env, token, "GET", "/orgs/acme")).status, 200, "the SAML session counts");
  // An email-link session for the same inbox does not.
  assert.strictEqual((await call(env, "erin@corp.example", "GET", "/orgs/acme")).body.error, "sso-required");
  assert.strictEqual((await call(env, "alice", "DELETE", `/orgs/acme/members/${encodeURIComponent("email:erin@corp.example")}`)).status, 204);
  const back = await signIn(env);
  assert.strictEqual(back.sso, "removed");
  assert.strictEqual((await asToken(env, back.hand.get("token"), "GET", "/orgs/acme")).body.error, "removed");
});

test("switching SAML off sends the door back to OIDC (or says sign-in is not set up)", async () => {
  const env = await samlAcme();
  assert.strictEqual((await call(env, "alice", "DELETE", "/orgs/acme/saml")).status, 204);
  const res = await worker.fetch(new Request(`${HUB}/login/oidc/start?org=acme&nonce=${nonce()}`), env);
  assert.strictEqual(new URL(res.headers.get("location")).searchParams.get("error"), "sso-not-set-up");
  assert.strictEqual((await signIn(env, {}, { via: "saml" })).error, "sso-not-set-up");
});
