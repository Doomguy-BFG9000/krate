// The harness the organisation page tests share: a small DOM, a stand-in
// hub that answers in the contract's shapes, and a page runner that loads
// the shipped scripts. Not a test itself (no -test in its name).
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { webcrypto } from "node:crypto";

const KIT = readFileSync("docs/landing/kit/org.js", "utf8");
const ORGS = readFileSync("docs/landing/orgs/orgs.js", "utf8");
// The page loads parts.js (Addendum 3) before dashboard.js; so does this.
const DASH = readFileSync("docs/landing/org/parts.js", "utf8") + "\n;\n" + readFileSync("docs/landing/org/dashboard.js", "utf8");
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

export { KIT, ORGS, DASH, JOIN, OPEN, check, page, settle, storage, P, ACME, member, DIGEST, orgRoutes, tabNames, button };
export const failures = () => bad;
