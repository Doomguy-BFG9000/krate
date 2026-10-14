/* An organisation's releases (IC-946): signed for the organisation,
 * approved by a second person against the exact digest, downloadable only
 * by a current member or recipient asked on every fetch, and kept out of
 * every public surface the hub has.
 *
 * The signed bundles are real, made by crates/bundle's own signing
 * (crates/validator-wasm/tests/org_fixtures.rs), and judged here by the
 * real validator compiled to WebAssembly.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-release.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { orgEnv, call, acme, fixture, KEYS, REVOCATIONS, worker } from "./org-tools.mjs";
import { sha256Hex } from "./r2-mock.mjs";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const BOUNCE = new Uint8Array(readFileSync(join(here, "..", "..", "..", "evidence", "ported", "bounce.krate")));

async function released(env, name = "candidate-v1.krate") {
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(name) });
  assert.strictEqual(up.status, 201, JSON.stringify(up.body));
  const ok = await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`);
  assert.strictEqual(ok.status, 200, JSON.stringify(ok.body));
  return up.body.digest;
}

/* ---- the signature chain -------------------------------------------- */

test("a candidate must be signed by the organisation's root or a key it delegated", async () => {
  const env = orgEnv();
  await acme(env);
  for (const [name, accepted] of [
    ["candidate-v1.krate", true],
    ["root-signed.krate", true],
    ["wrong-root.krate", false],
    ["forged-slip.krate", false],
    ["expired.krate", false],
    ["revoked.krate", false],
  ]) {
    const r = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(name) });
    if (accepted) {
      assert.strictEqual(r.status, 201, `${name}: ${JSON.stringify(r.body)}`);
      assert.strictEqual(r.body.state, "waiting-for-approval");
      assert.strictEqual(r.body.digest, await sha256Hex(fixture(name)), "bound to its sha256 (R7)");
    } else {
      assert.deepStrictEqual([r.status, r.body.error], [422, "not-signed-for-this-org"], name);
    }
  }
  const unsigned = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: BOUNCE });
  assert.deepStrictEqual([unsigned.status, unsigned.body.error], [422, "not-signed-for-this-org"], "unsigned");
  // Refused before a byte was stored.
  for (const name of ["wrong-root.krate", "forged-slip.krate", "expired.krate", "revoked.krate"]) {
    assert.strictEqual(await env.BUNDLES.head(await sha256Hex(fixture(name))), null, `${name} not stored`);
  }
  const why = async (name) => (await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(name) })).body.message;
  assert.match(await why("wrong-root.krate"), /different root/);
  assert.match(await why("forged-slip.krate"), /not signed by the publisher it names/);
  assert.match(await why("expired.krate"), /ran out/);
  assert.match(await why("revoked.krate"), /withdrawn/);
});

test("with no root key registered, nothing is signed for the organisation", async () => {
  const env = orgEnv();
  await acme(env, { root: null });
  const r = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  assert.deepStrictEqual([r.status, r.body.error], [422, "not-signed-for-this-org"]);
  assert.match(r.body.message, /no root key yet/);
});

test("a slip that runs out by the time of upload refuses, whatever time the bundle claims", async () => {
  const env = orgEnv();
  await acme(env);
  env.ORGS.object("acme").now = () => KEYS.slip_until + 1;
  const r = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  assert.deepStrictEqual([r.status, r.body.error], [422, "not-signed-for-this-org"]);
  assert.match(r.body.message, /run out/);
});

