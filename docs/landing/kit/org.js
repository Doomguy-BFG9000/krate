/* Organisations on krate.tech (IC-947): what /orgs/, /org/, /join/ and the
 * private side of /open/ share.
 *
 * The hub decides everything. These pages ask it, show what it said, and
 * hide only the buttons the role matrix says a role can never use, so a
 * person is not offered a click that is sure to be refused. A refusal that
 * still comes back is shown in words, with the next step, never as a code.
 *
 * Plain functions on purpose: docs/landing/studio/org-pages-test.mjs runs
 * this file as it ships.
 */
(function (root) {
  "use strict";

  var LIVE_HUB = "https://hub.krate.tech";

  /// The hub these pages talk to. Always hub.krate.tech, except on a copy
  /// of the site served from this machine, which may point at a hub on this
  /// machine (`wrangler dev`) through localStorage "krate-dev-hub". Both
  /// sides must be loopback, so krate.tech can never be pointed anywhere.
  function hub(loc, store) {
    loc = loc || root.location;
    store = store || root.localStorage;
    var LOOP = /^(127\.0\.0\.1|localhost|\[::1\])$/;
    if (loc && LOOP.test(loc.hostname || "")) {
      var dev = "";
      try { dev = store.getItem("krate-dev-hub") || ""; } catch (e) {}
      if (/^http:\/\/(127\.0\.0\.1|localhost|\[::1\]):\d{2,5}$/.test(dev)) return dev;
    }
    return LIVE_HUB;
  }

  /// This site's session: what /login/done keeps.
  function token(store) {
    store = store || root.localStorage;
    try { return store.getItem("krate_tok") || ""; } catch (e) { return ""; }
  }

  function forget(store) {
    store = store || root.localStorage;
    try { store.removeItem("krate_tok"); } catch (e) {}
  }

  /// A fresh Idempotency-Key. One per click: a retry of the same click
  /// sends the same key, so a doubled request does the thing once.
  function key() {
    var b = new Uint8Array(16);
    root.crypto.getRandomValues(b);
    var s = "";
    for (var i = 0; i < b.length; i++) s += (b[i] + 256).toString(16).slice(1);
    return s;
  }

  /// One call to the hub. Never throws: answers {ok, status, body}, where a
  /// refusal's body is the contract's {error, message} and a network
  /// failure is {error: "offline"}.
  function api(method, path, body, opts) {
    opts = opts || {};
    var headers = {};
    var tok = opts.token !== undefined ? opts.token : token();
    if (tok) headers.authorization = "Bearer " + tok;
    if (body !== undefined && body !== null) headers["content-type"] = "application/json";
    if (opts.key) headers["idempotency-key"] = opts.key;
    if (opts.accept) headers.accept = opts.accept;
    var f = opts.fetch || root.fetch;
    return f(hub() + path, {
      method: method,
      headers: headers,
      body: body !== undefined && body !== null ? JSON.stringify(body) : undefined,
    }).then(function (r) {
      if (r.status === 204) return { ok: true, status: 204, body: null };
      return r.text().then(function (t) {
        var parsed = null;
        try { parsed = t ? JSON.parse(t) : null; } catch (e) { parsed = null; }
        if (!r.ok && (!parsed || typeof parsed !== "object" || !parsed.error)) {
          parsed = { error: r.status === 401 ? "not-signed-in" : "hub-error", message: "" };
        }
        return { ok: r.ok, status: r.status, body: parsed };
      });
    }, function () {
      return { ok: false, status: 0, body: { error: "offline", message: "" } };
    });
  }

  // ------------------------------------------------------------ people

  /// A principal as a person reads it. Takes the hub's `github:alice` or
  /// `email:x@corp.com`, or an object carrying {principal, display, kind}.
  function who(p) {
    if (p && typeof p === "object") {
      var base = who(p.principal || "");
      if (p.display) base.display = String(p.display);
      if (p.kind) base.kind = String(p.kind);
      base.label = base.kind === "github" ? "@" + base.display : base.display;
      return base;
    }
    var s = String(p || "");
    var m = s.match(/^(github|email):(.+)$/);
    if (m) return { principal: s, kind: m[1], display: m[2], label: m[1] === "github" ? "@" + m[2] : m[2] };
    return { principal: s, kind: "", display: s || "someone", label: s || "someone" };
  }

  // ------------------------------------------------------------- roles

  var ROLES = ["owner", "admin", "developer", "reviewer", "recipient"];
  var ROLE_NAME = { owner: "Owner", admin: "Admin", developer: "Developer", reviewer: "Reviewer", recipient: "Recipient" };
  /// What each role is for, in one line (the same words as docs/book's
  /// organisations page).
  var ROLE_SAYS = {
    owner: "Everything, including the root key and other owners.",
    admin: "Invites and removes people, reads the activity.",
    developer: "Sends apps for approval.",
    reviewer: "Approves apps before they go out.",
    recipient: "Opens the organisation's apps, nothing else.",
  };

  /// The contract's role matrix (CONTRACT.md "Roles"), the same table the
  /// hub enforces. Used only to decide which controls to show.
  var MATRIX = {
    "org.view": { owner: "yes", admin: "yes", developer: "yes", reviewer: "yes", recipient: "own" },
    "invite.owner-admin": { owner: "yes" },
    "invite.other": { owner: "yes", admin: "yes" },
    "role.change": { owner: "yes", admin: "yes" },
    "role.owner": { owner: "yes" },
    "member.remove": { owner: "yes", admin: "not-owners", developer: "self", reviewer: "self", recipient: "self" },
    "root-key.set": { owner: "yes" },
    "candidate.publish": { owner: "yes", developer: "yes" },
    "candidate.approve": { owner: "yes", reviewer: "yes" },
    "release.download": { owner: "yes", admin: "yes", developer: "yes", reviewer: "yes", recipient: "yes" },
    "audit.read": { owner: "yes", admin: "yes" },
  };

  function can(role, action) {
    var row = MATRIX[action];
    return (row && row[role]) || null;
  }

  /// The tabs a role sees on /org/, in order.
  function tabsFor(role) {
    var out = ["releases"];
    // The candidates list is for the people who send, approve or run the
    // organisation; a recipient only ever opens what was released.
    if (can(role, "candidate.publish") || can(role, "candidate.approve") || can(role, "audit.read")) out.push("waiting");
    out.push("people");
    if (can(role, "audit.read")) out.push("activity");
    if (role && role !== "recipient") out.push("settings");
    return out;
  }

  /// The roles `role` may invite, in order.
  function invitable(role) {
    return ROLES.filter(function (r) {
      return can(role, r === "owner" || r === "admin" ? "invite.owner-admin" : "invite.other") === "yes";
    });
  }

  function high(r) { return r === "owner" || r === "admin"; }

  /// What `me` may do to `them` on the People tab: the roles they may be
  /// moved to (empty: no role control) and whether Remove is offered, with
  /// the reason in words when it is shown but cannot be used.
  function memberActions(me, them, owners) {
    var self = me.principal === them.principal;
    var roles = ROLES.filter(function (r) {
      if (r === them.role) return true;
      var action = them.role === "owner" || r === "owner" ? "role.owner" : high(them.role) || high(r) ? "invite.owner-admin" : "role.change";
      return can(me.role, action) === "yes";
    });
    var changeable = roles.length > 1;
    var lastOwner = them.role === "owner" && owners <= 1;
    var rule = can(me.role, "member.remove");
    var removable = rule === "yes" || (rule === "self" && self) || (rule === "not-owners" && (them.role !== "owner" || self));
    var out = {
      // The only owner is not offered another role: the hub would refuse
      // it (last-owner), and a control that can only fail is noise.
      roles: changeable && !lastOwner ? roles : [],
      roleWhy: changeable && lastOwner ? "The only owner stays an owner until someone else is one." : "",
      remove: removable,
      removeLabel: self ? "Leave" : "Remove",
      removeWhy: "",
    };
    if (removable && lastOwner) {
      out.removeWhy = self
        ? "You are the only owner. Make someone else an owner first, then you can leave."
        : "The only owner cannot be removed. Make someone else an owner first.";
    }
    return out;
  }

  // ----------------------------------------------------------- refusals

  /// A refusal in words, with the next step. `ctx` may carry org (slug or
  /// name), here (this page's path, for sign-in to come back to), what
  /// (the thing that was tried, as "invite people"), and invite details.
  function refusal(reason, ctx) {
    ctx = ctx || {};
    var org = ctx.org || "this organisation";
    var signIn = { label: "Sign in", href: signInHref(ctx.here || "/orgs/") };
    var mine = { label: "Your organisations", href: "/orgs/" };
    switch (reason) {
      case "not-signed-in":
        return { title: "Sign in to continue", body: "Your sign-in has ended or was never here. Sign in and you come straight back to this page.", action: signIn };
      case "not-a-member":
        return { title: "You are not in " + org, body: "Only its members can open this. Ask one of its owners to invite you.", action: mine };
      case "removed":
        return { title: "You were removed from " + org, body: "Its apps no longer open for you. Your own apps and your account are untouched. If this is a mistake, ask one of its owners.", action: mine };
      case "role-cannot":
        return { title: "Your role cannot " + (ctx.what || "do that"), body: "In " + org + " that is for " + (ctx.whoCan || "an owner") + ". Nothing changed.", action: null };
      case "last-owner":
        return { title: org + " would have no owner", body: "Nothing changed. Make someone else an owner first, then try again.", action: null };
      case "self-approval":
        return { title: "Someone else approves this one", body: "You sent it, so a second person has to look. Ask a reviewer or another owner in " + org + ".", action: null };
      case "digest-mismatch":
        return { title: "That version is not waiting any more", body: "It was approved, replaced or never sent here. The list below is fresh.", action: null };
      case "not-signed-for-this-org":
        return { title: "This app is not signed for " + org, body: "It needs the organisation's root key, or a release key that root delegated and has not withdrawn. Sign it again and send it again.", action: null };
      case "invite-expired":
        return { title: "This invitation has expired", body: "Invitations last 7 days. Ask " + (ctx.inviter || "the person who invited you") + " for a new one.", action: null };
      case "invite-used":
        return { title: "This invitation was already used", body: "Each one works once. If you accepted it, " + org + " is in your organisations.", action: mine };
      case "invite-revoked":
        return { title: "This invitation was withdrawn", body: "Ask " + (ctx.inviter || "the person who invited you") + " for a new one.", action: null };
      case "invite-for-someone-else":
        return {
          title: "This invitation is for someone else",
          body: (ctx.me ? "You are signed in as " + ctx.me + ". " : "") + "It works only for the account it names. Sign in with that account, or ask for an invitation to yours.",
          action: { label: "Sign in with another account", href: signInHref(ctx.here || "/orgs/") },
        };
      case "slug-taken":
        return { title: (ctx.slug || "That name") + " is taken", body: "Pick another short name. It is part of the organisation's links.", action: null };
      case "slug-invalid":
        return { title: "That short name will not work", body: "Use 2 to 39 lower-case letters, digits and dashes, starting with a letter. For example acme or acme-labs.", action: null };
      case "idempotency-key-reused":
        return { title: "That click was already used", body: "Nothing changed. Press the button again.", action: null };
      case "no-such-org":
        return { title: "There is no organisation called " + (ctx.slug || "that"), body: "Check the link you were sent.", action: mine };
      case "offline":
        return { title: "Krate could not be reached", body: "Check your connection and try again. Nothing changed.", action: null };
      default:
        return { title: "That did not work", body: (ctx.message ? String(ctx.message) + ". " : "") + "Nothing changed. Try again in a moment.", action: null };
    }
  }

  // ------------------------------------------------------------ sign-in

  /// The /login link that comes back to `here` once signed in. `via` goes
  /// straight on to that sign-in ("github", "google", "email").
  function signInHref(here, via) {
    return "/login/?next=" + encodeURIComponent(here || "/") + (via ? "#" + via : "");
  }

  // ------------------------------------------------------------- audit

  function targetOf(t) {
    var s = String(t || "");
    var at = s.indexOf(":");
    if (at < 0) return { kind: s, value: "" };
    return { kind: s.slice(0, at), value: s.slice(at + 1) };
  }

  var SHORT_WHY = {
    "not-signed-in": "not signed in",
    "not-a-member": "not a member",
    removed: "removed from the organisation",
    "role-cannot": "their role cannot",
    "last-owner": "it would leave no owner",
    "self-approval": "they sent it themselves",
    "digest-mismatch": "no such version waiting",
    "not-signed-for-this-org": "not signed for the organisation",
    "invite-expired": "the invitation had expired",
    "invite-used": "the invitation was already used",
    "invite-for-someone-else": "the invitation names someone else",
    "invite-revoked": "the invitation was withdrawn",
    "slug-taken": "the name is taken",
    "idempotency-key-reused": "a reused request",
  };

  /// One audit event as a sentence. `names` maps a digest to "Ledger 1.2".
  function eventWords(ev, names) {
    names = names || {};
    var a = who(ev.actor).label;
    var t = targetOf(ev.target);
    var app = function () { return names[t.value] || "version " + String(t.value).slice(0, 12); };
    var invitee = function () {
      var v = t.value, slash = v.lastIndexOf("/");
      return { p: who(slash > 0 ? v.slice(0, slash) : v).label, role: slash > 0 ? v.slice(slash + 1) : "" };
    };
    var roleWord = function (r) { return ROLE_NAME[r] ? ROLE_NAME[r].toLowerCase() : r; };
    if (ev.result === "denied") {
      var tried = {
        "org.create": "create the organisation",
        "org.view": "open the organisation",
        "invite.create": "invite " + invitee().p,
        "invite.revoke": "withdraw an invitation",
        "invite.accept": "accept an invitation",
        "member.role": "change " + who(t.value).label + "'s role",
        "member.remove": "remove " + who(t.value).label,
        "root-key.set": "set the root key",
        "candidate.publish": "send an app for approval",
        "candidate.approve": "approve " + app(),
        "candidate.list": "see what is waiting",
        "release.list": "see the releases",
        "release.download": "open " + app(),
        "audit.read": "read the activity",
      }[ev.action] || "do " + ev.action;
      // Shown beside a Refused badge, so the sentence says why, not "refused" again.
      return a + " tried to " + tried + ": " + (SHORT_WHY[ev.reason] || ev.reason || "not allowed") + ".";
    }
    switch (ev.action) {
      case "org.create": return a + " created the organisation.";
      case "member.add": return a + " joined as " + roleWord(ev.reason || "owner") + ".";
      case "invite.create": { var i = invitee(); return a + " invited " + i.p + " as " + roleWord(i.role) + "."; }
      case "invite.revoke": {
        var j = invitee();
        return a + " withdrew the invitation for " + j.p + (ev.reason === "issuer removed" ? ", because the person who sent it was removed." : ".");
      }
      case "invite.accept": { var k = invitee(); return a + " accepted the invitation and joined as " + roleWord(k.role) + "."; }
      case "member.role": {
        var ch = String(ev.reason || "").split("->");
        return a + " changed " + who(t.value).label + " from " + roleWord(ch[0]) + " to " + roleWord(ch[1] || "") + ".";
      }
      case "member.remove": {
        var them = who(t.value).label;
        return them === a ? a + " left the organisation." : a + " removed " + them + ".";
      }
      case "root-key.register": return a + " registered the root key " + fingerprint(t.value) + ".";
      case "root-key.rotate": return a + " replaced the root key with " + fingerprint(t.value) + ".";
      case "release-key.revoke": return a + " withdrew the release key " + fingerprint(t.value) + ".";
      case "candidate.publish": return a + " sent " + app() + " for approval.";
      case "candidate.approve": return a + " approved " + app() + ". It is released.";
      case "release.download": return a + " opened " + app() + ".";
      default: return a + " " + ev.action + (ev.target ? " " + ev.target : "") + ".";
    }
  }

  // ------------------------------------------------------------ format

  /// A key's fingerprint as people compare it: its first 16 hex digits in
  /// fours (the audit records the same 16).
  function fingerprint(hex) {
    var h = String(hex || "").toLowerCase().replace(/[^0-9a-f]/g, "").slice(0, 16);
    return h.replace(/(.{4})(?=.)/g, "$1 ");
  }

  var MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  function day(seconds) {
    if (!seconds) return "";
    var d = new Date(seconds * 1000);
    return d.getDate() + " " + MONTHS[d.getMonth()] + " " + d.getFullYear();
  }
  function moment(seconds) {
    if (!seconds) return "";
    var d = new Date(seconds * 1000);
    var hh = String(d.getHours()).padStart(2, "0"), mm = String(d.getMinutes()).padStart(2, "0");
    return day(seconds) + ", " + hh + ":" + mm;
  }

  /// What a release or candidate is called: "Ledger 1.2", else its digest.
  function appName(c) {
    var n = (c && c.name) || "";
    var v = (c && c.version) || "";
    if (!n) return "Version " + String((c && c.digest) || "").slice(0, 12);
    return v ? n + " " + v : n;
  }

  /// The join link for an invitation answer (`join_url`, Addendum 2).
  function joinUrl(answer) {
    return (answer && answer.join_url) || "";
  }

  // ------------------------------------------------------------- DOM

  /// el("p", {class: "x", text: "hi"}, [child, ...])
  function el(tag, attrs, kids) {
    var n = root.document.createElement(tag);
    attrs = attrs || {};
    for (var k in attrs) {
      if (!Object.prototype.hasOwnProperty.call(attrs, k) || attrs[k] === undefined || attrs[k] === null || attrs[k] === false) continue;
      if (k === "text") n.textContent = attrs[k];
      else if (k === "class") n.className = attrs[k];
      else if (k.slice(0, 2) === "on") n.addEventListener(k.slice(2), attrs[k]);
      else if (k === "hidden") n.hidden = true;
      else n.setAttribute(k, attrs[k] === true ? "" : attrs[k]);
    }
    (kids || []).forEach(function (c) {
      if (c === null || c === undefined || c === false) return;
      n.appendChild(typeof c === "string" ? root.document.createTextNode(c) : c);
    });
    return n;
  }

  /// A refusal as a call-out block, its action as a button.
  function refusalBlock(words, cls) {
    return el("div", { class: "call " + (cls || "warn") + " org-why", role: "status" }, [
      el("div", {}, [
        el("b", { text: words.title }),
        el("p", { text: words.body }),
        words.action ? el("a", { class: "btn ghost sm", href: words.action.href, text: words.action.label }) : null,
      ]),
    ]);
  }

  /// The signed-out face: the three ways in, each coming back to `here`.
  function signInChoices(here) {
    return el("div", { class: "stack" }, [
      el("a", { class: "btn dark", href: signInHref(here, "github"), text: "Continue with GitHub" }),
      el("a", { class: "btn ghost", href: signInHref(here, "google"), text: "Continue with Google" }),
      el("a", { class: "btn ghost", href: signInHref(here, "email"), text: "Continue with email" }),
    ]);
  }

  /// Copy text, with the button saying it worked.
  function copy(text, button) {
    var done = function () {
      if (!button) return;
      var was = button.textContent;
      button.textContent = "Copied";
      setTimeout(function () { button.textContent = was; }, 1500);
    };
    try {
      root.navigator.clipboard.writeText(text).then(done, done);
    } catch (e) { done(); }
  }

  /// Hand the browser a release's bytes as <name>.krate. The link alone
  /// carries no sign-in, so the bytes are fetched with it (Addendum 2).
  function download(url, filename) {
    return root.fetch(url, { headers: { authorization: "Bearer " + token() } }).then(function (r) {
      if (!r.ok) {
        return r.text().then(function (t) {
          var b = null;
          try { b = JSON.parse(t); } catch (e) {}
          return { ok: false, body: b && b.error ? b : { error: r.status === 401 ? "not-signed-in" : "hub-error" } };
        });
      }
      return r.blob().then(function (blob) {
        var a = root.document.createElement("a");
        a.href = URL.createObjectURL(blob);
        a.download = filename;
        root.document.body.appendChild(a);
        a.click();
        setTimeout(function () { URL.revokeObjectURL(a.href); a.remove(); }, 4000);
        return { ok: true };
      });
    }, function () { return { ok: false, body: { error: "offline" } }; });
  }

  function fileName(name) {
    var s = String(name || "app").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
    return (s || "app") + ".krate";
  }

  root.KrateOrg = {
    LIVE_HUB: LIVE_HUB, hub: hub, token: token, forget: forget, key: key, api: api,
    who: who, ROLES: ROLES, ROLE_NAME: ROLE_NAME, ROLE_SAYS: ROLE_SAYS, MATRIX: MATRIX,
    can: can, tabsFor: tabsFor, invitable: invitable, memberActions: memberActions,
    refusal: refusal, signInHref: signInHref, eventWords: eventWords, fingerprint: fingerprint,
    day: day, moment: moment, appName: appName, joinUrl: joinUrl, fileName: fileName,
    el: el, refusalBlock: refusalBlock, signInChoices: signInChoices, copy: copy, download: download,
  };
})(typeof window !== "undefined" ? window : globalThis);
