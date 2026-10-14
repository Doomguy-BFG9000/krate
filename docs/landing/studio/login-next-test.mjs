// Sign-in returns to the page it came from, and only to a page on this
// site (IC-947): /login?next=<path> is no open redirect.
//
// Runs the SHIPPED /login/signin.js, /login's script and /login/done's in
// a small fake browser, the same way signin-test.mjs does. The control
// matters: the same harness DOES follow a same-site next, so a refused
// off-site one is the check at work, not a harness that never navigates.
//
//   node docs/landing/studio/login-next-test.mjs
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { webcrypto } from "node:crypto";

const signinJs = readFileSync("docs/landing/login/signin.js", "utf8");
const loginHtml = readFileSync("docs/landing/login/index.html", "utf8");
const doneHtml = readFileSync("docs/landing/login/done/index.html", "utf8");

function inlineScript(html) {
  const blocks = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]);
  return blocks[blocks.length - 1];
}

let bad = 0;
function check(ok, name) {
  if (!ok) bad++;
  console.log(`${ok ? "ok  " : "FAIL"} ${name}`);
}

/* ---- the rule itself ---------------------------------------------------- */
{
  const ctx = { URL, crypto: webcrypto, Uint8Array, localStorage: { getItem: () => null, setItem() {}, removeItem() {} } };
  ctx.window = ctx;
  vm.createContext(ctx);
  vm.runInContext(signinJs, ctx);
  const safe = (n) => ctx.KrateSignIn.safeNext(n, "https://krate.tech");
  for (const ok of ["/org/?o=acme&tab=people", "/join/?o=acme&c=abc123", `/open/?a=${"0".repeat(64)}&private=1`, "/orgs/"]) {
    check(safe(ok) === ok, `a page on this site is a place to come back to: ${ok}`);
  }
  const OFF = [
    "//evil.example/steal",
    "/\\evil.example",
    "\\\\evil.example",
    "https://evil.example/",
    "http:evil.example",
    "javascript:alert(1)",
    "/\t/evil.example",
    "/\n/evil.example",
    " /org/",
    "//krate.tech.evil.example/",
    "studio",
    "",
  ];
  for (const off of OFF) check(safe(off) === "", `refused as a place to go: ${JSON.stringify(off)}`);
  check(safe("/org/?o=acme#people") === "/org/?o=acme#people", "a fragment on a same-site path survives");
}

/* ---- the round trip ------------------------------------------------------ */
function storage() {
  const m = new Map();
  return { getItem: (k) => (m.has(k) ? m.get(k) : null), setItem: (k, v) => m.set(k, String(v)), removeItem: (k) => m.delete(k) };
}
function element() {
  const classes = new Set(["hidden"]);
  return {
    textContent: "", href: "", disabled: false, firstChild: { textContent: "" },
    classList: { add: (c) => classes.add(c), remove: (c) => classes.delete(c), contains: (c) => classes.has(c) },
    addEventListener() {}, focus() {},
  };
}
function browser() {
  const localStorage = storage();
  function visit(url, html) {
    const u = new URL(url);
    const els = {};
    const location = {
      search: u.search, hash: u.hash, pathname: u.pathname, origin: u.origin, hostname: u.hostname, href: u.href,
      replace(to) { this.href = to; this.went = to; },
    };
    const ctx = {
      localStorage, location, URLSearchParams, URL, Date, JSON, crypto: webcrypto, Uint8Array, setTimeout,
      history: { replaceState() { location.hash = ""; } },
      addEventListener() {},
      document: {
        title: "",
        documentElement: { className: "", classList: { contains: () => false, add() {}, remove() {} } },
        getElementById: (id) => (els[id] ||= element()),
        querySelector: () => element(),
        body: { insertAdjacentHTML() {} },
      },
      fetch: () => new Promise(() => {}),
    };
    ctx.window = ctx;
    vm.createContext(ctx);
    vm.runInContext(signinJs, ctx);
    vm.runInContext(inlineScript(html), ctx);
    return { els, location };
  }
  return { localStorage, visit };
}
const fragment = (f) => "#" + new URLSearchParams(f).toString();

function roundTrip(next) {
  const b = browser();
  const login = b.visit("https://krate.tech/login/?next=" + encodeURIComponent(next), loginHtml);
  const nonce = new URL(login.els.ghBtn.href).searchParams.get("nonce");
  const done = b.visit("https://krate.tech/login/done/" + fragment({ token: "krs_mine", login: "bob", nonce }), doneHtml);
  return { b, login, done };
}

{
  const { b, login, done } = roundTrip("/join/?o=acme&c=abc123");
  check(/accept your invitation/.test(login.els.loginH.textContent), "/login says why when an invitation sent the person there");
  check(done.location.went === "/join/?o=acme&c=abc123", "after signing in, the person is back on the invitation");
  check(b.localStorage.getItem("krate_tok") === "krs_mine", "and signed in");
  check(b.localStorage.getItem("krate_next") === null, "and the continuation is used up");
}
{
  const { done } = roundTrip("/org/?o=acme&tab=people");
  check(done.location.went === "/org/?o=acme&tab=people", "an organisation's tab is where the person returns");
}
for (const off of ["//evil.example/steal", "https://evil.example/", "/\\evil.example", "javascript:alert(1)"]) {
  const { b, done } = roundTrip(off);
  check(done.location.went === undefined && !String(done.location.href).includes("evil"), `an off-site next is refused: ${JSON.stringify(off)}`);
  check(b.localStorage.getItem("krate_tok") === "krs_mine" && /All set/.test((done.els.doneSub || {}).textContent || ""), "and the person is still signed in, on the done page");
}
{
  // The named continuations are untouched.
  const { done } = roundTrip("studio");
  check(done.location.went === "/app/", "next=studio still opens the Studio");
}

{
  // Studio names the organisation in the fragment (IC-953), because
  // Windows' `start` drops everything after a bare `&`: the company's own
  // sign-in is offered first, as with ?org=.
  const page = browser().visit("https://krate.tech/login?app_nonce=abc123#org=acme", loginHtml);
  const door = new URL(page.els.orgBtn.href);
  check(door.pathname === "/login/oidc/start" && door.searchParams.get("org") === "acme", "#org=<slug> offers that organisation's company sign-in");
  check(door.searchParams.get("app_nonce") === "abc123", "and keeps Studio's nonce for the way back");
  check(/Sign in to acme/.test(page.els.loginH.textContent), "and says which organisation");
  const odd = browser().visit("https://krate.tech/login?app_nonce=abc123#org=Not%20A%20Slug!", loginHtml);
  check(!(odd.els.orgBtn && odd.els.orgBtn.href), "a fragment that is not a slug offers nothing");
}

console.log(bad ? `${bad} check(s) failed` : "ok  sign-in comes back to this site's pages, and nowhere else");
process.exit(bad ? 1 : 0);
