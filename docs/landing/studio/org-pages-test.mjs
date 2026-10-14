// The organisation pages on krate.tech (IC-947): /orgs/, /org/, /join/ and
// the private side of /open/, run from their shipped scripts against a
// stand-in hub that answers in the contract's shapes
// (Plan/org-pilot/CONTRACT.md, Addenda 1 and 2).
//
// What is checked is what a person meets: which tabs and buttons each role
// is shown (the contract's role matrix), that every refusal reads as words
// with a next step and never as its code, that Approve names the exact
// digest it sends, and that a private app names its organisation only to a
// member or to someone removed from it.
//
//   node docs/landing/studio/org-pages-test.mjs
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { webcrypto } from "node:crypto";

const KIT = readFileSync("docs/landing/kit/org.js", "utf8");
const ORGS = readFileSync("docs/landing/orgs/orgs.js", "utf8");
const DASH = readFileSync("docs/landing/org/dashboard.js", "utf8");
const JOIN = readFileSync("docs/landing/join/join.js", "utf8");
const OPEN = readFileSync("docs/open/index.html", "utf8");

let bad = 0;
function check(ok, name) {
  if (!ok) bad++;
  console.log(`${ok ? "ok  " : "FAIL"} ${name}`);
}

/* ---------------------------------------------------------------- a DOM
 * Just enough of one for these scripts: elements they build with
 * createElement, elements the page already has (by id, made on first ask),
 * text, attributes, classes, values, clicks and submits. */
function makeDom() {
  const byId = new Map();
  class Node {
    constructor(tag) {
      this.tagName = String(tag || "#text").toUpperCase();
      this.children = [];
      this.parent = null;
      this.attrs = {};
      this.on = {};
      this.dataset = {};
      this.style = {};
      this.hidden = false;
      this.disabled = false;
      this.selected = false;
      this._text = "";
      this._value = "";
      this.className = "";
      const self = this;
      this.classList = {
        add: (c) => { if (!self.classList.contains(c)) self.className = (self.className + " " + c).trim(); },
        remove: (c) => { self.className = self.className.split(/\s+/).filter((x) => x && x !== c).join(" "); },
        contains: (c) => self.className.split(/\s+/).includes(c),
        toggle: (c, on) => { (on === undefined ? !self.classList.contains(c) : on) ? self.classList.add(c) : self.classList.remove(c); },
      };
    }
    appendChild(n) { if (n.parent) n.parent.children = n.parent.children.filter((x) => x !== n); n.parent = this; this.children.push(n); return n; }
    remove() { if (this.parent) this.parent.children = this.parent.children.filter((x) => x !== this); this.parent = null; }
    get lastChild() { return this.children[this.children.length - 1] || null; }
    get textContent() { return this.tagName === "#TEXT" ? this._text : this.children.map((c) => c.textContent).join(""); }
    set textContent(v) { this.children = []; if (this.tagName === "#TEXT") this._text = String(v); else if (v !== "" && v != null) { const t = new Node("#text"); t._text = String(v); this.appendChild(t); } }
    /// What a person sees: text of everything not hidden.
    get shown() { if (this.hidden) return ""; return this.tagName === "#TEXT" ? this._text : this.children.map((c) => c.shown).join(" "); }
    setAttribute(k, v) { this.attrs[k] = String(v); if (k === "id") byId.set(String(v), this); if (k === "value") this._value = String(v); }
    getAttribute(k) { return k in this.attrs ? this.attrs[k] : null; }
    removeAttribute(k) { delete this.attrs[k]; }
    set id(v) { this.setAttribute("id", v); }
    get id() { return this.attrs.id || ""; }
    get href() { return this.attrs.href || ""; }
    set href(v) { this.attrs.href = String(v); }
    set src(v) { this.attrs.src = String(v); }
    get value() {
      if (this.tagName === "SELECT") { const o = this.all((n) => n.tagName === "OPTION"); const s = o.find((x) => x.selected) || o[0]; return s ? s.value : ""; }
      return this._value;
    }
    set value(v) {
      if (this.tagName === "SELECT") { this.all((n) => n.tagName === "OPTION").forEach((o) => { o.selected = o.value === String(v); }); return; }
      this._value = String(v);
    }
    addEventListener(t, f) { (this.on[t] ||= []).push(f); }
    dispatch(t, extra = {}) { const ev = { type: t, target: this, preventDefault() {}, ...extra }; (this.on[t] || []).forEach((f) => f(ev)); if (this["on" + t]) this["on" + t](ev); }
    click() { this.dispatch("click"); if (this.tagName === "BUTTON" && this.attrs.type === "submit") { let p = this.parent; while (p && p.tagName !== "FORM") p = p.parent; if (p) p.dispatch("submit"); } }
    focus() {} select() {}
    showModal() { this.attrs.open = ""; } close() { delete this.attrs.open; }
    all(pred) { const out = []; const walk = (n) => { n.children.forEach((c) => { if (pred(c)) out.push(c); walk(c); }); }; walk(this); return out; }
    querySelector(sel) { return this.querySelectorAll(sel)[0] || null; }
    querySelectorAll(sel) {
      if (sel.startsWith("#")) return this.all((n) => n.id === sel.slice(1));
      if (sel.startsWith(".")) return this.all((n) => n.classList.contains(sel.slice(1)));
      return this.all((n) => n.tagName === sel.toUpperCase());
    }
  }
  const body = new Node("body");
  const document = {
    title: "",
    body,
    documentElement: new Node("html"),
    createElement: (t) => new Node(t),
    createElementNS: (_ns, t) => new Node(t),
    createTextNode: (s) => { const n = new Node("#text"); n._text = String(s); return n; },
    getElementById(id) {
      if (!byId.has(id)) { const n = new Node("div"); n.setAttribute("id", id); body.appendChild(n); }
      return byId.get(id);
    },
    querySelector(sel) {
      if (sel.startsWith(".")) {
        const found = body.querySelector(sel);
        if (found) return found;
        const n = new Node("p"); n.className = sel.slice(1); body.appendChild(n); return n;
      }
      return body.querySelector(sel);
    },
  };
  return { document, byId, Node };
}

