/*
 * An organisation's own day (F-319, src/org-time.js): every count a team
 * reads by the day starts at midnight where the team is. Through the
 * hub's real routes and store: a Singapore organisation's evening open
 * (UTC) lands on the next local day, a UTC one on the same day; only
 * owners and admins change the zone, a made-up zone is refused, a change
 * is audited and keeps the days already counted; and a day with a clock
 * change is still one day.
 */
import { test } from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join as joinPath } from "node:path";
import { orgEnv, call } from "./org-tools.mjs";
import { dayIn, dayBefore, validZone } from "../src/org-time.js";

const here = dirname(fileURLToPath(import.meta.url));
const FIX = joinPath(here, "fixtures", "org-inventory");
const fixture = (name) => new Uint8Array(readFileSync(joinPath(FIX, name)));
const KEYS = JSON.parse(readFileSync(joinPath(FIX, "keys.json"), "utf8"));
const LEASE_SEED = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";

// 14 October 2026, 20:30 UTC: 04:30 on the 15th in Singapore.
const EVENING = Math.floor(Date.UTC(2026, 9, 14, 20, 30, 0) / 1000);

async function at(seconds, fn) {
  const real = Date.now;
  Date.now = () => seconds * 1000;
  try {
    return await fn();
  } finally {
    Date.now = real;
  }
}
const must = (r, what, status) => {
  if (status ? r.status !== status : r.status >= 300) throw new Error(`${what}: ${r.status} ${JSON.stringify(r.body)}`);
  return r;
};

/// An organisation made by alice's browser in `zone` (or none), with one
/// app she released herself, opened once by her at `openAt`.
async function world(zone, openAt) {
  const env = orgEnv({ ORG_SECRET_KEY: "org-secret-key-for-tests", ORG_LEASE_KEY: LEASE_SEED });
  const made = await at(openAt - 7200, async () => {
    const r = must(await call(env, "alice", "POST", "/orgs", { json: { slug: "acme", name: "Acme", ...(zone === undefined ? {} : { time_zone: zone }) } }), "create");
    must(await call(env, "alice", "PUT", "/orgs/acme/root-key", { json: { public_key: KEYS.root } }), "root");
    const digest = must(await call(env, "alice", "POST", "/orgs/acme/candidates", { bytes: fixture("expenses-1.krate") }), "send", 201).body.digest;
    must(await call(env, "alice", "POST", `/orgs/acme/candidates/${digest}/approve`), "release");
    return { org: r.body.org, digest };
  });
  await at(openAt, async () => must(await call(env, "alice", "GET", `/a/${made.digest}`, { headers: { accept: "application/json" } }), "open"));
  return { env, ...made };
}
const insights = async (env, when) => {
  const r = await at(when, () => call(env, "alice", "GET", "/orgs/acme/insights"));
  assert.strictEqual(r.status, 200, JSON.stringify(r.body));
  return r.body;
};
const lastDay = (body) => body.org.adoption.daily[body.org.adoption.daily.length - 1];

test("a Singapore team's evening open (UTC) is counted on its own next morning", async () => {
  const { env, org } = await world("Asia/Singapore", EVENING);
  assert.deepStrictEqual([org.time_zone, org.time_zone_chosen], ["Asia/Singapore", true], "the browser's zone is kept as the organisation is made");
  const shown = (await call(env, "alice", "GET", "/orgs/acme")).body.org;
  assert.deepStrictEqual([shown.time_zone, shown.time_zone_chosen], ["Asia/Singapore", true]);
  const body = await insights(env, EVENING + 600);
  assert.deepStrictEqual([body.today, body.time_zone], ["2026-10-15", "Asia/Singapore"]);
  assert.deepStrictEqual(lastDay(body), { day: "2026-10-15", people: 1 }, "counted on the 15th, Singapore's day");
  assert.strictEqual(body.org.adoption.active_week, 1);
  // The day before has nobody: the open is not split across two days.
  assert.strictEqual(body.org.adoption.daily[28].people, 0);
});

test("the same open in an organisation on UTC is counted on the 14th", async () => {
  const { env, org } = await world(undefined, EVENING);
  assert.deepStrictEqual([org.time_zone, org.time_zone_chosen], ["UTC", false], "made by the CLI: UTC, and it says it was not chosen");
  const body = await insights(env, EVENING + 600);
  assert.deepStrictEqual([body.today, body.time_zone, body.time_zone_chosen], ["2026-10-14", "UTC", false]);
  assert.deepStrictEqual(lastDay(body), { day: "2026-10-14", people: 1 });
});