test("revocations come from a list the root signed; a list from anyone else is refused", async () => {
  const env = orgEnv();
  await acme(env, { revocations: null });
  // Before the list: the stolen key's slip is genuine, so it is accepted.
  const before = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("revoked.krate") });
  assert.strictEqual(before.status, 201);

  const edited = JSON.parse(JSON.stringify(REVOCATIONS));
  edited.list.revocations[0].key = KEYS.release;
  const forged = await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: KEYS.root, revocations: edited } });
  assert.deepStrictEqual([forged.status, forged.body.error], [422, "not-signed-for-this-org"]);
  assert.match(forged.body.message, /altered or forged/);
  const other = await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: KEYS.other_root, revocations: REVOCATIONS } });
  assert.deepStrictEqual([other.status, other.body.error], [422, "not-signed-for-this-org"]);

  const good = await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: KEYS.root, revocations: REVOCATIONS } });
  assert.strictEqual(good.status, 200);
  assert.strictEqual(good.body.key.revoked_keys, 1);
  const after = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("revoked.krate") });
  assert.deepStrictEqual([after.status, after.body.error], [422, "not-signed-for-this-org"], "sent again, refused");
  const approve = await call(env, "bob", "POST", `/orgs/acme/candidates/${before.body.digest}/approve`);
  assert.deepStrictEqual([approve.status, approve.body.error], [422, "not-signed-for-this-org"], "and what it signed before is not released");
  const history = (await call(env, "alice", "GET", "/orgs/acme/candidates")).body;
  assert.strictEqual(history.find((c) => c.digest === before.body.digest).state, "waiting-for-approval", "the record stays");
  const v2 = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  assert.strictEqual(v2.status, 201, "an unrevoked key is unaffected");
});

test("rotating the root is recorded, and from then on the new root decides", async () => {
  const env = orgEnv();
  await acme(env);
  const rotated = await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: KEYS.other_root } });
  assert.strictEqual(rotated.status, 200);
  assert.strictEqual(rotated.body.key.previous, KEYS.root);
  const old = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  assert.strictEqual(old.body.error, "not-signed-for-this-org");
  const fresh = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("wrong-root.krate") });
  assert.strictEqual(fresh.status, 201, "the other root's delegated key is acme's now");
  const audit = (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events.map((e) => e.action);
  assert.ok(audit.includes("root-key.register") && audit.includes("root-key.rotate"));
});

/* ---- approval is bound to the exact digest --------------------------- */

test("approving X releases X; changed bytes are a new candidate that approving X does not touch", async () => {
  const env = orgEnv();
  await acme(env);
  const x = await released(env, "candidate-v1.krate");
  const y = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v2.krate") });
  assert.strictEqual(y.body.state, "waiting-for-approval");
  assert.notStrictEqual(y.body.digest, x);
  // Approving X again changes nothing about Y.
  const again = await call(env, "bob", "POST", `/orgs/acme/candidates/${x}/approve`);
  assert.strictEqual(again.body.release.digest, x);
  const releases = (await call(env, "dan", "GET", "/orgs/acme/releases")).body;
  assert.deepStrictEqual(releases.map((r) => r.digest), [x], "only X is released");
  const yFetch = await call(env, "dan", "GET", `/a/${y.body.digest}`);
  assert.deepStrictEqual([yFetch.status, yFetch.body.error], [403, "digest-mismatch"], "a candidate is never served");
  const unknown = await call(env, "bob", "POST", `/orgs/acme/candidates/${"0".repeat(64)}/approve`);
  assert.deepStrictEqual([unknown.status, unknown.body.error], [404, "digest-mismatch"]);
  const junk = await call(env, "bob", "POST", "/orgs/acme/candidates/not-a-digest/approve");
  assert.strictEqual(junk.body.error, "digest-mismatch");
  const [release] = releases;
  assert.deepStrictEqual(
    { name: release.name, version: release.version, uploaded_by: release.uploaded_by, approved_by: release.approved_by },
    { name: "Bounce", version: "1.0.0", uploaded_by: "github:carol", approved_by: "github:bob" },
  );
  assert.strictEqual(release.url, `https://hub.example/a/${x}`);
});

/* ---- the private fetch ----------------------------------------------- */

