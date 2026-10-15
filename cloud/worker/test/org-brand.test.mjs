/*
 * An organisation's logo (src/org-brand.js), through the hub's real routes
 * and store: an owner sets it and every member reads it in GET
 * /orgs/<slug>; only owners and admins change it; anything but a small
 * PNG, JPEG or WebP is refused -- an SVG, a picture that is not what its
 * type says, one too large -- and removing it is audited like setting it.
 */
import { test } from "node:test";
import assert from "node:assert";
import { orgEnv, call } from "./org-tools.mjs";
import { validLogo, LOGO_MAX } from "../src/org-brand.js";

const b64 = (bytes) => Buffer.from(bytes).toString("base64");
const PNG = "data:image/png;base64," + b64([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1]);
const JPEG = "data:image/jpeg;base64," + b64([0xff, 0xd8, 0xff, 0xe0, 0, 16, 0x4a, 0x46, 0x49, 0x46, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 0xff, 0xdb, 0, 0x43]);
const WEBP = "data:image/webp;base64," + b64([0x52, 0x49, 0x46, 0x46, 0x24, 0, 0, 0, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38, 0x20, 0x18, 0, 0, 0, 0x30, 0x01, 0, 0x9d]);

async function world() {
  const env = orgEnv({});
  assert.strictEqual((await call(env, "alice", "POST", "/orgs", { json: { slug: "acme", name: "Acme" } })).status, 201);
  for (const [who, role] of [["erin", "admin"], ["carol", "developer"], ["dan", "recipient"]]) {
    const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: who, role } });
    assert.strictEqual((await call(env, who, "POST", `/orgs/acme/invites/${inv.body.code}/accept`)).status, 200);
  }
  return env;
}

test("an owner sets the logo, every member sees it, and removing it is audited", async () => {
  const env = await world();
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme")).body.org.logo, null, "none until one is set");
  const set = await call(env, "alice", "PUT", "/orgs/acme/logo", { json: { data_url: PNG } });
  assert.deepStrictEqual([set.status, set.body.logo], [200, PNG]);
  assert.strictEqual((await call(env, "dan", "GET", "/orgs/acme")).body.org.logo, PNG, "a recipient sees it too");
  assert.strictEqual((await call(env, "erin", "PUT", "/orgs/acme/logo", { json: { data_url: WEBP } })).status, 200, "an admin may change it");
  assert.strictEqual((await call(env, "erin", "DELETE", "/orgs/acme/logo")).status, 200);
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme")).body.org.logo, null);
  const events = (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events.filter((e) => e.action === "settings.logo");
  assert.deepStrictEqual(events.map((e) => [e.actor, e.reason]), [["github:alice", "set"], ["github:erin", "set"], ["github:erin", "removed"]]);
});

test("only owners and admins change it, and only a small real picture is kept", async () => {
  const env = await world();
  const dev = await call(env, "carol", "PUT", "/orgs/acme/logo", { json: { data_url: PNG } });
  assert.deepStrictEqual([dev.status, dev.body.error], [403, "role-cannot"]);
  const svg = "data:image/svg+xml;base64," + b64([...Buffer.from("<svg onload='alert(1)'/>")]);
  const lying = "data:image/png;base64," + b64([...Buffer.from("<script>alert(1)</script>....")]);
  const huge = "data:image/png;base64," + "A".repeat(LOGO_MAX);
  for (const [bad, why] of [[svg, "an SVG can carry a script"], [lying, "bytes that are not a PNG"], [huge, "too large"], ["https://evil.example/x.png", "a link, not a picture"], [42, "not text"]]) {
    const r = await call(env, "alice", "PUT", "/orgs/acme/logo", { json: { data_url: bad } });
    assert.deepStrictEqual([r.status, r.body.error], [400, "bad-request"], why);
  }
  assert.strictEqual((await call(env, "alice", "GET", "/orgs/acme")).body.org.logo, null, "nothing refused was kept");
  assert.ok(validLogo(PNG) && validLogo(JPEG) && validLogo(WEBP), "PNG, JPEG and WebP are pictures it keeps");
});