function storage(seed = {}) {
  const m = new Map(Object.entries(seed));
  return { getItem: (k) => (m.has(k) ? m.get(k) : null), setItem: (k, v) => m.set(k, String(v)), removeItem: (k) => m.delete(k), _m: m };
}

/* -------------------------------------------------------- a stand-in hub
 * `routes` maps "METHOD /path" to (headers, body) => [status, json]. Every
 * request is recorded so a test can see exactly what was sent. */
function hubFetch(routes, sent) {
  return async (url, init = {}) => {
    const u = new URL(url);
    const key = `${(init.method || "GET").toUpperCase()} ${u.pathname}`;
    const headers = init.headers || {};
    sent.push({ key, url, headers, body: init.body ? JSON.parse(init.body) : null });
    const route = routes[key];
    const [status, json] = route ? route(headers, init.body ? JSON.parse(init.body) : null) : [404, { error: "no-such-route", message: "" }];
    return {
      ok: status >= 200 && status < 300,
      status,
      text: async () => (json === null ? "" : JSON.stringify(json)),
      blob: async () => ({ size: 10 }),
    };
  };
}

const settle = async () => { for (let i = 0; i < 12; i++) await new Promise((r) => setTimeout(r, 0)); };

/// One page: the kit, then the page's script, in a fresh window.
async function page(url, script, { token = "", routes = {}, ua = "Mozilla/5.0 (Macintosh) Chrome/120" } = {}) {
  const { document, byId } = makeDom();
  const u = new URL(url);
  const sent = [];
  const localStorage = storage(token ? { krate_tok: token } : {});
  const location = {
    href: u.href, search: u.search, pathname: u.pathname, hostname: u.hostname, origin: u.origin, hash: u.hash,
    replace(to) { this.href = to; this.replaced = to; },
  };
  const win = {
    document, localStorage, location, URLSearchParams, URL, JSON, Date, Math, Promise, setTimeout, clearTimeout,
    Uint8Array, crypto: webcrypto, navigator: { userAgent: ua, clipboard: { writeText: async () => {} } },
    history: { replaceState(_s, _t, to) { location.href = new URL(to, u.origin).href; } },
    fetch: hubFetch(routes, sent), console,
  };
  win.window = win;
  win.globalThis = win;
  vm.createContext(win);
  vm.runInContext(KIT, win);
  vm.runInContext(script, win);
  await settle();
  return { win, document, byId, sent, location, localStorage, el: (id) => document.getElementById(id) };
}

