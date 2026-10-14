/* What an approver sees and must confirm (IC-950): each candidate carries
 * the capabilities its manifest asks for, in the plain words the validator
 * gives them; the list shows what changed against the app's current
 * release; and approving a release that adds something a person is asked
 * for needs each addition named.
 *
 * The bundles are the real signed fixtures, judged by the real validator.
 * The older release's list is edited in its store to stand for "the last
 * version asked for less": the fixtures are one app, and minting new signed
 * ones would mint new keys for every org test.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-review.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { orgEnv, call, acme, fixture } from "./org-tools.mjs";
import { changesOf, capsArg, judgementCaps } from "../src/org-review.js";

const SEED = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const pair = (r) => [r.status, r.body && r.body.error];

async function world() {
  const env = orgEnv({ ORG_LEASE_KEY: SEED });
  await acme(env);
  return env;
}

const upload = async (env, name) => {
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture(name) });
  assert.strictEqual(up.status, 201, JSON.stringify(up.body));
  return up.body.digest;
};
const approve = (env, who, digest, accept) =>
  call(env, who, "POST", `/orgs/acme/candidates/${digest}/approve`, accept ? { json: { accept } } : {});
const listed = async (env, digest) =>
  (await call(env, "alice", "GET", "/orgs/acme/candidates")).body.find((c) => c.digest === digest);

/* Stand for an older release that asked for less: drop `cap` from its list. */
function askedForLess(env, digest, cap) {
  const store = env.ORGS.object("acme");
  const row = store.sql.exec("SELECT caps FROM candidates WHERE digest = ?", digest).toArray()[0];
  const caps = JSON.parse(row.caps).filter((c) => c.cap !== cap);
  store.sql.exec("UPDATE candidates SET caps = ? WHERE digest = ?", JSON.stringify(caps), digest);
}
/* And one that asked for something this one no longer does. */
function askedForMore(env, digest, extra) {
  const store = env.ORGS.object("acme");
  const row = store.sql.exec("SELECT caps FROM candidates WHERE digest = ?", digest).toArray()[0];
  store.sql.exec("UPDATE candidates SET caps = ? WHERE digest = ?", JSON.stringify([...JSON.parse(row.caps), extra]), digest);
}

test("a candidate carries what it asks for, in plain words, from the real validator", async () => {
  const env = await world();
  const d1 = await upload(env, "candidate-v1.krate");
  const c = await listed(env, d1);
  assert.ok(Array.isArray(c.capabilities) && c.capabilities.length > 0, JSON.stringify(c));
  const window = c.capabilities.find((x) => x.cap === "ui.window:create");
  assert.deepStrictEqual(
    { words: window.words, asks: window.asks, required: window.required },
    { words: "open a window on your screen", asks: false, required: true },
  );
  assert.ok(c.signer, "the signer is shown");
  // A first release has nothing to compare with: its whole list is the change.
  assert.strictEqual(c.changes.against, null);
  assert.strictEqual(c.changes.added.length, c.capabilities.length);
  // Approving a first release needs no confirmation beyond approving it.
  assert.strictEqual((await approve(env, "bob", d1)).status, 200);
  // Once released it is not a pending change.
  assert.strictEqual((await listed(env, d1)).changes, null);
});

