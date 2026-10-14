/* Stubbed sign-ins for the local organisation proof (IC-946).
 *
 * `wrangler dev` can be given KRATE_TEST_IDENTITIES so two or three people
 * can be signed in at once without real GitHub accounts. It must never
 * work anywhere else: absent, a test token goes to GitHub like any other
 * token and GitHub refuses it; present on a hub whose own address is not
 * this machine's loopback, it is ignored the same way.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-identity.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { orgEnv, worker } from "./org-tools.mjs";

const MAP = "tok-alice=alice,tok-bob=Bob";

/* GitHub, as far as these tests need it: it has never issued these tokens. */
function githubRefusesEverything() {
  const real = globalThis.fetch;
  const asked = [];
  globalThis.fetch = async (url, init) => {
    asked.push(String(url));
    if (String(url).startsWith("https://api.github.com/")) {
      return new Response(JSON.stringify({ message: "Bad credentials" }), { status: 401 });
    }
    return real(url, init);
  };
  return { asked, restore: () => (globalThis.fetch = real) };
}

const create = (env, token, slug) =>
  worker.fetch(
    new Request(`${env.PUBLIC_BASE}/orgs`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      body: JSON.stringify({ slug }),
    }),
    env,
  );

test("without KRATE_TEST_IDENTITIES a test token is just a token GitHub refuses", async () => {
  const gh = githubRefusesEverything();
  try {
    const env = orgEnv({ PUBLIC_BASE: "http://127.0.0.1:8787" });
    assert.strictEqual(env.KRATE_TEST_IDENTITIES, undefined);
    const res = await create(env, "tok-alice", "acme");
    assert.strictEqual(res.status, 401);
    assert.strictEqual((await res.json()).error, "not-signed-in");
    assert.ok(gh.asked.some((u) => u.startsWith("https://api.github.com/user")), "GitHub was asked, the map was not");
    assert.strictEqual(env.ORGS.objects.size, 0, "nothing was created");
  } finally {
    gh.restore();
  }
});

test("with KRATE_TEST_IDENTITIES on a loopback hub, each test token is its login", async () => {
  const gh = githubRefusesEverything();
  try {
    const env = orgEnv({ PUBLIC_BASE: "http://127.0.0.1:8787", KRATE_TEST_IDENTITIES: MAP });
    const res = await create(env, "tok-alice", "acme");
    assert.strictEqual(res.status, 201);
    const show = await worker.fetch(
      new Request("http://127.0.0.1:8787/orgs/acme", { headers: { authorization: "Bearer tok-alice" } }),
      env,
    );
    assert.strictEqual((await show.json()).me.principal, "github:alice");
    // Logins compare without case and are stored lower-case.
    const bob = await create(env, "tok-bob", "bobs");
    assert.strictEqual((await bob.json()).org.created_by, "github:bob");
    // A token not in the map still goes to GitHub.
    assert.strictEqual((await create(env, "tok-mallory", "mal")).status, 401);
    assert.ok(gh.asked.length >= 1);
  } finally {
    gh.restore();
  }
});

test("KRATE_TEST_IDENTITIES set on a hub that is not this machine grants nothing", async () => {
  const gh = githubRefusesEverything();
  try {
    for (const base of ["https://hub.krate.tech", "https://hub.example", "http://127.0.0.1.evil.example"]) {
      const env = orgEnv({ PUBLIC_BASE: base, KRATE_TEST_IDENTITIES: MAP });
      const res = await create(env, "tok-alice", "acme");
      assert.strictEqual(res.status, 401, base);
    }
  } finally {
    gh.restore();
  }
});

test("the deployed configuration never sets KRATE_TEST_IDENTITIES", async () => {
  const { readFileSync } = await import("node:fs");
  const toml = readFileSync(new URL("../wrangler.toml", import.meta.url), "utf8");
  const active = toml.split("\n").filter((line) => !line.trim().startsWith("#"));
  assert.ok(!active.some((line) => line.includes("KRATE_TEST_IDENTITIES")), "wrangler.toml would ship it");
});