test("a private release downloads only for a current member or recipient, asked on every fetch", async () => {
  const env = orgEnv();
  await acme(env);
  const digest = await released(env);
  const bytes = fixture("candidate-v1.krate");

  const dan = await call(env, "dan", "GET", `/a/${digest}`);
  assert.strictEqual(dan.status, 200);
  assert.deepStrictEqual(dan.bytes, bytes, "the exact bytes");
  assert.strictEqual(dan.headers.get("cache-control"), "private, no-store");
  assert.match(dan.headers.get("content-disposition"), /bounce\.krate/);

  const asked = env.ORGS.requests;
  for (const [who, reason] of [[null, "not-signed-in"], ["stranger", "not-a-member"]]) {
    for (const method of ["GET", "HEAD"]) {
      const r = await call(env, who, method, `/a/${digest}`);
      assert.strictEqual(r.status, 403, `${who} ${method}`);
      if (method === "GET") assert.strictEqual(r.body.error, reason);
      assert.strictEqual(r.bytes.length === 0 || r.body.error === reason, true, "no bytes in a refusal");
    }
    // A page navigation (no session) goes to the site's open page (Addendum 2, item 5); a signed-in request is answered.
    const browser = await call(env, who, "GET", `/a/${digest}`, { headers: { accept: "text/html" } });
    assert.deepStrictEqual(who ? [browser.status, browser.body.error] : [browser.status, browser.headers.get("location")], who ? [403, reason] : [303, `https://krate.tech/open/?a=${digest}&private=1`]);
  }
  assert.strictEqual(
    env.ORGS.requests - asked,
    3,
    "the organisation was asked on every signed-in fetch; no session is answered at the door",
  );

  // Removed: refused at the very next fetch, no session or cache in between.
  assert.strictEqual((await call(env, "alice", "DELETE", "/orgs/acme/members/github%3Adan")).status, 204);
  const gone = await call(env, "dan", "GET", `/a/${digest}`);
  assert.deepStrictEqual([gone.status, gone.body.error], [403, "removed"]);
  // The removed member is told which organisation; a stranger is not.
  assert.strictEqual(gone.body.org, "acme");
  const stranger = await call(env, "erin", "GET", `/a/${digest}`);
  assert.strictEqual(stranger.body.org, undefined, "a stranger holding the link learns no organisation name");
  // A short link or a ?dl=1 is no way round it.
  assert.strictEqual((await call(env, null, "GET", `/a/${digest}?dl=1`)).status, 403);
  assert.strictEqual((await call(env, "dan", "GET", `/a/${digest.slice(0, 12)}`)).status, 404, "no alias is minted for a private release");
});

test("removing a member keeps the organisation's releases, their history, and everything personal", async () => {
  const env = orgEnv();
  await acme(env);
  // carol's own public app, outside the organisation.
  const personal = await worker.fetch(
    new Request("https://hub.example/publish", {
      method: "POST",
      headers: { authorization: "Bearer krs_carol", "x-krate-name": "Carol Notes" },
      body: BOUNCE,
    }),
    env,
  );
  assert.strictEqual(personal.status, 200, await personal.clone().text());
  const personalId = (await personal.json()).id;
  const x = await released(env);
  const y = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v2.krate") });

  assert.strictEqual((await call(env, "alice", "DELETE", "/orgs/acme/members/github%3Acarol")).status, 204);
  const releases = (await call(env, "bob", "GET", "/orgs/acme/releases")).body;
  assert.deepStrictEqual(releases.map((r) => [r.digest, r.uploaded_by, r.approved_by]), [[x, "github:carol", "github:bob"]], "history as it was (R12)");
  const history = (await call(env, "alice", "GET", "/orgs/acme/candidates")).body;
  assert.deepStrictEqual(
    history.map((c) => [c.digest, c.state, c.uploaded_by]),
    [
      [x, "released", "github:carol"],
      [y.body.digest, "waiting-for-approval", "github:carol"],
    ],
  );
  assert.strictEqual((await call(env, "dan", "GET", `/a/${x}`)).status, 200, "the release still serves its recipients");
  const own = await call(env, null, "GET", `/a/${personalId}?dl=1`);
  assert.strictEqual(own.status, 200, "her personal app is untouched (R3, R10)");
  assert.ok(env._kv.get(`app:${personalId}`), "and still listed");
  const refused = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("root-signed.krate") });
  assert.strictEqual(refused.body.error, "removed");
});

/* ---- never public ----------------------------------------------------- */

