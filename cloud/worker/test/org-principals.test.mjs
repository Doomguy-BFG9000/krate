/* Principals (IC-947, Addendum 1 and 2): a member is `github:<login>` only
 * when GitHub vouched, or `email:<address>` only when the inbox was proved;
 * never a bare login. And what the dashboards need from the hub: GET /orgs,
 * CORS, join links, reading an invitation, the root key's provenance, and a
 * private release asked about in JSON or opened in a browser.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-principals.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { orgEnv, call, acme, fixture, KEYS, worker } from "./org-tools.mjs";

const auditCount = (env) => env.ORGS.object("acme").ctx.storage.db.prepare("SELECT COUNT(*) AS n FROM audit").get().n;

/* ---- an address is never a GitHub login ----------------------------- */

test("an email account whose address starts with a member's GitHub login is not that member", async () => {
  const env = orgEnv();
  await acme(env);
  // bob is github:bob, a reviewer. bob@evil.example signs in by email; its
  // session's `login` is "bob", exactly as a real one's would be.
  const lookalike = "bob@evil.example";
  const show = await call(env, lookalike, "GET", "/orgs/acme");
  assert.deepStrictEqual([show.status, show.body.error], [403, "not-a-member"]);
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  const approve = await call(env, lookalike, "POST", `/orgs/acme/candidates/${up.body.digest}/approve`);
  assert.strictEqual(approve.body.error, "not-a-member", "cannot approve as bob");
  assert.strictEqual((await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`)).status, 200);
  const fetch_ = await call(env, lookalike, "GET", `/a/${up.body.digest}`);
  assert.deepStrictEqual([fetch_.status, fetch_.body.error], [403, "not-a-member"], "cannot download as bob");
  // Nor accept an invitation for github:bob.
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: "frank", role: "developer" } });
  const steal = await call(env, "frank@evil.example", "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  assert.deepStrictEqual([steal.status, steal.body.error], [403, "invite-for-someone-else"]);
  // The audit names the address it really was.
  const events = (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events;
  assert.ok(events.some((e) => e.actor === "email:bob@evil.example" && e.result === "denied"));
  assert.ok(!events.some((e) => e.actor === "bob" || e.actor === "github:bob@evil.example"));
});

test("an email invitation is accepted only by a sign-in that proved that inbox", async () => {
  const env = orgEnv();
  await acme(env);
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { email: "Erin@Corp.example", role: "developer" } });
  assert.strictEqual(inv.status, 201);
  assert.deepStrictEqual([inv.body.principal, inv.body.kind, inv.body.display], ["email:erin@corp.example", "email", "erin@corp.example"]);
  for (const other of ["erin", "erin@other.example", "erin@corp.example.evil"]) {
    const r = await call(env, other, "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
    assert.deepStrictEqual([r.status, r.body.error], [403, "invite-for-someone-else"], other);
  }
  const ok = await call(env, "erin@corp.example", "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  assert.strictEqual(ok.status, 200, JSON.stringify(ok.body));
  assert.deepStrictEqual(
    { principal: ok.body.membership.principal, kind: ok.body.membership.kind, display: ok.body.membership.display, role: ok.body.membership.role },
    { principal: "email:erin@corp.example", kind: "email", display: "erin@corp.example", role: "developer" },
  );
  assert.ok(Number.isInteger(ok.body.membership.since));
  // She publishes as her email principal, and the record says so.
  const up = await call(env, "erin@corp.example", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  assert.strictEqual(up.status, 201);
  const history = (await call(env, "alice", "GET", "/orgs/acme/candidates")).body;
  assert.strictEqual(history[0].uploaded_by, "email:erin@corp.example");
  // Members are addressed by principal; a bare login names nobody.
  const bare = await call(env, "alice", "PUT", "/orgs/acme/members/erin", { json: { role: "reviewer" } });
  assert.deepStrictEqual([bare.status, bare.body.error], [403, "not-a-member"]);
  // Not even when a GitHub member has that login: "bob" is not github:bob.
  const bareBob = await call(env, "alice", "DELETE", "/orgs/acme/members/bob");
  assert.deepStrictEqual([bareBob.status, bareBob.body.error], [403, "not-a-member"]);
  assert.match(bareBob.body.message, /github:<login> or email:<address>/);
  assert.strictEqual((await call(env, "bob", "GET", "/orgs/acme")).status, 200, "bob is still a member");
  const moved = await call(env, "alice", "PUT", `/orgs/acme/members/${encodeURIComponent("email:erin@corp.example")}`, { json: { role: "reviewer" } });
  assert.strictEqual(moved.body.membership.role, "reviewer");
  assert.strictEqual((await call(env, "alice", "DELETE", `/orgs/acme/members/${encodeURIComponent("email:erin@corp.example")}`)).status, 204);
  assert.strictEqual((await call(env, "erin@corp.example", "GET", "/orgs/acme")).body.error, "removed");
});

test("an invitation names exactly one of github or email", async () => {
  const env = orgEnv();
  await acme(env);
  for (const body of [{ role: "developer" }, { github: "frank", email: "frank@corp.example", role: "developer" }, { github: "not a login", role: "developer" }, { email: "nope", role: "developer" }]) {
    const r = await call(env, "alice", "POST", "/orgs/acme/invites", { json: body });
    assert.deepStrictEqual([r.status, r.body.error], [403, "role-cannot"], JSON.stringify(body));
  }
});

test("a session that proved no inbox is treated as no session", async () => {
  const env = orgEnv();
  await acme(env);
  // An email account from before principals: no verified_emails yet.
  env._kv.set("session:krs_old", "uold");
  env._kv.set("user:uold", JSON.stringify({ id: "uold", login: "bob", email: "bob@old.example", providers: ["email"] }));
  const before = auditCount(env);
  const r = await worker.fetch(new Request("https://hub.example/orgs/acme", { headers: { authorization: "Bearer krs_old" } }), env);
  assert.strictEqual(r.status, 401);
  assert.strictEqual((await r.json()).error, "not-signed-in");
  assert.strictEqual(auditCount(env), before);
});

/* ---- what the dashboards read --------------------------------------- */

test("GET /orgs lists the signed-in person's organisations and follows joins, role changes and removal", async () => {
  const env = orgEnv();
  await acme(env);
  await call(env, "carol", "POST", "/orgs", { json: { slug: "carols", name: "Carol's" } });
  const carol = await call(env, "carol", "GET", "/orgs");
  assert.deepStrictEqual(carol.body, [
    { slug: "acme", name: "Acme", role: "developer" },
    { slug: "carols", name: "Carol's", role: "owner" },
  ]);
  await call(env, "alice", "PUT", "/orgs/acme/members/github%3Acarol", { json: { role: "reviewer" } });
  assert.strictEqual((await call(env, "carol", "GET", "/orgs")).body[0].role, "reviewer");
  await call(env, "alice", "DELETE", "/orgs/acme/members/github%3Acarol");
  assert.deepStrictEqual((await call(env, "carol", "GET", "/orgs")).body.map((o) => o.slug), ["carols"]);
  assert.deepStrictEqual((await call(env, "stranger", "GET", "/orgs")).body, []);
  assert.strictEqual((await call(env, null, "GET", "/orgs")).status, 401);
  // A forged index entry grants nothing: opening asks the store (R5).
  env._kv.set("orgs-of:github:stranger", JSON.stringify([{ slug: "acme", name: "Acme", role: "owner" }]));
  assert.strictEqual((await call(env, "stranger", "GET", "/orgs/acme")).body.error, "not-a-member");
});

test("every /orgs route answers with CORS, and a preflight allows the headers the dashboards send", async () => {
  const env = orgEnv();
  await acme(env);
  for (const [method, path] of [["GET", "/orgs"], ["GET", "/orgs/acme"], ["GET", "/orgs/acme/releases"], ["POST", "/orgs/acme/invites"]]) {
    const r = await call(env, "alice", method, path, method === "POST" ? { json: { github: "frank", role: "developer" } } : {});
    assert.strictEqual(r.headers.get("access-control-allow-origin"), "*", `${method} ${path}`);
  }
  const pre = await worker.fetch(
    new Request("https://hub.example/orgs/acme/members/github%3Abob", {
      method: "OPTIONS",
      headers: { origin: "https://krate.tech", "access-control-request-method": "PUT", "access-control-request-headers": "authorization, content-type, idempotency-key" },
    }),
    env,
  );
  assert.strictEqual(pre.status, 204);
  const allowed = pre.headers.get("access-control-allow-headers");
  for (const h of ["authorization", "content-type", "idempotency-key"]) assert.ok(allowed.includes(h), h);
  for (const m of ["GET", "POST", "PUT", "DELETE"]) assert.ok(pre.headers.get("access-control-allow-methods").includes(m), m);
});

test("every invite answer carries its join link, and the invited principal can read the invitation", async () => {
  const env = orgEnv();
  await acme(env);
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { email: "frank@corp.example", role: "reviewer" } });
  assert.strictEqual(inv.body.join_url, `https://krate.tech/join/?o=acme&c=${inv.body.code}`);
  const before = auditCount(env);
  const read = await call(env, "frank@corp.example", "GET", `/orgs/acme/invites/${inv.body.code}`);
  assert.strictEqual(read.status, 200);
  assert.strictEqual(read.headers.get("access-control-allow-origin"), "*");
  assert.deepStrictEqual(
    { org: read.body.org, role: read.body.role, invited_by: read.body.invited_by, for: read.body.for, state: read.body.state, expires_at: read.body.expires_at },
    {
      org: { slug: "acme", name: "Acme" },
      role: "reviewer",
      invited_by: { principal: "github:alice", display: "alice" },
      for: { principal: "email:frank@corp.example", display: "frank@corp.example", kind: "email" },
      state: "open",
      expires_at: inv.body.expires_at,
    },
  );
  // Anyone else: the reason, and nothing about the org beyond its slug.
  const other = await call(env, "stranger", "GET", `/orgs/acme/invites/${inv.body.code}`);
  assert.deepStrictEqual([other.status, other.body.error], [403, "invite-for-someone-else"]);
  assert.deepStrictEqual(Object.keys(other.body).sort(), ["error", "message"]);
  assert.ok(!JSON.stringify(other.body).includes("Acme") && !JSON.stringify(other.body).includes("reviewer") && !JSON.stringify(other.body).includes("alice"));
  assert.strictEqual((await call(env, null, "GET", `/orgs/acme/invites/${inv.body.code}`)).body.error, "not-signed-in");
  assert.strictEqual(auditCount(env), before, "reading an invitation changes nothing and is not audited");

  // The other refusals, for the person it names.
  await call(env, "frank@corp.example", "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  assert.strictEqual((await call(env, "frank@corp.example", "GET", `/orgs/acme/invites/${inv.body.code}`)).body.error, "invite-used");
  const gone = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: "erin", role: "developer" } });
  await call(env, "alice", "DELETE", `/orgs/acme/invites/${gone.body.code}`);
  assert.strictEqual((await call(env, "erin", "GET", `/orgs/acme/invites/${gone.body.code}`)).body.error, "invite-revoked");
  const late = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: "erin", role: "developer" } });
  env.ORGS.object("acme").now = () => late.body.expires_at + 1;
  assert.strictEqual((await call(env, "erin", "GET", `/orgs/acme/invites/${late.body.code}`)).body.error, "invite-expired");
});