test("a made-up zone is not kept, and only owners and admins change it, audited", async () => {
  const { env, org } = await world("Mars/Olympus_Mons", EVENING);
  assert.deepStrictEqual([org.time_zone, org.time_zone_chosen], ["UTC", false], "a zone nobody knows is ignored at creation, not fatal");
  for (const [who, role] of [["erin", "admin"], ["carol", "developer"]]) {
    const inv = must(await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: who, role } }), `invite ${who}`);
    must(await call(env, who, "POST", `/orgs/acme/invites/${inv.body.code}/accept`), `accept ${who}`);
  }
  const bad = await call(env, "erin", "PUT", "/orgs/acme/time-zone", { json: { time_zone: "Not/AZone" } });
  assert.deepStrictEqual([bad.status, bad.body.error], [400, "bad-request"]);
  const injected = await call(env, "erin", "PUT", "/orgs/acme/time-zone", { json: { time_zone: "UTC'; DROP TABLE org; --" } });
  assert.strictEqual(injected.status, 400);
  const dev = await call(env, "carol", "PUT", "/orgs/acme/time-zone", { json: { time_zone: "Europe/Berlin" } });
  assert.deepStrictEqual([dev.status, dev.body.error], [403, "role-cannot"]);
  const ok = await at(EVENING + 60, () => call(env, "erin", "PUT", "/orgs/acme/time-zone", { json: { time_zone: "Europe/Berlin" } }));
  assert.deepStrictEqual([ok.status, ok.body.time_zone, ok.body.time_zone_chosen], [200, "Europe/Berlin", true]);
  const events = (await call(env, "alice", "GET", "/orgs/acme/audit")).body.events;
  const change = events.find((e) => e.action === "settings.time-zone");
  assert.deepStrictEqual([change.actor, change.result, change.reason], ["github:erin", "allowed", "UTC (not chosen) -> Europe/Berlin"]);
  assert.ok(events.some((e) => e.action === "org.time-zone" && e.actor === "github:carol" && e.result === "denied"), "the developer's attempt is in the audit");
});

test("a change applies from now on: days already counted keep their day", async () => {
  const { env } = await world("UTC", EVENING);
  must(await at(EVENING + 60, () => call(env, "alice", "PUT", "/orgs/acme/time-zone", { json: { time_zone: "Asia/Singapore" } })), "change");
  const body = await insights(env, EVENING + 600);
  assert.strictEqual(body.today, "2026-10-15");
  const daily = body.org.adoption.daily;
  assert.deepStrictEqual([daily[28], daily[29]], [{ day: "2026-10-14", people: 1 }, { day: "2026-10-15", people: 0 }]);
});

test("the day's arithmetic: zones, a clock change, and what counts as a zone", () => {
  // Los Angeles goes from PST to PDT on 8 March 2026: a 23-hour day.
  assert.strictEqual(dayIn("America/Los_Angeles", Date.UTC(2026, 2, 8, 6, 30) / 1000), "2026-03-07");
  assert.strictEqual(dayIn("America/Los_Angeles", Date.UTC(2026, 2, 9, 6, 30) / 1000), "2026-03-08");
  assert.strictEqual(dayBefore("2026-03-09", 1), "2026-03-08");
  assert.strictEqual(dayBefore("2026-03-01", 1), "2026-02-28");
  assert.strictEqual(dayBefore("2026-01-01", 30), "2025-12-02");
  // India is UTC+5:30, Kathmandu +5:45: half and quarter hours.
  assert.strictEqual(dayIn("Asia/Kolkata", Date.UTC(2026, 9, 14, 18, 29) / 1000), "2026-10-14");
  assert.strictEqual(dayIn("Asia/Kolkata", Date.UTC(2026, 9, 14, 18, 31) / 1000), "2026-10-15");
  assert.strictEqual(dayIn("Asia/Kathmandu", Date.UTC(2026, 9, 14, 18, 16) / 1000), "2026-10-15");
  for (const z of ["UTC", "Europe/Berlin", "Asia/Singapore", "America/Argentina/Buenos_Aires", "Etc/GMT+5"]) assert.ok(validZone(z), z);
  for (const z of ["", "Mars/Olympus_Mons", "../../etc", "x".repeat(65), 42, null]) assert.ok(!validZone(z), String(z));
});