test("a private release and its candidates never appear on any public surface", async () => {
  const env = orgEnv();
  await acme(env);
  // A public app by the same developer, so each surface is shown to work.
  const pub = await worker.fetch(
    new Request("https://hub.example/publish", {
      method: "POST",
      headers: { authorization: "Bearer krs_carol", "x-krate-name": "Carol Notes" },
      body: BOUNCE,
    }),
    env,
  );
  const publicId = (await pub.json()).id;
  const x = await released(env);
  const y = (await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v2.krate") })).body.digest;
  const html = { accept: "text/html" };
  const surfaces = [
    ["https://hub.example/apps", {}],
    ["https://hub.example/apps?q=bounce", {}],
    ["https://hub.example/apps?q=carol", {}],
    ["https://hub.example/apps?cat=apps", {}],
    [`https://hub.example/meta/${x}`, {}],
    [`https://hub.example/meta/${x.slice(0, 12)}`, {}],
    [`https://hub.example/meta/${y}`, {}],
    ["https://carol.hub.example/", html],
    ["https://carol.hub.example/bounce", html],
    ["https://hub.example/c/carol/bounce", {}],
    ["https://hub.example/my/apps", { authorization: "Bearer krs_carol" }],
    [`https://hub.example/shot/${x}`, {}],
    [`https://hub.example/icon/${x}`, {}],
    ["https://hub.example/takedowns", {}],
  ];
  let sawPublic = 0;
  for (const [url, headers] of surfaces) {
    const res = await worker.fetch(new Request(url, { headers }), env);
    const body = await res.text();
    for (const digest of [x, y]) {
      assert.ok(!body.includes(digest) && !body.includes(digest.slice(0, 12)), `${url} names a private digest`);
    }
    assert.ok(!/acme\/bounce/.test(body), `${url} names the private namespace`);
    if (body.includes(publicId)) sawPublic += 1;
  }
  assert.ok(sawPublic >= 4, `the public app shows where it should (${sawPublic} surfaces)`);
  // Nothing private was ever written to the KV the public surfaces read.
  for (const key of env._kv.keys()) {
    assert.ok(!key.includes(x) && !key.includes(y), `KV ${key}`);
  }
});

test("the public door refuses an organisation's bytes, and an organisation refuses public bytes", async () => {
  const env = orgEnv();
  await acme(env);
  const x = await released(env);
  const pub = await worker.fetch(
    new Request("https://hub.example/publish", {
      method: "POST",
      headers: { authorization: "Bearer krs_stranger", "x-krate-name": "Copied" },
      body: fixture("candidate-v1.krate"),
    }),
    env,
  );
  // Addendum 5: the bytes carry acme's mark, so the public door turns
  // them back before it asks who holds them.
  assert.strictEqual(pub.status, 422, "a copy of the bytes cannot be made public");
  const said = await pub.json();
  assert.strictEqual(said.error, "private-app");
  assert.strictEqual(said.message, "this app is private to acme: publish it with krate publish --org acme");
  assert.strictEqual(env._kv.get(`app:${x}`), undefined);
  assert.strictEqual((await call(env, "stranger", "GET", `/a/${x}`)).status, 403, "and is still private");

  await worker.fetch(
    new Request("https://hub.example/publish", {
      method: "POST",
      headers: { authorization: "Bearer krs_carol", "x-krate-name": "Bounce" },
      body: BOUNCE,
    }),
    env,
  );
  // Bytes already public cannot become a private release: they are out.
  await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: KEYS.root } });
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: "frank", role: "developer" } });
  await call(env, "frank", "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  // A public listing of a signed bundle, then the same bytes to the org.
  // A public app is unmarked (Addendum 5), so the organisation refuses it
  // as not marked for it, before it asks who holds the bytes.
  const signedPublic = await worker.fetch(
    new Request("https://hub.example/publish", {
      method: "POST",
      headers: { authorization: "Bearer krs_frank", "x-krate-name": "Frank" },
      body: fixture("unmarked.krate"),
    }),
    env,
  );
  assert.strictEqual(signedPublic.status, 200);
  const held = await call(env, "frank", "POST", "/orgs/acme/candidates", { bytes: fixture("unmarked.krate") });
  assert.deepStrictEqual([held.status, held.body.error], [422, "not-marked-for-this-org"]);
});

/* ---- the mark (Addendum 5) ------------------------------------------- */

test("a candidate must carry its organisation's mark under a valid signature", async () => {
  const env = orgEnv();
  await acme(env);
  const unmarked = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("unmarked.krate") });
  assert.deepStrictEqual([unmarked.status, unmarked.body.error], [422, "not-marked-for-this-org"]);
  assert.match(unmarked.body.message, /krate sign app\.krate --org acme/);
  const other = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("marked-globex.krate") });
  assert.deepStrictEqual([other.status, other.body.error], [422, "not-marked-for-this-org"]);
  assert.match(other.body.message, /marked as globex's private app, not acme's/);
  // Neither is stored, and the refusals are in the audit.
  for (const name of ["unmarked.krate", "marked-globex.krate"]) {
    assert.strictEqual(await env.BUNDLES.head(await sha256Hex(fixture(name))), null, `${name} not stored`);
  }
  const audit = await call(env, "alice", "GET", "/orgs/acme/audit");
  const denied = audit.body.events.filter((e) => e.reason === "not-marked-for-this-org");
  assert.strictEqual(denied.length, 2);
  // The marked one is taken.
  const marked = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  assert.strictEqual(marked.status, 201);
});

