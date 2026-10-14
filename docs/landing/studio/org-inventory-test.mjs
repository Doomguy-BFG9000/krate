// The organisation dashboard's Apps tab (IC-954), run from the shipped
// scripts against a stand-in hub in the hub's shapes: every app with its
// owner, how it was made, what it can do, its connections, who it is for
// and who opened it; an app whose owner left comes first with the control
// that fixes it; owners and admins choose an owner, reviewers only read;
// the list downloads as a spreadsheet; and no machinery word is in what a
// person reads by default.
//
//   node docs/landing/studio/org-inventory-test.mjs
import { DASH, check, page, settle, ACME, member, button, failures } from "./org-test-kit.mjs";

const t = (s) => String(s || "").replace(/\s+/g, " ");
const EXP = {
  app_id: "com.acme.expenses", name: "Expenses", version: "1.0.1", digest: "e".repeat(64), state: "released",
  owner: { principal: "github:carol", display: "carol", active: true }, needs_owner: false,
  last_sent_by: "github:frank", last_sent_at: 1760000400,
  built: { tool: "krate studio", provider: "claude", model: "claude-opus-5", finished_at: 1760000300, revisions: 1, prompt: "an expenses tracker for finance", words: "Krate Studio with Claude (claude-opus-5)" },
  built_words: "Krate Studio with Claude (claude-opus-5)",
  capabilities: [{ cap: "net.connection:finance-api", words: "use your organisation's finance-api connection" }],
  connections: ["finance-api"], audience: ["finance"], people_30_days: 3,
  people: [{ principal: "github:alice" }, { principal: "github:dan" }, { principal: "github:erin" }],
};
const ROTA = {
  app_id: "com.acme.rota", name: "Rota", version: "1.0.0", digest: "r".repeat(64), state: "waiting",
  owner: { principal: "github:gone", display: "gone", active: false }, needs_owner: true,
  last_sent_by: "github:gone", last_sent_at: 1760000500, built: null, built_words: "by hand",
  capabilities: [], connections: [], audience: null, people_30_days: 0,
};
const people = [member("github:alice", "owner"), member("github:bob", "reviewer"), member("github:carol", "developer"), member("github:frank", "developer"), member("github:dan", "recipient")];
const routes = (me, extra = {}) => ({
  "GET /orgs/acme": () => [200, { org: { ...ACME, state: "active" }, me: people.find((m) => m.principal === me), members: people }],
  "GET /orgs/acme/inventory": () => [200, { apps: me === "github:bob" ? [ROTA, { ...EXP, people: undefined }] : [ROTA, EXP], days: 30, names: me !== "github:bob" }],
  ...extra,
});
const cards = (p) => p.el("pane").all((n) => n.tagName === "DIV" && n.className.includes("appc inv"));

