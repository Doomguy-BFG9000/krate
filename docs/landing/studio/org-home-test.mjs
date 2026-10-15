// The organisation console's own parts, run from the shipped scripts
// against a stand-in hub: the Dashboard every role starts on (org/home.js)
// with its four numbers, the bars to point at and what needs this person;
// the right-hand panel each of them opens; Settings in its window, with
// its sections down the side; and Activity narrowed by kind and by words.
//
//   node docs/landing/studio/org-home-test.mjs
import { DASH, check, page, settle, ACME, member, button, failures } from "./org-test-kit.mjs";

const t = (s) => String(s || "").replace(/\s+/g, " ");
const NOW = 1792000000;
const DAY = 86400;
const iso = (k) => new Date((NOW - k * DAY) * 1000).toISOString().slice(0, 10);
const days = (known) => Array.from({ length: 30 }, (_, i) => ({ day: iso(29 - i), people: known[29 - i] ?? 0 }));
const weeks = (known) => Array.from({ length: 12 }, (_, i) => ({ from: iso(7 * (11 - i) + 6), to: iso(7 * (11 - i)), people: known[11 - i] ?? 0 }));
const people = [member("github:alice", "owner"), member("github:bob", "reviewer"), member("github:carol", "developer"), member("github:dan", "recipient")];

const insights = {
  v: 1, now: NOW, today: iso(0), time_zone: "Asia/Singapore", role: "owner", names: true, sections: ["org", "developer", "reviewer", "yours"],
  org: {
    adoption: { members: 4, active_week: 3, active_month: 4, daily: days({ 0: 2, 1: 3, 5: 1 }), trend: weeks({ 0: 3, 1: 1 }), apps: 2, apps_used: 1, top: [{ app_id: "com.acme.notes", name: "Team Notes", people: 3 }], unused: [{ app_id: "com.acme.rota", name: "Rota", version: "1.0.0", digest: "r".repeat(64), released_at: NOW - 40 * DAY }], joined_not_opened: 0 },
    people: { by_role: { owner: 1, reviewer: 1, developer: 1, recipient: 1 }, inactive: [{ principal: "github:dan", display: "dan", role: "recipient", last_active: iso(38) }], invites: [{ principal: "email:kim@acme.test" }] },
    security: { devices: [], publishers: [], approvers: 2, two_person: true, solo_releases: 0, refusals: { week: [{ reason: "not-a-member", n: 5 }], week_total: 5, weekly_average_before: 1, spike: true } },
    releases: { approved_month: 1, waiting: 1, oldest_waiting_since: NOW - 2 * DAY },
  },
  reviewer: { queue: [{ name: "Rota", version: "1.1.0", waited_seconds: 2 * DAY }], approved_by_you_month: 1, approved_month: 1 },
  yours: { devices: [], apps: [] },
};
const audit = { chain_ok: true, events: [
  { seq: 1, at: NOW - 3 * DAY, actor: "github:alice", action: "org.create", target: "org:acme", result: "allowed" },
  { seq: 2, at: NOW - 2 * DAY, actor: "github:alice", action: "key.init", target: "key:v1", result: "allowed" },
  { seq: 3, at: NOW - DAY, actor: "github:mallory", action: "release.download", target: "release:" + "d".repeat(64), result: "denied", reason: "not-a-member" },
  { seq: 4, at: NOW - 3600, actor: "github:alice", action: "invite.create", target: "invite:email:kim@acme.test/developer", result: "allowed" },
] };
function routes(role) {
  const me = people.find((m) => m.role === role);
  return {
    "GET /orgs/acme": () => [200, { org: { ...ACME, created_at: NOW - 120 * DAY, state: "active", time_zone: "Asia/Singapore", time_zone_chosen: true }, me, members: people }],
    "GET /orgs/acme/insights": () => [200, insights],
    "GET /orgs/acme/audit": () => [200, audit],
    "GET /orgs/acme/candidates": () => [200, []],
    "GET /orgs/acme/releases": () => [200, []],
  };
}
const shown = (n) => t(n.shown);