/* ---- the big-file path ------------------------------------------------ */

const SECRET = "test-builder-secret";
const bigReq = (path, init) => new Request(`https://hub.example${path}`, init);

test("an organisation candidate through the big-file path lands as a candidate, private, and releases the same way", async () => {
  const env = orgEnv({ KRATE_BUILDER_SECRET: SECRET, BIG_PUBLISHERS: "carol,stranger", BIG_PART_BYTES: String(5 * 1024 * 1024) });
  await acme(env);
  const bytes = fixture("candidate-v1.krate");
  const sha = await sha256Hex(bytes);
  const start = async (who) =>
    worker.fetch(
      bigReq("/publish/big/start", {
        method: "POST",
        headers: { authorization: `Bearer gho_${who}`, "content-type": "application/json" },
        body: JSON.stringify({ kind: "bundle", size: bytes.byteLength, sha256: sha, org: "acme" }),
      }),
      env,
    );
  const strangerStart = await start("stranger");
  assert.strictEqual(strangerStart.status, 403);
  assert.strictEqual((await strangerStart.json()).error, "not-a-member", "asked before a part is accepted");

  const started = await start("carol");
  assert.strictEqual(started.status, 200, await started.clone().text());
  const up = await started.json();
  const part = await worker.fetch(
    bigReq(`/publish/big/${up.id}/part/1`, {
      method: "PUT",
      headers: { "x-krate-upload": up.token, "content-length": String(bytes.byteLength) },
      body: bytes,
    }),
    env,
  );
  const { etag } = await part.json();
  const fin = await worker.fetch(
    bigReq(`/publish/big/${up.id}/finish`, {
      method: "POST",
      headers: { "x-krate-upload": up.token, "content-type": "application/json" },
      body: JSON.stringify({ parts: [{ part: 1, etag }] }),
    }),
    env,
  );
  assert.strictEqual(fin.status, 202);
  // What the build service would answer: the validator's judgement.
  const { judgeBundle } = await import("../src/index.js");
  const judged = judgeBundle(bytes);
  const verdict = await worker.fetch(
    bigReq(`/internal/big/${up.id}/verdict`, {
      method: "POST",
      headers: { "x-krate-builder": SECRET, "content-type": "application/json" },
      body: JSON.stringify({ ok: true, sha256: sha, judgement: judged.judgement }),
    }),
    env,
  );
  const status = await verdict.json();
  assert.deepStrictEqual([status.status, status.digest, status.state, status.org], ["candidate", sha, "waiting-for-approval", "acme"]);
  assert.strictEqual(env._kv.get(`app:${sha}`), undefined, "no listing");
  assert.strictEqual((await call(env, "dan", "GET", `/a/${sha}`)).status, 403, "not served as a candidate");

  assert.strictEqual((await call(env, "bob", "POST", `/orgs/acme/candidates/${sha}/approve`)).status, 200);
  const dan = await call(env, "dan", "GET", `/a/${sha}`);
  assert.strictEqual(dan.status, 200);
  assert.strictEqual(dan.headers.get("x-krate-tier"), "big");
  assert.deepStrictEqual(dan.bytes, bytes);
  assert.strictEqual((await call(env, "stranger", "GET", `/a/${sha}`)).status, 403);
  assert.strictEqual((await call(env, null, "GET", `/a/${sha}`)).body.error, "not-signed-in");
});

