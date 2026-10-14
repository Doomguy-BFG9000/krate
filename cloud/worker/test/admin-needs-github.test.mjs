/* An email or Google sign-in is never an admin by its address (K-1085).
 *
 * KRATE_ADMINS names GitHub logins. An email or Google account's `login` is
 * only the part of its address before the @, which anyone can choose: an
 * email sign-in as boss@attacker.example got an account whose login was
 * "boss", and that session opened every /admin route on a hub whose admin
 * was the GitHub user boss. Only a login GitHub vouches for is an admin now.
 *
 * Driven through the real email sign-in and the real worker.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/admin-needs-github.test.mjs
 */
import assert from "node:assert";
import test from "node:test";
import worker from "../src/index.js";
import { r2Mock } from "./r2-mock.mjs";
import "./github-stub.mjs";

function env() {
  const kv = new Map();
  return {
    APPS: {
      get: async (k) => kv.get(k) ?? null,
      put: async (k, v) => { kv.set(k, v); },
      delete: async (k) => { kv.delete(k); },
      list: async ({ prefix }) => ({ keys: [...kv.keys()].filter((k) => k.startsWith(prefix)).map((name) => ({ name })), list_complete: true }),
    },
    BUNDLES: r2Mock(new Map()),
    PUBLIC_BASE: "https://hub.example",
    KRATE_ADMINS: "boss",
    _kv: kv,
  };
}
const call = (e, path, token, init = {}) =>
  worker.fetch(new Request(`https://hub.example${path}`, { ...init, headers: { authorization: `Bearer ${token}`, ...(init.headers || {}) } }), e, { waitUntil() {} });

/// Sign in by email as `address` the way a person does, and give back the session.
async function emailSignIn(e, address) {
  e._kv.set("email:t1", JSON.stringify({ email: address, from: "", nonce: "", app_nonce: "" }));
  const r = await worker.fetch(new Request("https://hub.example/login/email/verify?t=t1"), e, { waitUntil() {} });
  assert.strictEqual(r.status, 302);
  const session = [...e._kv.keys()].find((k) => k.startsWith("session:krs_"));
  return session.slice("session:".length);
}

test("an email session whose address starts with an admin's login is not that admin", async () => {
  const e = env();
  const token = await emailSignIn(e, "boss@attacker.example");
  const user = JSON.parse(e._kv.get(`user:${e._kv.get(`session:${token}`)}`));
  assert.strictEqual(user.login, "boss", "the fixture really carries the hazard: the account's login is the admin's");
  for (const path of ["/admin/scrub/last", "/admin/api/overview", "/admin/api/reports", "/admin/api/people?days=7"]) {
    const r = await call(e, path, token);
    assert.strictEqual(r.status, 404, `${path} must not open for an email session (got ${r.status})`);
  }
  const purge = await call(e, `/blob/${"a".repeat(64)}`, token, { method: "DELETE" });
  assert.strictEqual(purge.status, 404, "nor may it purge a bundle");
});

test("the admin's own GitHub token still opens the desk", async () => {
  const e = env();
  const r = await call(e, "/admin/scrub/last", "gho_boss");
  assert.notStrictEqual(r.status, 404, "the real admin is still an admin");
  const stranger = await call(e, "/admin/scrub/last", "gho_mallory");
  assert.strictEqual(stranger.status, 404);
});