test("GET /orgs/<slug> says when and by whom the root key was set", async () => {
  const env = orgEnv();
  await acme(env);
  const org = (await call(env, "bob", "GET", "/orgs/acme")).body.org;
  assert.strictEqual(org.root_key, KEYS.root);
  assert.strictEqual(org.root_key_set_by, "github:alice");
  assert.ok(Number.isInteger(org.root_key_set_at) && org.root_key_set_at > 1_700_000_000);
  const none = orgEnv();
  await acme(none, { root: null });
  const bare = (await call(none, "bob", "GET", "/orgs/acme")).body.org;
  assert.deepStrictEqual([bare.root_key, bare.root_key_set_at, bare.root_key_set_by], [null, null, null]);
});

/* ---- a private release, asked about and opened ----------------------- */

test("a private release asked for in JSON answers what it is, with no bytes; without that Accept it is the bytes", async () => {
  const env = orgEnv();
  await acme(env);
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`);
  const digest = up.body.digest;
  const before = auditCount(env);
  const look = await call(env, "dan", "GET", `/a/${digest}`, { headers: { accept: "application/json" } });
  assert.strictEqual(look.status, 200);
  assert.strictEqual(look.headers.get("content-type"), "application/json");
  assert.deepStrictEqual(look.body, { name: "Bounce", version: "1.0.0", size: fixture("candidate-v1.krate").length, org: { slug: "acme", name: "Acme" } });
  assert.strictEqual(auditCount(env), before, "a look is not a download");
  // The CLI and the dashboards' downloads send no such Accept: bytes.
  for (const accept of [undefined, "*/*", "application/octet-stream"]) {
    const bytes = await call(env, "dan", "GET", `/a/${digest}`, { headers: accept ? { accept } : {} });
    assert.strictEqual(bytes.status, 200);
    assert.deepStrictEqual(bytes.bytes, fixture("candidate-v1.krate"), String(accept));
  }
  // Refusals are unchanged, and a stranger's names no organisation (R8).
  const stranger = await call(env, "stranger", "GET", `/a/${digest}`, { headers: { accept: "application/json" } });
  assert.deepStrictEqual([stranger.status, stranger.body.error], [403, "not-a-member"]);
  assert.ok(!/acme/i.test(JSON.stringify(stranger.body)), JSON.stringify(stranger.body));
  const anon = await call(env, null, "GET", `/a/${digest}`, { headers: { accept: "application/json" } });
  assert.deepStrictEqual([anon.status, anon.body.error], [403, "not-signed-in"]);
  await call(env, "alice", "DELETE", "/orgs/acme/members/github%3Adan");
  const removed = await call(env, "dan", "GET", `/a/${digest}`, { headers: { accept: "application/json" } });
  assert.deepStrictEqual([removed.status, removed.body.error, removed.body.org], [403, "removed", "acme"]);
});

test("a browser asking for a private release is sent to the site's open page, and nothing else is said", async () => {
  const env = orgEnv();
  await acme(env);
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`);
  const browserUA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
  const asked = env.ORGS.requests;
  // A page navigation: Accept text/html, no Authorization.
  const nav = await call(env, null, "GET", `/a/${up.body.digest}`, {
    headers: { accept: "text/html,application/xhtml+xml,*/*;q=0.8", "user-agent": browserUA },
  });
  assert.strictEqual(nav.status, 303);
  assert.strictEqual(nav.headers.get("location"), `https://krate.tech/open/?a=${up.body.digest}&private=1`);
  assert.strictEqual(nav.headers.get("set-cookie"), null, "no cookies");
  assert.strictEqual(nav.bytes.length, 0);
  assert.strictEqual(env.ORGS.requests, asked, "the redirect asks no store and names no organisation");

  // The dashboards' "Open in Krate": fetch() from a browser tab, browser UA,
  // Accept */*, the person's session -- the bytes, never a redirect.
  const open = await call(env, "dan", "GET", `/a/${up.body.digest}`, { headers: { accept: "*/*", "user-agent": browserUA } });
  assert.strictEqual(open.status, 200);
  assert.deepStrictEqual(open.bytes, fixture("candidate-v1.krate"));
  // The open page's own call: session plus Accept: application/json -- what it is.
  const meta = await call(env, "dan", "GET", `/a/${up.body.digest}`, { headers: { accept: "application/json", "user-agent": browserUA } });
  assert.deepStrictEqual([meta.status, meta.body.name, meta.body.org.slug], [200, "Bounce", "acme"]);
  // A signed-in request that happens to accept HTML is answered, not redirected.
  const stranger = await call(env, "stranger", "GET", `/a/${up.body.digest}`, { headers: { accept: "text/html", "user-agent": browserUA } });
  assert.deepStrictEqual([stranger.status, stranger.body.error], [403, "not-a-member"]);
});