test("an update that adds a capability a person is asked for needs it named to be approved", async () => {
  const env = await world();
  const d1 = await upload(env, "candidate-v1.krate");
  assert.strictEqual((await approve(env, "bob", d1)).status, 200);
  const d2 = await upload(env, "candidate-v2.krate");
  // The candidate asks for everything 1.0.0 did plus the camera.
  const camera = { cap: "camera.capture", words: "use your camera", required: false, rationale: "scan receipts", asks: true };
  askedForMore(env, d2, camera);

  const c = await listed(env, d2);
  assert.strictEqual(c.changes.against.digest, d1);
  assert.deepStrictEqual(c.changes.added.map((x) => x.cap), ["camera.capture"]);
  assert.deepStrictEqual(c.changes.removed, []);
  assert.strictEqual(c.changes.kept, c.capabilities.length - 1);

  // Unconfirmed: refused, naming the addition in plain words.
  const no = await approve(env, "bob", d2);
  assert.deepStrictEqual(pair(no), [409, "new-capabilities"]);
  assert.match(no.body.message, /use your camera/);
  assert.deepStrictEqual(no.body.added.map((x) => x.cap), ["camera.capture"]);
  // Confirming something else is not confirming this.
  assert.deepStrictEqual(pair(await approve(env, "bob", d2, ["net.connect:evil.example:443"])), [409, "new-capabilities"]);
  // Nothing was released by the refusals.
  assert.strictEqual((await listed(env, d2)).state, "waiting-for-approval");
  // Named: approved.
  assert.strictEqual((await approve(env, "bob", d2, ["camera.capture"])).status, 200);
  assert.strictEqual((await listed(env, d2)).state, "released");
});

test("an update that only drops or keeps capabilities, or adds one nobody is asked for, approves as before", async () => {
  const env = await world();
  const d1 = await upload(env, "candidate-v1.krate");
  // 1.0.0 also asked for the network; 1.0.1 does not.
  askedForMore(env, d1, { cap: "net.connect:api.acme.example:443", words: "connect to api.acme.example:443 over the network", required: true, rationale: "sync", asks: true });
  assert.strictEqual((await approve(env, "bob", d1)).status, 200);
  const d2 = await upload(env, "candidate-v2.krate");
  // And 1.0.0 did not print; 1.0.1 does -- printing is not asked for.
  askedForLess(env, d1, "io.stdout");
  const c = await listed(env, d2);
  assert.deepStrictEqual(c.changes.removed.map((x) => x.cap), ["net.connect:api.acme.example:443"]);
  assert.deepStrictEqual(c.changes.added.map((x) => x.cap), ["io.stdout"]);
  assert.strictEqual((await approve(env, "bob", d2)).status, 200);
});

test("the usual refusals come first: nobody hears about capabilities they may not act on", async () => {
  const env = await world();
  const d1 = await upload(env, "candidate-v1.krate");
  assert.strictEqual((await approve(env, "bob", d1)).status, 200);
  const d2 = await upload(env, "candidate-v2.krate");
  askedForMore(env, d2, { cap: "camera.capture", words: "use your camera", required: false, rationale: "", asks: true });
  assert.deepStrictEqual(pair(await approve(env, "carol", d2)), [403, "self-approval"]);
  assert.deepStrictEqual(pair(await approve(env, "dan", d2)), [403, "role-cannot"]);
  assert.deepStrictEqual(pair(await approve(env, "stranger", d2)), [403, "not-a-member"]);
});

test("the list is bounded and an unknown capability is treated as asked for", () => {
  const many = Array.from({ length: 500 }, (_, i) => ({ cap: `fs.read:./d${i}/**`, words: "x".repeat(1000), required: true, rationale: "", asks: false }));
  const caps = capsArg(many);
  assert.strictEqual(caps.length, 200);
  assert.strictEqual(caps[0].words.length, 300);
  assert.strictEqual(capsArg([{ cap: "mystery.thing" }])[0].asks, true);
  // A validator from before the field: the manifest's list, every one asked.
  const old = judgementCaps({ manifest: { capabilities: [{ cap: "io.stdout", required: true, rationale: "r" }] } });
  assert.deepStrictEqual(old, [{ cap: "io.stdout", words: "io.stdout", required: true, rationale: "r", asks: true }]);
  // Changes against nothing, and against a release.
  const a = { digest: "a", version: "1", caps: JSON.stringify([{ cap: "x" }, { cap: "y" }]) };
  const b = { digest: "b", version: "2", caps: JSON.stringify([{ cap: "y" }, { cap: "z" }]) };
  const d = changesOf(b, a);
  assert.deepStrictEqual([d.added.map((x) => x.cap), d.removed.map((x) => x.cap), d.kept], [["z"], ["x"], 1]);
});