const P = (s) => s.replace(/\s+/g, " ").trim();
const ACME = { slug: "acme", name: "Acme", created_at: 1760000000, created_by: "github:alice", root_key: "0d9219f281efdb72dede3a3461fb042b595a3763e94c2f03e65d1e48660a4ca2", root_key_set_at: 1760000100, root_key_set_by: "github:alice" };
const member = (principal, role, extra = {}) => ({ org: "acme", principal, display: principal.split(":")[1], kind: principal.split(":")[0], role, state: "active", since: 1760000000, ...extra });
const DIGEST = "7373e64ed97975e617f8c9c0c78cac114370fb54156be5bfd9400656e40c8f37";

/* ------------------------------------------------------- the kit itself */
{
  const p = await page("https://krate.tech/orgs/", "void 0");
  const K = p.win.KrateOrg;

  // The hub address: krate.tech always talks to hub.krate.tech.
  const loc = (h) => ({ hostname: h });
  const st = (v) => storage({ "krate-dev-hub": v });
  check(K.hub(loc("krate.tech"), st("http://127.0.0.1:8799")) === "https://hub.krate.tech", "krate.tech ignores a development hub setting");
  check(K.hub(loc("127.0.0.1"), st("http://127.0.0.1:8799")) === "http://127.0.0.1:8799", "a copy served on this machine may use a hub on this machine");
  check(K.hub(loc("127.0.0.1"), st("https://evil.example")) === "https://hub.krate.tech", "and never a hub anywhere else");
  check(K.hub(loc("localhost"), st("")) === "https://hub.krate.tech", "with nothing set, it is the real hub");

  // Tabs per role, from the contract's matrix.
  const tabs = (r) => K.tabsFor(r).join(",");
  check(tabs("owner") === "releases,waiting,people,activity,settings", "an owner sees all five tabs");
  check(tabs("admin") === "releases,waiting,people,activity,settings", "an admin sees the activity (reads audit)");
  check(tabs("developer") === "releases,waiting,people,settings", "a developer has no Activity tab");
  check(tabs("reviewer") === "releases,waiting,people,settings", "a reviewer has no Activity tab");
  check(tabs("recipient") === "releases,people", "a recipient sees releases and their own entry, nothing else");

  check(K.invitable("owner").join() === "owner,admin,developer,reviewer,recipient", "an owner invites any role");
  check(K.invitable("admin").join() === "developer,reviewer,recipient", "an admin invites developers, reviewers and recipients only");
  check(K.invitable("developer").length === 0 && K.invitable("reviewer").length === 0 && K.invitable("recipient").length === 0, "nobody else invites");

  const A = (me, them, owners = 2) => K.memberActions({ principal: me[0], role: me[1] }, { principal: them[0], role: them[1] }, owners);
  check(A(["github:a", "admin"], ["github:o", "owner"]).remove === false, "an admin is not offered Remove on an owner");
  check(A(["github:a", "admin"], ["github:d", "developer"]).remove === true, "an admin may remove a developer");
  check(A(["github:a", "admin"], ["github:d", "developer"]).roles.join() === "developer,reviewer,recipient", "an admin moves people between developer, reviewer and recipient");
  check(A(["github:a", "admin"], ["github:x", "admin"]).roles.length === 0, "an admin cannot change another admin's role");
  check(A(["github:d", "developer"], ["github:e", "reviewer"]).remove === false, "a developer cannot remove someone else");
  check(A(["github:d", "developer"], ["github:d", "developer"]).removeLabel === "Leave", "but can leave");
  const last = A(["github:o", "owner"], ["github:o", "owner"], 1);
  check(last.remove && /only owner/i.test(last.removeWhy) && last.roles.length === 0, "the last owner's Leave explains why it cannot happen, and offers no other role");
  check(A(["github:o", "owner"], ["github:p", "owner"], 2).removeWhy === "", "with two owners, an owner can be removed");

  // Every stable reason reads as words with a next step, never as itself.
  const REASONS = ["not-signed-in", "not-a-member", "removed", "role-cannot", "last-owner", "self-approval", "digest-mismatch",
    "not-signed-for-this-org", "invite-expired", "invite-used", "invite-for-someone-else", "invite-revoked", "slug-taken",
    "slug-invalid", "idempotency-key-reused", "no-such-org", "offline"];
  for (const r of REASONS) {
    const w = K.refusal(r, { org: "Acme", slug: "acme", here: "/org/?o=acme" });
    const all = w.title + " " + w.body;
    // "removed" and "offline" are words already; a code with a dash is not.
    check(w.title && w.body && (!r.includes("-") || !all.includes(r)) && !/[—–`]|--/.test(all), `"${r}" is words: ${w.title}`);
  }
  const si = K.refusal("not-signed-in", { here: "/org/?o=acme&tab=people" });
  check(si.action && si.action.href === "/login/?next=%2Forg%2F%3Fo%3Dacme%26tab%3Dpeople", "the sign-in refusal comes back to the same page");

  // The audit in words.
  const w1 = K.eventWords({ actor: "github:carol", action: "candidate.approve", target: `candidate:${DIGEST}`, result: "denied", reason: "self-approval" }, { [DIGEST]: "Bounce 1.0.0" });
  check(w1 === "@carol tried to approve Bounce 1.0.0. Refused: they sent it themselves.", "a refused self-approval reads plainly");
  const w2 = K.eventWords({ actor: "github:alice", action: "invite.create", target: "invite:email:erin@acme.test/recipient", result: "allowed" });
  check(w2 === "@alice invited erin@acme.test as recipient.", "an email invitation names the inbox");
  const w3 = K.eventWords({ actor: "github:alice", action: "member.role", target: "member:github:bob", result: "allowed", reason: "reviewer->developer" });
  check(w3 === "@alice changed @bob from reviewer to developer.", "a role change says from and to");
  check(K.fingerprint(ACME.root_key) === "0d92 19f2 81ef db72", "a key fingerprint is its first 16 hex in fours");
}

/* ---------------------------------------------------------------- /orgs/ */
{
  const out = await page("https://krate.tech/orgs/", ORGS);
  check(out.el("out").hidden === false && out.el("in").hidden === true, "/orgs/ signed out shows the sign-in choices");
  const links = out.el("outChoices").all((n) => n.tagName === "A").map((a) => a.href);
  check(links.join() === "/login/?next=%2Forgs%2F#github,/login/?next=%2Forgs%2F#google,/login/?next=%2Forgs%2F#email", "each choice comes back to /orgs/");

  const p = await page("https://krate.tech/orgs/", ORGS, {
    token: "t", routes: { "GET /orgs": () => [200, [{ slug: "acme", name: "Acme", role: "owner" }, { slug: "beta-lab", name: "Beta Lab", role: "recipient" }]] },
  });
  const rows = p.el("list").all((n) => n.tagName === "A");
  check(rows.length === 2 && rows[0].href === "/org/?o=acme" && /Acme acme Owner/.test(P(rows[0].shown)), "/orgs/ lists each organisation with your role");

  const made = await page("https://krate.tech/orgs/", ORGS, {
    token: "t",
    routes: {
      "GET /orgs": () => [200, []],
      "POST /orgs": (_h, b) => (b.slug === "acme" ? [409, { error: "slug-taken", message: "" }] : [201, { org: { slug: b.slug, name: b.name } }]),
    },
  });
  check(made.el("create").hidden === false, "with no organisation yet, the create form is open");
  made.el("cName").value = "Acme";
  made.el("cName").dispatch("input");
  check(made.el("cSlug").value === "acme", "the short name follows the name");
  made.el("createForm").dispatch("submit");
  await settle();
  check(/acme is taken/.test(made.el("cWhy").shown) && !/slug-taken/.test(made.el("cWhy").shown), "a taken name is said in words");
  const firstKey = made.sent.find((s) => s.key === "POST /orgs").headers["idempotency-key"];
  made.el("cSlug").value = "acme-labs";
  made.el("cSlug").dispatch("input");
  made.el("createForm").dispatch("submit");
  await settle();
  const second = made.sent.filter((s) => s.key === "POST /orgs")[1];
  check(second && second.headers["idempotency-key"] && second.headers["idempotency-key"] !== firstKey, "a different request gets a different Idempotency-Key");
  check(made.location.href === "/org/?o=acme-labs&tab=people&new=1", "a new organisation opens on People, to invite the team");
}

/* ----------------------------------------------------------------- /org/ */
function orgRoutes(me, extra = {}) {
  const members = [member("github:alice", "owner"), member("github:bob", "reviewer"), member("github:carol", "developer"), member("email:erin@acme.test", "recipient")];
  return {
    "GET /orgs/acme": () => [200, { org: ACME, me: members.find((m) => m.principal === me) || member(me, "recipient"), members: me === "github:dan" ? [member("github:dan", "recipient")] : members }],
    "GET /orgs/acme/releases": () => [200, [{ name: "Bounce", version: "1.0.0", digest: DIGEST, url: `https://hub.krate.tech/a/${DIGEST}`, uploaded_by: "github:carol", approved_by: "github:bob", at: 1760000300 }]],
    "GET /orgs/acme/candidates": () => [200, [{ digest: "a".repeat(64), name: "Bounce", version: "1.0.1", state: "waiting-for-approval", uploaded_by: "github:carol", uploaded_at: 1760000400, approved_by: null, approved_at: null }]],
    "GET /orgs/acme/audit": () => [200, { events: [{ seq: 1, at: 1760000000, actor: "github:alice", action: "org.create", target: "org:acme", result: "allowed", reason: null }], chain_ok: true }],
    ...extra,
  };
}
const tabNames = (p) => p.el("tabs").children.map((b) => b.attrs.id.replace("tab-", "")).join(",");
const button = (root, text) => root.all((n) => n.tagName === "BUTTON" && n.textContent === text)[0];

{
  const owner = await page("https://krate.tech/org/?o=acme", DASH, { token: "tok-alice", routes: orgRoutes("github:alice") });
  check(tabNames(owner) === "releases,waiting,people,activity,settings", "/org/ shows an owner all five tabs");
  check(/you are an owner/.test(owner.el("oMeta").textContent), "and says which role they hold");
  check(/Bounce 1\.0\.0/.test(owner.el("pane").shown) && /Sent by @carol · approved by @bob/.test(owner.el("pane").shown), "Releases names who sent and who approved");
  check(button(owner.el("pane"), "Open in Krate") && button(owner.el("pane"), "Copy link"), "each release has Open in Krate and Copy link");
  check(/Waiting for approval|Waiting/.test(owner.el("tabs").shown) && owner.el("tab-waiting").shown.includes("1"), "the Waiting tab counts what is waiting");

  const dan = await page("https://krate.tech/org/?o=acme&tab=activity", DASH, { token: "tok-dan", routes: orgRoutes("github:dan") });
  check(tabNames(dan) === "releases,people", "a recipient's /org/ has Releases and People only");
  check(dan.location.href.endsWith("tab=releases"), "and a link to a tab they cannot see lands on Releases instead");

  // A reviewer approves somebody else's upload, and the confirmation names the digest it sends.
  const bob = await page("https://krate.tech/org/?o=acme&tab=waiting", DASH, {
    token: "tok-bob",
    routes: orgRoutes("github:bob", { [`POST /orgs/acme/candidates/${"a".repeat(64)}/approve`]: () => [200, { release: { digest: "a".repeat(64), url: "x" } }] }),
  });
  const approve = button(bob.el("pane"), "Approve");
  check(!!approve, "a reviewer is offered Approve on carol's upload");
  approve.click();
  check(bob.el("sheetDigest").shown.includes("a".repeat(64)), "the confirmation shows the exact digest");
  check(bob.sent.every((s) => !s.key.endsWith("/approve")), "nothing is approved before the person confirms");
  bob.el("sheetYes").onclick();
  await settle();
  const sentApprove = bob.sent.find((s) => s.key.endsWith("/approve"));
  check(sentApprove && sentApprove.key === `POST /orgs/acme/candidates/${"a".repeat(64)}/approve` && sentApprove.headers["idempotency-key"], "Approve sends that digest, once, with an Idempotency-Key");
  check(/is released/.test(bob.el("toast").shown), "and says it is released");

  const carol = await page("https://krate.tech/org/?o=acme&tab=waiting", DASH, { token: "tok-carol", routes: orgRoutes("github:carol") });
  check(!button(carol.el("pane"), "Approve") && /someone else approves/.test(carol.el("pane").shown), "the uploader is told someone else approves it, and has no Approve button");

  // A refused approval stays on the sheet, in words.
  const late = await page("https://krate.tech/org/?o=acme&tab=waiting", DASH, {
    token: "tok-bob",
    routes: orgRoutes("github:bob", { [`POST /orgs/acme/candidates/${"a".repeat(64)}/approve`]: () => [404, { error: "digest-mismatch", message: "" }] }),
  });
  button(late.el("pane"), "Approve").click();
  late.el("sheetYes").onclick();
  await settle();
  check(/not waiting any more/.test(late.el("sheetWhy").shown) && !/digest-mismatch/.test(late.el("sheetWhy").shown), "a refused approval is explained on the sheet");

  // People: invite by GitHub and by email, then the join link and its expiry.
  const inv = await page("https://krate.tech/org/?o=acme&tab=people", DASH, {
    token: "tok-alice",
    routes: orgRoutes("github:alice", {
      "POST /orgs/acme/invites": (_h, b) => [201, { code: "c0de", expires_at: 1760604800, join_url: "https://krate.tech/join/?o=acme&c=c0de", ...b }],
    }),
  });
  inv.el("invWho").value = "@Bob";
  inv.el("invRole").value = "reviewer";
  button(inv.el("pane"), "Create invitation").click();
  await settle();
  const sentInv = inv.sent.find((s) => s.key === "POST /orgs/acme/invites");
  check(sentInv && sentInv.body.github === "bob" && sentInv.body.role === "reviewer" && !("email" in sentInv.body), "a GitHub invitation sends {github, role}");
  const out = inv.el("invOut");
  check(out.all((n) => n.tagName === "INPUT")[0].value === "https://krate.tech/join/?o=acme&c=c0de" && /until/.test(out.shown), "it shows the join link to copy, and when it expires");
  button(inv.el("pane"), "Work email").click();
  inv.el("invWho").value = "Erin@Acme.test";
  inv.el("invRole").value = "recipient";
  button(inv.el("pane"), "Create invitation").click();
  await settle();
  const sentMail = inv.sent.filter((s) => s.key === "POST /orgs/acme/invites")[1];
  check(sentMail && sentMail.body.email === "erin@acme.test" && !("github" in sentMail.body), "an email invitation sends {email, role}");
  const rows = inv.el("pane").querySelectorAll(".rw");
  const aliceRow = rows.find((r) => /@alice/.test(r.shown));
  check(aliceRow && !button(aliceRow, "Remove") && button(aliceRow, "Leave").attrs["aria-disabled"] === "true", "the only owner's own row holds Leave back");
  button(aliceRow, "Leave").click();
  check(/only owner/i.test(aliceRow.shown), "and pressing it says why");
  check(/Recipients/.test(inv.el("pane").shown) && /erin@acme\.test/.test(inv.el("pane").shown), "recipients are listed apart from members");

  const admin = await page("https://krate.tech/org/?o=acme&tab=people", DASH, {
    token: "tok-x", routes: orgRoutes("github:xadmin", { "GET /orgs/acme": () => [200, { org: ACME, me: member("github:xadmin", "admin"), members: [member("github:alice", "owner"), member("github:xadmin", "admin"), member("github:carol", "developer")] }] }),
  });
  const opts = admin.el("invRole").all((n) => n.tagName === "OPTION").map((o) => o.value).join();
  check(opts === "developer,reviewer,recipient", "an admin's invite offers only the roles an admin may invite");
  const ownerRow = admin.el("pane").querySelectorAll(".rw").find((r) => /@alice/.test(r.shown));
  check(!button(ownerRow, "Remove") && !ownerRow.querySelector("select"), "an admin has no Remove or role control on an owner");

  // Settings and Activity.
  const set = await page("https://krate.tech/org/?o=acme&tab=settings", DASH, { token: "tok-alice", routes: orgRoutes("github:alice") });
  check(/0d92 19f2 81ef db72/.test(set.el("pane").shown) && /by @alice/.test(set.el("pane").shown), "Settings shows the root key's fingerprint and who set it");
  check(/krate publish app\.krate --org acme/.test(set.el("pane").shown) && /krate delegate/.test(set.el("pane").shown) && /krate sign/.test(set.el("pane").shown), "and the commands that send an app to it");
  const act = await page("https://krate.tech/org/?o=acme&tab=activity", DASH, { token: "tok-alice", routes: orgRoutes("github:alice") });
  check(/Chain intact/.test(act.el("pane").shown) && /@alice created the organisation/.test(act.el("pane").shown), "Activity reads in words, with the chain badge");
  const broken = await page("https://krate.tech/org/?o=acme&tab=activity", DASH, {
    token: "tok-alice", routes: orgRoutes("github:alice", { "GET /orgs/acme/audit": () => [200, { events: [], chain_ok: false }] }),
  });
  check(/Chain broken/.test(broken.el("pane").shown), "a broken chain says so");

  // The page-level refusals.
  const removed = await page("https://krate.tech/org/?o=acme", DASH, { token: "tok-erin", routes: { "GET /orgs/acme": () => [403, { error: "removed", message: "" }] } });
  check(/You were removed from acme/.test(removed.el("stop").shown) && removed.el("in").hidden, "a removed member is told so, and sees nothing of the organisation");
  const expired = await page("https://krate.tech/org/?o=acme", DASH, { token: "old", routes: { "GET /orgs/acme": () => [401, { error: "not-signed-in", message: "" }] } });
  check(expired.el("out").hidden === false && expired.localStorage.getItem("krate_tok") === null, "an ended sign-in is dropped and the sign-in choices shown");
  const signedOut = await page("https://krate.tech/org/?o=acme", DASH);
  check(signedOut.el("outChoices").all((n) => n.tagName === "A")[0].href === "/login/?next=%2Forg%2F%3Fo%3Dacme#github", "/org/ signed out offers sign-in that returns to the organisation");
}

/* ---------------------------------------------------------------- /join/ */
{
  const out = await page("https://krate.tech/join/?o=acme&c=abc123def", JOIN);
  check(/invitation to acme/.test(out.el("jH").textContent) && out.el("jBody").all((n) => n.tagName === "A")[0].href === "/login/?next=%2Fjoin%2F%3Fo%3Dacme%26c%3Dabc123def#github", "/join/ signed out offers sign-in that returns to the invitation");

  const inv = { org: { slug: "acme", name: "Acme" }, role: "reviewer", invited_by: { principal: "github:alice", display: "alice" }, for: { principal: "github:bob", display: "bob", kind: "github" }, expires_at: 1760604800, state: "open" };
  const ok = await page("https://krate.tech/join/?o=acme&c=abc123def", JOIN, {
    token: "tok-bob",
    routes: {
      "GET /orgs/acme/invites/abc123def": () => [200, inv],
      "POST /orgs/acme/invites/abc123def/accept": () => [200, { membership: member("github:bob", "reviewer") }],
    },
  });
  check(/Join Acme/.test(ok.el("jH").textContent) && /@alice invited @bob/.test(ok.el("jP").textContent) && /As reviewer/.test(ok.el("jBody").shown), "it says who invited whom, and the role");
  button(ok.el("jBody"), "Accept and join Acme").click();
  await settle();
  check(ok.sent.some((s) => s.key === "POST /orgs/acme/invites/abc123def/accept" && s.headers["idempotency-key"]), "Accept posts to the invitation with an Idempotency-Key");
  check(/You are in Acme/.test(ok.el("jH").textContent) && ok.el("jBody").all((n) => n.tagName === "A")[0].href === "/org/?o=acme", "and then opens the organisation");

  for (const [reason, words] of [["invite-for-someone-else", /for someone else/], ["invite-expired", /expired/], ["invite-used", /already used/], ["invite-revoked", /withdrawn/]]) {
    const p = await page("https://krate.tech/join/?o=acme&c=abc123def", JOIN, {
      token: "tok-carol", routes: { "GET /orgs/acme/invites/abc123def": () => [reason === "invite-for-someone-else" ? 403 : 410, { error: reason, message: "" }], "GET /me": () => [200, { user: { login: "carol" } }] },
    });
    check(words.test(p.el("jH").textContent) && !p.el("jH").textContent.includes(reason) && !/Acme/.test(p.el("card").shown + p.el("jBody").shown), `an invitation that is ${reason.replace("invite-", "")} is said in words, naming no organisation`);
  }
  const broken = await page("https://krate.tech/join/?o=acme", JOIN, { token: "t" });
  check(/incomplete/.test(broken.el("jH").textContent) && broken.sent.length === 0, "a link with no code says so without asking the hub");
}

/* ------------------------------------------------ the private /open/ page */
{
  // The script block that renders the app the link names.
  const at = OPEN.indexOf("var hash;");
  const start = OPEN.lastIndexOf("(function () {", at);
  const end = OPEN.indexOf("\n})();", OPEN.indexOf("function privateApp", at)) + "\n})();".length;
  const code = OPEN.slice(start, end);
  const url = `https://krate.tech/open/?a=${DIGEST}&private=1`;
  const route = (status, body) => ({ [`GET /a/${DIGEST}`]: (h) => (h.accept === "application/json" ? [status, body] : [200, null]) });

  const out = await page(url, code);
  check(/Sign in to open it/.test(out.el("taName").textContent) && out.sent.length === 0, "signed out: sign in, and nothing is asked of the hub");
  check(!/Acme/.test(out.el("theirApp").shown + out.el("pageHead").textContent), "and the organisation is not named");
  check(out.el("taMore").all((n) => n.tagName === "A")[0].href === "/login/?next=" + encodeURIComponent(`/open/?a=${DIGEST}&private=1`) + "#github", "sign-in comes back to the app");

  const mem = await page(url, code, { token: "tok-erin", routes: route(200, { name: "Bounce", version: "1.0.0", size: 13778, org: { slug: "acme", name: "Acme" } }) });
  check(mem.sent.every((s) => !s.url.includes("/meta/")), "a private app never asks /meta");
  check(mem.sent[0].headers.accept === "application/json" && mem.sent[0].headers.authorization === "Bearer tok-erin", "it asks /a/ with the session and Accept: application/json");
  check(/From Acme/.test(mem.el("taKick").textContent) && /Bounce 1\.0\.0/.test(mem.el("taName").textContent) && mem.el("taRow").hidden === false, "a member sees the app, who sent it, and Download");

  const stranger = await page(url, code, { token: "tok-stranger", routes: route(403, { error: "not-a-member", message: "" }) });
  check(/not shared with you/.test(stranger.el("taName").textContent) && !/acme/i.test(stranger.el("theirApp").shown + stranger.el("pageHead").textContent), "a stranger is told it is not shared with them, and learns no organisation");
  check(stranger.el("steps").hidden === true, "and is not walked through opening it");

  const gone = await page(url, code, { token: "tok-erin", routes: route(403, { error: "removed", message: "", org: "acme", org_name: "Acme" }) });
  check(/You were removed from Acme/.test(gone.el("taName").textContent), "a removed member is told which organisation removed them");

  const ended = await page(url, code, { token: "old", routes: route(401, { error: "not-signed-in", message: "" }) });
  check(/Sign in to open it/.test(ended.el("taName").textContent) && ended.localStorage.getItem("krate_tok") === null, "an ended sign-in asks to sign in again");
}

console.log(bad ? `${bad} check(s) failed` : "ok  the organisation pages behave as the contract says");
process.exit(bad ? 1 : 0);