/* ---- Google ---------------------------------------------------------- */

function googleAnswers(claims) {
  const real = globalThis.fetch;
  globalThis.fetch = async (input, init) => {
    const url = String((input && input.url) || input);
    if (url === "https://oauth2.googleapis.com/token") return new Response(JSON.stringify({ id_token: "idt" }));
    if (url.startsWith("https://oauth2.googleapis.com/tokeninfo")) return new Response(JSON.stringify(claims));
    return real(input, init);
  };
  return () => (globalThis.fetch = real);
}

async function googleSignIn(env, claims) {
  const restore = googleAnswers({ aud: "gid", ...claims });
  try {
    env._kv.set("login:st", JSON.stringify({ from: "web", nonce: "", app_nonce: "" }));
    const res = await worker.fetch(new Request("https://hub.example/login/google/callback?state=st&code=c"), env);
    assert.strictEqual(res.status, 302);
    const hand = new URLSearchParams(new URL(res.headers.get("location")).hash.slice(1));
    const token = hand.get("token");
    const user = JSON.parse(env._kv.get(`user:${env._kv.get(`session:${token}`)}`));
    return { token, user };
  } finally {
    restore();
  }
}

test("Google sign-in merges and proves an email only when Google says it is verified", async () => {
  const env = orgEnv({ GOOGLE_CLIENT_ID: "gid", GOOGLE_CLIENT_SECRET: "gs" });
  await acme(env);
  // Someone already holds boss@corp.example (an email-link account).
  env._kv.set("ident:email:boss@corp.example", "uboss");
  env._kv.set("user:uboss", JSON.stringify({ id: "uboss", login: "boss", email: "boss@corp.example", providers: ["email"], verified_emails: ["boss@corp.example"] }));

  // A Google account claiming that address, unverified (string, as tokeninfo answers).
  const unverified = await googleSignIn(env, { sub: "g-1", email: "Boss@corp.example", email_verified: "false" });
  assert.notStrictEqual(unverified.user.id, "uboss", "merged into nobody");
  assert.ok(!(unverified.user.verified_emails || []).length, "proves no inbox");
  assert.strictEqual(unverified.user.email, "", "and is never the account's email");
  assert.strictEqual(env._kv.get("ident:email:boss@corp.example"), "uboss", "the address still belongs to its owner");
  const asOrg = await worker.fetch(new Request("https://hub.example/orgs", { headers: { authorization: `Bearer ${unverified.token}` } }), env);
  assert.strictEqual(asOrg.status, 401, "no principal at all");
  // An invitation for that address cannot be accepted by it.
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { email: "boss@corp.example", role: "recipient" } });
  const accept = await worker.fetch(
    new Request(`https://hub.example/orgs/acme/invites/${inv.body.code}/accept`, { method: "POST", headers: { authorization: `Bearer ${unverified.token}` } }),
    env,
  );
  assert.strictEqual(accept.status, 401);

  // Verified (boolean, as an ID token carries it): the same account, and the inbox is proved.
  const verified = await googleSignIn(env, { sub: "g-2", email: "boss@corp.example", email_verified: true });
  assert.strictEqual(verified.user.id, "uboss");
  assert.deepStrictEqual(verified.user.verified_emails, ["boss@corp.example"]);
  const ok = await worker.fetch(
    new Request(`https://hub.example/orgs/acme/invites/${inv.body.code}/accept`, { method: "POST", headers: { authorization: `Bearer ${verified.token}` } }),
    env,
  );
  assert.strictEqual(ok.status, 200);
  assert.strictEqual((await ok.json()).membership.principal, "email:boss@corp.example");

  // A new verified Google address is proved on its own account.
  const fresh = await googleSignIn(env, { sub: "g-3", email: "new@corp.example", email_verified: "true" });
  assert.deepStrictEqual(fresh.user.verified_emails, ["new@corp.example"]);
});