/* ---- an owner ------------------------------------------------------------ */
{
  let sent = null;
  const p = await page("https://krate.tech/org/?o=acme&tab=apps", DASH, {
    token: "t",
    routes: routes("github:alice", {
      "PUT /orgs/acme/apps/com.acme.rota/owner": (h, body) => { sent = { body, key: h["idempotency-key"] }; return [200, { app_id: "com.acme.rota", owner: { principal: body.principal, active: true } }]; },
    }),
  });
  const pane = t(p.el("pane").shown);
  check(/Apps Every app Acme has: who owns it, how it was made, what it can do and who uses it\./.test(pane), "the tab says what it is for");
  check(/2 apps · 1 needs an owner/.test(pane), "a one-line summary, with what needs doing");
  const [first, second] = cards(p);
  check(first && /Rota/.test(first.shown) && /Needs an owner/.test(first.shown) && /gone, who owned it, is no longer in Acme/.test(t(first.shown)), "an app whose owner left comes first, and says so");
  check(button(first, "Make them the owner") && first.all((n) => n.tagName === "SELECT").length === 1, "with the one control that fixes it");
  const s = t(second.shown);
  check(/Expenses In use/.test(s) && /Version 1\.0\.1 · Only for the finance group/.test(s), "name, state, version and who it is for");
  check(/Owned by @carol/.test(s) && /Last sent by @frank on/.test(s), "its owner and who last sent it");
  check(/Made with Krate Studio with Claude \(claude-opus-5\), changed 1 time since/.test(s), "how it was made, in the same words as the hub's");
  check(/Can use your organisation's finance-api connection/.test(s) && /Uses the finance-api connection: the app never holds the credential/.test(s), "what it can do and the connection it uses");
  check(/3 people opened it in the last 30 days: @alice, @dan, @erin/.test(s), "who opened it, by name, for an owner");
  check(/Made by hand/.test(t(first.shown)), "an app made by hand says so");
  // Rule 1: one obvious action. An app with an owner keeps the picker folded.
  const change = button(second, "Change owner");
  check(change && button(second, "Make them the owner").parent.hidden === true, "an owned app shows Change owner, its picker folded away");
  change.click();
  check(button(second, "Make them the owner").parent.hidden === false && change.hidden, "Change owner opens the picker");
  // Rule 3 (the seamless bar): the exact terms only behind Details.
  const details = second.all((n) => n.tagName === "DETAILS")[0];
  check(details && /App id: com\.acme\.expenses/.test(t(details.shown)) && /Asked for: “an expenses tracker for finance”/.test(t(details.shown)), "the app id, the request and the exact terms are behind Details");
  const outside = second.children.filter((c) => c.tagName !== "DETAILS").map((c) => t(c.shown)).join(" ");
  check(!/digest|manifest|capabilit|net\.connection|sha256|principal|github:/i.test(outside), "no machinery word outside Details");
  check(!outside.includes("e".repeat(12)), "no digest anywhere a person reads");

  // Assigning: the picked member, with an Idempotency-Key, then the list again.
  const sel = first.all((n) => n.tagName === "SELECT")[0];
  sel.value = "github:frank";
  button(first, "Make them the owner").click();
  await settle();
  check(sent && sent.body.principal === "github:frank" && sent.key, "Make them the owner sends that member, once");
  check(/@frank owns Rota now\. It is in the activity\./.test(t(p.el("toast").shown)), "and says it is done, and where it is recorded");
}

/* ---- the spreadsheet ----------------------------------------------------- */
{
  const p = await page("https://krate.tech/org/?o=acme&tab=apps", DASH, {
    token: "t",
    routes: routes("github:alice", { "GET /orgs/acme/inventory.csv": () => [200, "app_id,name\n"] }),
  });
  const b = button(p.el("pane"), "Download the list as a spreadsheet");
  check(Boolean(b), "the list downloads as a spreadsheet");
  let made = null;
  p.win.URL.createObjectURL = () => "blob:x";
  p.win.URL.revokeObjectURL = () => {};
  const create = p.document.createElement.bind(p.document);
  p.document.createElement = (tag) => { const n = create(tag); if (tag === "a") { made = n; n.click = () => {}; } return n; };
  b.click();
  await settle();
  check(p.sent.some((r) => r.key === "GET /orgs/acme/inventory.csv" && r.headers.authorization === "Bearer t"), "from the hub, signed in");
  check(made && made.download === "acme-apps.csv", "saved as acme-apps.csv");
}

/* ---- a reviewer reads; a developer has no tab ---------------------------- */
{
  const p = await page("https://krate.tech/org/?o=acme&tab=apps", DASH, { token: "t", routes: routes("github:bob") });
  const pane = t(p.el("pane").shown);
  check(/3 people opened it in the last 30 days/.test(pane) && !/: alice, dan/.test(pane), "a reviewer sees how many, not who");
  check(!button(p.el("pane"), "Make them the owner") && !button(p.el("pane"), "Change owner"), "and cannot choose an owner");
  const dev = await page("https://krate.tech/org/?o=acme&tab=apps", DASH, { token: "t", routes: routes("github:carol") });
  check(!dev.el("tabs").children.some((b) => b.attrs.id === "tab-apps"), "a developer has no Apps tab");
}

/* ---- empty, and refused --------------------------------------------------- */
{
  const p = await page("https://krate.tech/org/?o=acme&tab=apps", DASH, {
    token: "t", routes: routes("github:alice", { "GET /orgs/acme/inventory": () => [200, { apps: [], days: 30, names: true }] }),
  });
  check(/No apps yet/.test(p.el("pane").shown) && /Send to Acme for approval/.test(t(p.el("pane").shown)), "empty, it teaches the next step");
  const r = await page("https://krate.tech/org/?o=acme&tab=apps", DASH, {
    token: "t", routes: routes("github:alice", { "GET /orgs/acme/inventory": () => [403, { error: "role-cannot", message: "a reviewer cannot" }] }),
  });
  check(r.el("pane").shown.length > 20 && !/\{"error"/.test(r.el("pane").shown), "a refusal is words, never JSON");
}

/* ---- the activity says it in words ---------------------------------------- */
{
  const { KIT } = await import("./org-test-kit.mjs");
  const vm = await import("node:vm");
  const ctx = { URL, URLSearchParams };
  ctx.window = ctx;
  vm.createContext(ctx);
  vm.runInContext(KIT, ctx);
  const K = ctx.KrateOrg;
  check(K.eventWords({ actor: "github:erin", action: "app.owner", target: "app:com.acme.rota", result: "allowed", reason: "github:frank" }) === "@erin made @frank the owner of com.acme.rota.", "assigning an owner, in the activity");
  check(K.eventWords({ actor: "github:alice", action: "app.owner", target: "app:com.acme.rota", result: "allowed", reason: "needs an owner: github:gone was removed" }) === "com.acme.rota needs an owner: @gone, who owned it, was removed.", "an app left without an owner, in the activity");
  check(K.MATRIX["inventory.view"].reviewer === "yes" && !K.MATRIX["inventory.view"].developer && !K.MATRIX["inventory.people"].reviewer && K.MATRIX["app.owner"].admin === "yes" && !K.MATRIX["usage.view"].reviewer, "the page's matrix has IC-954's rows, as the hub's");
}

console.log(failures() ? `${failures()} check(s) failed` : "ok  the Apps tab");
process.exit(failures() ? 1 : 0);
