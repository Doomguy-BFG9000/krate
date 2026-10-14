/* The two small doors the organisation onboarding on krate.tech uses
 * (IC-958 B and C):
 *
 *   GET /orgs/<slug>/available            is this short name free (signed in)
 *   GET /orgs/<slug>/invites/<code>/door  how an open invitation's join page
 *                                         should offer sign-in (signed out)
 *
 * Neither writes to an organisation's activity: a person typing a name, or
 * opening an invitation before signing in, is not an attempt on anything.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-onboarding.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { orgEnv, call, acme } from "./org-tools.mjs";

const auditLength = async (env) => (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events.length;

test("a short name is checked live, without touching that organisation's activity", async () => {
  const env = orgEnv();
  await acme(env);
  const before = await auditLength(env);
  const taken = await call(env, "erin", "GET", "/orgs/acme/available");
  assert.deepStrictEqual([taken.status, taken.body], [200, { slug: "acme", available: false }]);
  const free = await call(env, "erin", "GET", "/orgs/globex/available");
  assert.deepStrictEqual([free.status, free.body], [200, { slug: "globex", available: true }]);
  // Asking created nothing: the name is still free to create.
  const made = await call(env, "erin", "POST", "/orgs", { json: { slug: "globex", name: "Globex" } });
  assert.strictEqual(made.status, 201);
  assert.strictEqual(await auditLength(env), before, "a stranger checking acme wrote nothing in acme's activity");
  // Signed out it is the usual refusal, and a bad name is said as such.
  assert.strictEqual((await call(env, null, "GET", "/orgs/acme/available")).body.error, "not-signed-in");
  assert.strictEqual((await call(env, "erin", "GET", "/orgs/A!/available")).body.error, "slug-invalid");
});

test("an open invitation tells its join page which sign-in to offer, and nothing else", async () => {
  const env = orgEnv();
  await acme(env);
  const byMail = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { email: "erin@corp.example", role: "recipient" } });
  const byGitHub = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: "frank", role: "developer" } });
  const before = await auditLength(env);

  const mail = await call(env, null, "GET", `/orgs/acme/invites/${byMail.body.code}/door`);
  assert.deepStrictEqual([mail.status, mail.body], [200, { name: "Acme", company_sign_in: false, kind: "email" }]);
  assert.match(mail.headers.get("cache-control") || "", /no-store/);
  const gh = await call(env, null, "GET", `/orgs/acme/invites/${byGitHub.body.code}/door`);
  assert.deepStrictEqual(gh.body, { name: "Acme", company_sign_in: false, kind: "github" });

  // A guessed code, a withdrawn one and an unknown organisation are one answer.
  const guessed = await call(env, null, "GET", "/orgs/acme/invites/not-a-code/door");
  await call(env, "alice", "DELETE", `/orgs/acme/invites/${byGitHub.body.code}`);
  const withdrawn = await call(env, null, "GET", `/orgs/acme/invites/${byGitHub.body.code}/door`);
  const nowhere = await call(env, null, "GET", `/orgs/globex/invites/${byMail.body.code}/door`);
  for (const r of [guessed, withdrawn, nowhere]) assert.deepStrictEqual([r.status, r.body.error], [404, "invite-revoked"]);
  // Beyond the name the email already carries, it names no inbox and no role.
  for (const r of [mail, gh]) assert.ok(!/erin|frank|recipient|developer/i.test(JSON.stringify(r.body)), JSON.stringify(r.body));
  // Withdrawing wrote its event; the door wrote none.
  assert.strictEqual(await auditLength(env), before + 1);
});

test("with company sign-in set up, the door says so", async () => {
  const env = orgEnv({ ORG_SECRET_KEY: "test-only" });
  await acme(env);
  const set = await call(env, "alice", "PUT", "/orgs/acme/sso", {
    json: { issuer: "https://accounts.google.com", client_id: "cid", client_secret: "sec", domain: "corp.example" },
  });
  assert.ok(set.status < 300, JSON.stringify(set.body));
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { email: "erin@corp.example", role: "recipient" } });
  const door = await call(env, null, "GET", `/orgs/acme/invites/${inv.body.code}/door`);
  assert.deepStrictEqual(door.body, { name: "Acme", company_sign_in: true, kind: "email" });
});