test("a big upload signed for another root is refused at the verdict and leaves nothing", async () => {
  const env = orgEnv({ KRATE_BUILDER_SECRET: SECRET, BIG_PUBLISHERS: "carol", BIG_PART_BYTES: String(5 * 1024 * 1024) });
  await acme(env);
  const bytes = fixture("wrong-root.krate");
  const sha = await sha256Hex(bytes);
  const started = await worker.fetch(
    bigReq("/publish/big/start", {
      method: "POST",
      headers: { authorization: "Bearer gho_carol", "content-type": "application/json" },
      body: JSON.stringify({ kind: "bundle", size: bytes.byteLength, sha256: sha, org: "acme" }),
    }),
    env,
  );
  const up = await started.json();
  const part = await worker.fetch(
    bigReq(`/publish/big/${up.id}/part/1`, {
      method: "PUT",
      headers: { "x-krate-upload": up.token, "content-length": String(bytes.byteLength) },
      body: bytes,
    }),
    env,
  );
  const { etag } = await part.json();
  await worker.fetch(
    bigReq(`/publish/big/${up.id}/finish`, {
      method: "POST",
      headers: { "x-krate-upload": up.token, "content-type": "application/json" },
      body: JSON.stringify({ parts: [{ part: 1, etag }] }),
    }),
    env,
  );
  const { judgeBundle } = await import("../src/index.js");
  const verdict = await worker.fetch(
    bigReq(`/internal/big/${up.id}/verdict`, {
      method: "POST",
      headers: { "x-krate-builder": SECRET, "content-type": "application/json" },
      body: JSON.stringify({ ok: true, sha256: sha, judgement: judgeBundle(bytes).judgement }),
    }),
    env,
  );
  const status = await verdict.json();
  assert.strictEqual(status.status, "refused");
  assert.match(status.problem, /not-signed-for-this-org/);
  assert.strictEqual(env._kv.get(`bigkey:${sha}`), undefined);
  assert.strictEqual(await env.BUNDLES.head(`big/${up.id}`), null);
});

/// One big upload, start to verdict, answering the verdict's status.
async function bigVerdict(env, who, bytes, org) {
  const sha = await sha256Hex(bytes);
  const ask = { kind: "bundle", size: bytes.byteLength, sha256: sha, name: "Bounce" };
  if (org) ask.org = org;
  const started = await worker.fetch(
    bigReq("/publish/big/start", {
      method: "POST",
      headers: { authorization: `Bearer gho_${who}`, "content-type": "application/json" },
      body: JSON.stringify(ask),
    }),
    env,
  );
  const up = await started.json();
  const part = await worker.fetch(
    bigReq(`/publish/big/${up.id}/part/1`, {
      method: "PUT",
      headers: { "x-krate-upload": up.token, "content-length": String(bytes.byteLength) },
      body: bytes,
    }),
    env,
  );
  const { etag } = await part.json();
  await worker.fetch(
    bigReq(`/publish/big/${up.id}/finish`, {
      method: "POST",
      headers: { "x-krate-upload": up.token, "content-type": "application/json" },
      body: JSON.stringify({ parts: [{ part: 1, etag }] }),
    }),
    env,
  );
  const { judgeBundle } = await import("../src/index.js");
  const verdict = await worker.fetch(
    bigReq(`/internal/big/${up.id}/verdict`, {
      method: "POST",
      headers: { "x-krate-builder": SECRET, "content-type": "application/json" },
      body: JSON.stringify({ ok: true, sha256: sha, judgement: judgeBundle(bytes).judgement }),
    }),
    env,
  );
  return { status: await verdict.json(), sha, id: up.id };
}

test("the big-file path holds the mark too: no unmarked candidate, no marked public app", async () => {
  const env = orgEnv({ KRATE_BUILDER_SECRET: SECRET, BIG_PUBLISHERS: "carol", BIG_PART_BYTES: String(5 * 1024 * 1024) });
  await acme(env);
  const unmarked = await bigVerdict(env, "carol", fixture("unmarked.krate"), "acme");
  assert.strictEqual(unmarked.status.status, "refused");
  assert.match(unmarked.status.problem, /^not-marked-for-this-org: /);
  assert.strictEqual(await env.BUNDLES.head(`big/${unmarked.id}`), null, "nothing kept");

  const marked = await bigVerdict(env, "carol", fixture("candidate-v2.krate"), null);
  assert.strictEqual(marked.status.status, "refused");
  assert.strictEqual(
    marked.status.problem,
    "this app is private to acme: publish it with krate publish --org acme",
  );
  assert.strictEqual(env._kv.get(`app:${marked.sha}`), undefined, "never listed");
});