/* ---- the Dashboard is where everyone starts ---- */
{
  const p = await page("https://krate.tech/org/?o=acme", DASH, { token: "t", routes: routes("owner") });
  await settle();
  const pane = p.el("pane");
  check(p.location.href.endsWith("tab=home") && p.el("tab-home").attrs["aria-selected"] === "true", "an organisation opens on its Dashboard");
  check(/Active this week 3 of 4 ▲ 2 vs last week/.test(shown(pane)), "the week's active people, with how it moved against last week");
  check(/Apps in use 1 of 2/.test(shown(pane)) && /Waiting for approval 1 the oldest for 2 days/.test(shown(pane)), "apps in use, and what is waiting with how long");
  check(/Refused this week 5 ▲ 4 vs a usual week/.test(shown(pane)), "refusals against a usual week; up is the bad direction");
  const bars = pane.all((n) => n.tagName === "BUTTON" && /^hc-b( |$)/.test(String(n.className)));
  check(bars.length === 30 && /: 2 people$/.test(bars[29].attrs["aria-label"]) && /: 3 people$/.test(bars[28].attrs["aria-label"]), "thirty bars, each saying its day and its number");
  check(/Needs you/.test(shown(pane)) && /1 version is waiting for approval Review/.test(shown(pane)) && /More refusals than usual this week See why/.test(shown(pane)), "what needs the owner, each with its click");
  check(/Days run midnight to midnight, Singapore time\./.test(shown(pane)), "and which day it counts by");

  // A number opens what is behind it, on the right.
  button(pane, "Active this week3of 4▲ 2 vs last week") || pane.all((n) => n.tagName === "BUTTON" && /^Active this week/.test(n.attrs["aria-label"] || ""))[0].click();
  const stat = pane.all((n) => n.tagName === "BUTTON" && /^Active this week/.test(n.attrs["aria-label"] || ""))[0];
  stat.click();
  await settle();
  check(p.el("crTitle").textContent === "People" && /Not seen for 30 days @dan last on/.test(shown(p.el("crBody"))), "a number opens its people in the right-hand panel");
  check(p.el("in").classList.contains("inspect") && p.el("crBack").hidden === false, "and the panel says how to go back");
  p.el("crBack").click();
  await settle();
  check(p.el("crTitle").textContent === "Overview" && /Quick actions/.test(shown(p.el("crBody"))), "back to the overview: quick actions and what changed lately");
  bars[29].click();
  await settle();
  check(p.el("crTitle").textContent === bars[29].attrs["aria-label"].split(":")[0] && /2 people opened one of Acme's apps that day/.test(shown(p.el("crBody"))), "a bar opens its day");
}

/* ---- the right-hand panel's overview speaks in words ---- */
{
  const p = await page("https://krate.tech/org/?o=acme&tab=releases", DASH, { token: "t", routes: routes("owner") });
  await settle();
  const feed = shown(p.el("crBody"));
  check(/turned on end-to-end encryption for the team's data/.test(feed) && /invited kim@acme\.test as developer/.test(feed), "what changed lately is said as what a person did");
  check(!/key\.init|key:v1|signing\.|ring:/.test(feed), "no event name or key ever reaches the panel");
}

/* ---- Settings is a window with its sections down the side ---- */
{
  const p = await page("https://krate.tech/org/?o=acme&tab=releases", DASH, { token: "t", routes: { ...routes("owner"), "GET /orgs/acme/sso": () => [200, null], "GET /orgs/acme/exports": () => [200, []], "GET /orgs/acme/connections": () => [200, []] } });
  await settle();
  p.el("tab-settings").click();
  await settle();
  const dlg = p.el("setDlg");
  check(dlg.attrs.open !== undefined && p.el("pane").parent === p.el("setSlot"), "Settings opens in its window, holding the page's own pane");
  const nav = p.el("setNav").all((n) => n.tagName === "BUTTON").map((b) => b.textContent);
  check(nav[0] === "General" && nav.includes("Signature") && nav.includes("Billing"), "its sections down the side, General first and Billing last: " + nav.join(", "));
  check(/Time zone/.test(shown(p.el("pane"))), "General holds the time zone");
  p.el("setClose").click();
  await settle();
  check(dlg.attrs.open === undefined && p.el("pane").parent !== p.el("setSlot") && p.location.href.endsWith("tab=releases"), "closing it puts the pane back and returns to where the person was");
}

/* ---- Activity, narrowed down ---- */
{
  const p = await page("https://krate.tech/org/?o=acme&tab=activity", DASH, { token: "t", routes: routes("owner") });
  await settle();
  const pane = p.el("pane");
  const entries = () => pane.all((n) => n.tagName === "BUTTON" && String(n.className).startsWith("ev")).length;
  check(entries() === 4, "every entry is listed");
  button(pane, "Refused 1").click();
  check(entries() === 1 && /Refused @mallory tried to open/.test(shown(pane)), "Refused shows only what the hub said no to");
  button(pane, "All").click();
  const find = pane.all((n) => n.tagName === "INPUT" && n.attrs.type === "search")[0];
  find.value = "kim";
  find.dispatch("input");
  check(entries() === 1 && /invited kim@acme\.test/.test(shown(pane)), "words find the entries that say them");
  pane.all((n) => n.tagName === "BUTTON" && String(n.className).startsWith("ev"))[0].click();
  await settle();
  check(p.el("crTitle").textContent === "What happened" && /Who @alice/.test(shown(p.el("crBody"))), "an entry opens in full on the right");
}

console.log(failures() ? `${failures()} check(s) failed` : "ok  the console: Dashboard, panel, Settings, Activity");
process.exit(failures() ? 1 : 0);