/* ---- test identities, both forms ------------------------------------ */

test("KRATE_TEST_IDENTITIES gives a GitHub principal for a login and an email principal for email:<address>", async () => {
  const env = orgEnv({
    PUBLIC_BASE: "http://127.0.0.1:8787",
    KRATE_TEST_IDENTITIES: "tok-alice=alice,tok-gh=github:bob,tok-dan@acme.example=email:dan@acme.example,tok-alice@corp.example=email:alice@corp.example",
  });
  const as = (token) => (path, init = {}) =>
    worker.fetch(new Request(`http://127.0.0.1:8787${path}`, { ...init, headers: { authorization: `Bearer ${token}`, "content-type": "application/json", ...(init.headers || {}) } }), env);
  const created = await (await as("tok-alice")("/orgs", { method: "POST", body: JSON.stringify({ slug: "acme" }) })).json();
  assert.strictEqual(created.membership.principal, "github:alice");
  const inv = await (await as("tok-alice")("/orgs/acme/invites", { method: "POST", body: JSON.stringify({ email: "dan@acme.example", role: "recipient" }) })).json();
  const joined = await (await as("tok-dan@acme.example")(`/orgs/acme/invites/${inv.code}/accept`, { method: "POST" })).json();
  assert.strictEqual(joined.membership.principal, "email:dan@acme.example");
  // alice@corp.example is an inbox, not the GitHub user alice.
  const lookalike = await as("tok-alice@corp.example")("/orgs/acme");
  assert.strictEqual((await lookalike.json()).error, "not-a-member");
  const bob = await (await as("tok-gh")("/orgs", { method: "POST", body: JSON.stringify({ slug: "bobs" }) })).json();
  assert.strictEqual(bob.membership.principal, "github:bob");
  // The guards hold for the email form too: off a loopback hub, nothing.
  const prod = orgEnv({ PUBLIC_BASE: "https://hub.krate.tech", KRATE_TEST_IDENTITIES: "tok-dan@acme.example=email:dan@acme.example" });
  const refused = await worker.fetch(new Request("https://hub.krate.tech/orgs", { headers: { authorization: "Bearer tok-dan@acme.example" } }), prod);
  assert.strictEqual(refused.status, 401);
});
