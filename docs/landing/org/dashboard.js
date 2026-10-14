/* /org/?o=<slug>: one organisation's releases, approvals, people, activity
 * and settings (IC-947, CONTRACT.md Addendum 1 and 2).
 *
 * Every tab asks the organisation's store when it opens; nothing here is
 * trusted from an earlier answer. The role matrix only decides which
 * controls are drawn, so nobody is offered a click that is sure to be
 * refused. When the hub refuses anyway, the person reads why, in words.
 */
(function () {
  "use strict";
  var K = window.KrateOrg;
  var el = K.el;
  var $ = function (id) { return document.getElementById(id); };
  var q = new URLSearchParams(location.search);
  var slug = (q.get("o") || "").toLowerCase();
  var HERE = "/org/?o=" + encodeURIComponent(slug);
  var S = { org: null, me: null, members: [], tab: q.get("tab") || "", waiting: 0, names: {} };

  var TAB = {
    releases: { long: "Releases" },
    waiting: { long: "Waiting for approval", short: "Waiting" },
    people: { long: "People" },
    activity: { long: "Activity" },
    billing: { long: "Billing" },
    settings: { long: "Settings" },
  };
  // Filled in at the bottom: the Addendum 3 parts (parts.js).
  var P = null;
  function tabList() { var t = K.tabsFor(roleOf()); return P ? P.tabs(t) : t; }

  // --------------------------------------------------------------- frame
  function signedOut() {
    $("in").hidden = true;
    $("stop").hidden = true;
    var box = $("outChoices");
    box.textContent = "";
    box.appendChild(K.signInChoices(HERE));
    $("out").hidden = false;
  }

  /// A refusal that ends the page: not a member, removed, no such org.
  function stop(reason, message) {
    if (reason === "not-signed-in") { K.forget(); signedOut(); return; }
    $("in").hidden = true;
    var box = $("stop");
    box.textContent = "";
    var w = K.refusal(reason, { org: slug, slug: slug, here: HERE, message: message });
    box.appendChild(el("div", { class: "panel signedout" }, [
      el("img", { class: "mk", src: "/krate-mark-3d-96.png", alt: "" }),
      el("h1", { text: w.title }),
      el("p", { text: w.body }),
      w.action ? el("div", { class: "stack" }, [el("a", { class: "btn dark", href: w.action.href, text: w.action.label })]) : null,
    ]));
    box.hidden = false;
    document.title = w.title;
  }

  /// A refusal inside a pane or a row: `ctx.what` is what was tried.
  function words(res, ctx) {
    ctx = ctx || {};
    ctx.org = ctx.org || (S.org && S.org.name) || slug;
    ctx.slug = slug;
    ctx.here = HERE + "&tab=" + S.tab;
    ctx.message = res.body && res.body.message;
    // What a withdrawn version and an app for other groups say (IC-949, R).
    if (res.body && res.body.withdrawn) ctx.withdrawn = res.body.withdrawn;
    if (res.body && res.body.groups) ctx.groups = res.body.groups;
    return K.refusal(res.body ? res.body.error : "hub-error", ctx);
  }

  function toast(text) {
    var t = $("toast");
    t.textContent = "";
    t.appendChild(el("div", { class: "call ok", role: "status" }, [el("div", { text: text })]));
    t.hidden = false;
    clearTimeout(toast.t);
    toast.t = setTimeout(function () { t.hidden = true; }, 6000);
  }

  function me() { return K.who(S.me); }
  function isMe(p) { return K.who(p).principal === me().principal; }
  function roleOf() { return S.me ? S.me.role : ""; }

  function drawTabs() {
    var bar = $("tabs");
    bar.textContent = "";
    tabList().forEach(function (id) {
      var t = TAB[id];
      var b = el("button", {
        class: "tab", type: "button", role: "tab", id: "tab-" + id,
        "aria-label": t.long + (id === "waiting" && S.waiting ? ", " + S.waiting + " waiting" : ""),
        "aria-selected": S.tab === id ? "true" : "false",
        onclick: function () { show(id); },
      }, [
        el("span", { class: t.short ? "long" : "", text: t.long }),
        t.short ? el("span", { class: "short", text: t.short }) : null,
        id === "waiting" && S.waiting ? el("span", { class: "n", text: String(S.waiting) }) : null,
      ]);
      bar.appendChild(b);
    });
  }

  function show(id) {
    var tabs = tabList();
    if (tabs.indexOf(id) < 0) id = tabs[0];
    S.tab = id;
    try { history.replaceState(null, "", HERE + "&tab=" + id); } catch (e) {}
    drawTabs();
    if (P) P.banner($("banner"));
    var pane = $("pane");
    pane.textContent = "";
    pane.setAttribute("aria-labelledby", "tab-" + id);
    pane.appendChild(el("p", { class: "loading", text: "Loading" }));
    ({ releases: releases, waiting: waiting, people: people, activity: activity, settings: settings, billing: P && P.panes.billing })[id](pane);
  }

  function head(pane, title, sub) {
    pane.textContent = "";
    pane.appendChild(el("div", { class: "phd" }, [el("h2", { text: title }), sub ? el("p", { text: sub }) : null]));
  }

  function failed(pane, res, ctx) {
    var c = el("div", { class: "card" }, [K.refusalBlock(words(res, ctx))]);
    pane.appendChild(c);
  }

  // ------------------------------------------------------------- sheet
  /// The confirmation sheet. `act` runs on Yes and answers a hub result;
  /// a refusal stays on the sheet in words, a success closes it.
  function confirmSheet(opts) {
    var d = $("sheet");
    $("sheetH").textContent = opts.title;
    $("sheetP").textContent = opts.body;
    // Points under the sentence, one consequence each; `key` ones stand out.
    var pts = $("sheetPts");
    pts.textContent = "";
    (opts.points || []).forEach(function (pt) {
      if (pt) pts.appendChild(el("li", { class: pt.key ? "key" : "", text: pt.text || pt }));
    });
    pts.hidden = !pts.children.length;
    var dg = $("sheetDigest");
    dg.textContent = "";
    var box = $("sheetIn"), inp = $("sheetInput");
    box.hidden = !opts.input;
    inp.value = "";
    if (opts.input) { $("sheetInLabel").textContent = opts.input.label; inp.placeholder = opts.input.placeholder || ""; }
    if (opts.digest) {
      dg.appendChild(el("span", { text: opts.digestLabel || "Exact bytes (sha256)" }));
      dg.appendChild(document.createTextNode(opts.digest));
      dg.hidden = false;
    } else dg.hidden = true;
    var why = $("sheetWhy");
    why.hidden = true;
    why.textContent = "";
    var yes = $("sheetYes");
    yes.textContent = opts.yes;
    yes.disabled = false;
    yes.onclick = function () {
      // A check in the page first (a required reason): it keeps the sheet
      // open with Yes still there.
      var said = opts.check ? opts.check(inp.value) : null;
      if (said) {
        why.textContent = "";
        why.appendChild(K.refusalBlock(said, "warn"));
        why.hidden = false;
        inp.focus();
        return;
      }
      yes.disabled = true;
      yes.textContent = opts.busy || "Working";
      opts.act(inp.value).then(function (res) {
        yes.disabled = false;
        yes.textContent = opts.yes;
        if (res.ok) { d.close(); opts.done(res); return; }
        if (res.body.error === "not-signed-in") { d.close(); K.forget(); signedOut(); return; }
        why.appendChild(K.refusalBlock(words(res, opts.ctx), "bad"));
        why.hidden = false;
        yes.hidden = true;
      });
    };
    yes.hidden = false;
    $("sheetNo").textContent = opts.no || "Cancel";
    $("sheetNo").onclick = function () { d.close(); };
    if (typeof d.showModal === "function") d.showModal(); else d.setAttribute("open", "");
    if (opts.input) inp.focus();
  }

  // ---------------------------------------------------------- releases
  /// Releases by app, with the current version, history, withdraw, set
  /// current and audience (releases.js, IC-949 R); the flat list below is
  /// what a hub without those routes still answers.
  function releases(pane) {
    if (R) { R.pane(pane); return; }
    flatReleases(pane);
  }

  function flatReleases(pane) {
    K.api("GET", "/orgs/" + slug + "/releases").then(function (res) {
      head(pane, "Releases", "Approved apps. Everyone in " + (S.org.name || slug) + " can open them; a copied link alone opens nothing.");
      if (!res.ok) return failed(pane, res, { what: "see the releases" });
      var list = Array.isArray(res.body) ? res.body : [];
      var card = el("div", { class: "card rows" });
      pane.appendChild(card);
      if (!list.length) {
        var how = K.can(roleOf(), "candidate.publish")
          ? "Send one with krate publish app.krate --org " + slug + ". It shows in Waiting for approval until someone else approves it."
          : "When a developer sends an app and a reviewer approves it, it appears here.";
        card.appendChild(el("div", { class: "empty" }, [el("b", { text: "Nothing released yet" }), el("p", { text: how })]));
        return;
      }
      list.forEach(function (r) {
        S.names[r.digest] = K.appName(r);
        var note = el("p", { class: "note", hidden: true });
        var offline = P ? P.releaseOffline(r, note) : null;
        var open = el("button", { class: "btn dark sm", type: "button", text: "Open in Krate" });
        var copyBtn = el("button", { class: "btn ghost sm", type: "button", text: "Copy link" });
        open.addEventListener("click", function () {
          open.disabled = true;
          open.textContent = "Getting it";
          var file = K.fileName(r.name);
          K.download(r.url, file).then(function (out) {
            open.disabled = false;
            open.textContent = "Open in Krate";
            note.textContent = "";
            if (out.ok) {
              note.appendChild(document.createTextNode("Saved as " + file + ". Double-click it to open it in Krate. No Krate yet? "));
              note.appendChild(el("a", { href: "/open/", text: "Get it here" }));
              note.appendChild(document.createTextNode("."));
            } else {
              var w = words({ body: out.body }, { what: "open it" });
              note.textContent = w.title + ". " + w.body;
            }
            note.hidden = false;
          });
        });
        copyBtn.addEventListener("click", function () { K.copy(r.url, copyBtn); });
        card.appendChild(el("div", { class: "rw flat" }, [
          el("div", { class: "t" }, [
            el("b", { text: K.appName(r) }),
            el("small", { text: "Sent by " + K.who(r.uploaded_by).label + " · approved by " + K.who(r.approved_by).label + " · " + K.day(r.at) }),
            note,
          ]),
          el("div", { class: "ra" }, [open, copyBtn, offline]),
        ]));
      });
    });
  }

  // ----------------------------------------------------------- waiting
  function countWaiting() {
    if (K.tabsFor(roleOf()).indexOf("waiting") < 0) return;
    K.api("GET", "/orgs/" + slug + "/candidates").then(function (res) {
      if (!res.ok || !Array.isArray(res.body)) return;
      res.body.forEach(function (c) { S.names[c.digest] = K.appName(c); });
      S.waiting = res.body.filter(function (c) { return c.state === "waiting-for-approval"; }).length;
      drawTabs();
    });
  }

  function waiting(pane) {
    K.api("GET", "/orgs/" + slug + "/candidates").then(function (res) {
      head(pane, "Waiting for approval", "Apps sent to " + (S.org.name || slug) + " that nobody can open yet. A reviewer or an owner approves the exact bytes; nobody approves their own.");
      if (!res.ok) return failed(pane, res, { what: "see what is waiting", whoCan: "owners, admins, developers and reviewers" });
      var all = Array.isArray(res.body) ? res.body : [];
      all.forEach(function (c) { S.names[c.digest] = K.appName(c); });
      var list = all.filter(function (c) { return c.state === "waiting-for-approval"; });
      S.waiting = list.length;
      drawTabs();
      var card = el("div", { class: "card rows" });
      pane.appendChild(card);
      if (!list.length) {
        card.appendChild(el("div", { class: "empty" }, [
          el("b", { text: "Nothing is waiting" }),
          el("p", { text: "A developer sends an app with krate publish app.krate --org " + slug + "." }),
        ]));
        return;
      }
      var approver = K.can(roleOf(), "candidate.approve") === "yes";
      // While the organisation is closing nothing new is released
      // (Addendum 3 D), so Approve is not offered then.
      var closing = S.org && S.org.state === "closing";
      list.forEach(function (c) {
        var mine = isMe(c.uploaded_by);
        var acts = null, note = null, btn = null;
        // What it can do, and what changed (IC-950). Built first so the
        // Approve button can wait on its ticks.
        var review = capsReview(c, approver && !mine && !closing);
        if (closing) {
          note = el("p", { class: "note", text: "Approvals stop while " + (S.org.name || slug) + " is closing. If an owner cancels the closing, this can be approved again." });
        } else if (approver && !mine) {
          btn = el("button", {
            class: "btn dark sm", type: "button", text: "Approve",
            onclick: function () { approve(c, review.accepted()); },
          });
          acts = el("div", { class: "ra" }, [btn]);
        } else {
          note = el("p", { class: "note", text: mine
            ? "You sent this one, so someone else approves it."
            : "A reviewer or an owner approves this." });
        }
        card.appendChild(el("div", { class: "rw flat" }, [
          el("div", { class: "t" }, [
            el("b", { text: K.appName(c) }),
            el("small", { text: "Sent by " + K.who(c.uploaded_by).label + " · " + K.moment(c.uploaded_at) }),
            el("small", { class: "dg", text: "sha256 " + c.digest.slice(0, 16) + "…" }),
            review.node,
            note,
          ]),
          acts,
        ]));
        if (btn) review.gate(btn);
      });
    });
  }

  /// What a waiting candidate can do, and what it changes against the
  /// version that opens now (IC-950). A new capability a person is asked
  /// for gets a tick; Approve waits until every one is ticked, and the
  /// hub refuses an approval that leaves one out.
  function capsReview(c, deciding) {
    var caps = Array.isArray(c.capabilities) ? c.capabilities : [];
    var ch = c.changes || { against: null, added: caps, removed: [], kept: 0 };
    var box = el("div", { class: "caps" });
    var ticks = [];
    var line = function (x, extra) {
      return el("li", {}, [
        el("span", { text: x.words || x.cap }),
        x.required ? null : el("span", { class: "opt", text: " (optional)" }),
        x.rationale ? el("small", { text: "Its maker says: " + x.rationale }) : null,
        extra || null,
      ]);
    };
    if (!caps.length && !ch.removed.length) {
      box.appendChild(el("p", { class: "cl", text: "It asks for nothing beyond what every app may do." }));
    } else if (!ch.against) {
      var asked = caps.filter(function (x) { return x.asks; });
      var basic = caps.filter(function (x) { return !x.asks; });
      box.appendChild(el("p", { class: "cl", text: asked.length ? "What it can do once someone allows it:" : "It asks for nothing a person is asked about." }));
      if (asked.length) box.appendChild(el("ul", {}, asked.map(function (x) { return line(x); })));
      if (basic.length) box.appendChild(el("p", { class: "cl sub", text: "And what every app may do: " + basic.map(function (x) { return x.words || x.cap; }).join(", ") + "." }));
    } else {
      var since = ch.against.version
        ? "Compared with " + ch.against.version + ", the version that opens now:"
        : "Compared with the version that opens now:";
      box.appendChild(el("p", { class: "cl", text: since }));
      var newAsked = ch.added.filter(function (x) { return x.asks; });
      var newBasic = ch.added.filter(function (x) { return !x.asks; });
      if (newAsked.length) {
        box.appendChild(el("p", { class: "cl new", text: "New, check each before approving:" }));
        box.appendChild(el("ul", { class: "new" }, newAsked.map(function (x) {
          var tick = el("input", { type: "checkbox", "aria-label": "I checked: " + (x.words || x.cap) });
          tick.disabled = !deciding;
          ticks.push({ cap: x.cap, box: tick });
          return el("li", {}, [el("label", { class: "tick" }, [tick, el("span", { text: x.words || x.cap })]),
            x.required ? null : el("span", { class: "opt", text: " (optional)" }),
            x.rationale ? el("small", { text: "Its maker says: " + x.rationale }) : null]);
        })));
      }
      if (newBasic.length) box.appendChild(el("p", { class: "cl sub", text: "Also new, not asked about: " + newBasic.map(function (x) { return x.words || x.cap; }).join(", ") + "." }));
      if (ch.removed.length) box.appendChild(el("p", { class: "cl sub", text: "No longer asks to: " + ch.removed.map(function (x) { return x.words || x.cap; }).join(", ") + "." }));
      if (!ch.added.length && !ch.removed.length) box.appendChild(el("p", { class: "cl sub", text: "It asks for exactly the same as before." }));
      else if (ch.kept) box.appendChild(el("p", { class: "cl sub", text: "Everything else is as before (" + ch.kept + (ch.kept === 1 ? " permission" : " permissions") + ")." }));
    }
    return {
      node: box,
      accepted: function () { return ticks.filter(function (t) { return t.box.checked; }).map(function (t) { return t.cap; }); },
      newOnes: function () { return ch.against ? ch.added.filter(function (x) { return x.asks; }) : []; },
      gate: function (btn) {
        if (!ticks.length) return;
        var sync = function () {
          var all = ticks.every(function (t) { return t.box.checked; });
          btn.disabled = !all;
          btn.title = all ? "" : "Tick each new capability first";
        };
        ticks.forEach(function (t) { t.box.addEventListener("change", sync); });
        sync();
      },
    };
  }

  function approve(c, accepted) {
    var key = K.key();
    var added = (c.changes && c.changes.against ? c.changes.added : []).filter(function (x) { return x.asks; });
    confirmSheet({
      title: "Approve " + K.appName(c) + "?",
      body: "This releases exactly these bytes to everyone in " + (S.org.name || slug) + ". If the app changes, the new version waits for its own approval.",
      points: added.map(function (x) { return { key: true, text: "New: it can " + (x.words || x.cap) }; }),
      digest: c.digest,
      yes: "Approve and release",
      busy: "Releasing",
      ctx: { what: "approve releases", whoCan: "owners and reviewers" },
      act: function () { return K.api("POST", "/orgs/" + slug + "/candidates/" + c.digest + "/approve", { accept: accepted || [] }, { key: key }); },
      done: function () {
        toast(K.appName(c) + " is released. Everyone in " + (S.org.name || slug) + " can open it now.");
        show("releases");
        countWaiting();
      },
    });
  }

  // ------------------------------------------------------------ people
  function people(pane) {
    K.api("GET", "/orgs/" + slug).then(function (res) {
      if (!res.ok) { head(pane, "People"); return failed(pane, res, {}); }
      take(res.body);
      var role = roleOf();
      head(pane, "People", role === "recipient"
        ? "You receive " + (S.org.name || slug) + "'s apps. Recipients see only themselves here."
        : "Who is in " + (S.org.name || slug) + ", and what each person may do.");
      var roles = K.invitable(role);
      if (roles.length) pane.appendChild(inviteCard(roles));
      var groupsAt = el("div", { hidden: true });

      var active = S.members.filter(function (m) { return (m.state || "active") === "active"; });
      var gone = S.members.filter(function (m) { return m.state === "removed"; });
      var owners = active.filter(function (m) { return m.role === "owner"; }).length;
      var team = active.filter(function (m) { return m.role !== "recipient"; });
      var recips = active.filter(function (m) { return m.role === "recipient"; });
      var card = el("div", { class: "card rows" });
      pane.appendChild(card);
      if (team.length) {
        if (recips.length) card.appendChild(el("p", { class: "grp", text: "Members" }));
        team.forEach(function (m) { card.appendChild(memberRow(m, owners)); });
      }
      if (recips.length) {
        if (role !== "recipient") card.appendChild(el("p", { class: "grp", text: "Recipients: they open the apps, and nothing else" }));
        recips.forEach(function (m) { card.appendChild(memberRow(m, owners)); });
      }
      if (gone.length) {
        var g = el("div", { class: "card rows" }, [el("p", { class: "grp", text: "Removed. What they sent and approved stays on record." })]);
        gone.forEach(function (m) {
          var w = K.who(m);
          g.appendChild(el("div", { class: "rw flat" }, [
            el("span", { class: "who", "aria-hidden": "true", text: w.display.slice(0, 1).toUpperCase() }),
            el("div", { class: "t" }, [
              el("b", { text: w.label }),
              el("small", { text: "Was " + (K.ROLE_NAME[m.role] || m.role).toLowerCase() + (m.removed_at ? " · removed " + K.day(m.removed_at) : "") + (m.removed_by ? " by " + K.who(m.removed_by).label : "") }),
            ]),
          ]));
        });
        pane.appendChild(g);
      }
      // Groups (IC-949, R), below the people they are made of.
      pane.appendChild(groupsAt);
      if (R) R.groupsCard(groupsAt, active);
    });
  }

  function memberRow(m, owners) {
    var w = K.who(m);
    var mine = isMe(m.principal);
    var a = K.memberActions({ principal: me().principal, role: roleOf() }, { principal: w.principal, role: m.role }, owners);
    var note = el("p", { class: "note", hidden: true });
    var acts = [];
    if (a.roles.length) {
      var sel = el("select", { class: "sel", "aria-label": "Role for " + w.label });
      a.roles.forEach(function (r) {
        var o = el("option", { value: r, text: K.ROLE_NAME[r] });
        if (r === m.role) o.selected = true;
        sel.appendChild(o);
      });
      sel.addEventListener("change", function () { changeRole(m, sel, note); });
      acts.push(sel);
    } else {
      acts.push(el("span", { class: "tag" + (m.role === "owner" ? " blue" : ""), text: K.ROLE_NAME[m.role] || m.role }));
    }
    if (a.remove) {
      var rm = el("button", { class: "btn quiet sm" + (a.removeWhy ? " soft-off" : ""), type: "button", text: a.removeLabel });
      if (a.removeWhy) {
        rm.setAttribute("aria-disabled", "true");
        rm.title = a.removeWhy;
        rm.addEventListener("click", function () { note.textContent = a.removeWhy; note.hidden = false; });
      } else {
        rm.addEventListener("click", function () { removeMember(m, mine); });
      }
      acts.push(rm);
    }
    return el("div", { class: "rw" }, [
      el("span", { class: "who", "aria-hidden": "true", text: w.display.slice(0, 1).toUpperCase() }),
      el("div", { class: "t" }, [
        el("b", {}, [w.label + (mine ? " (you)" : ""), el("span", { class: "tag kind", text: w.kind === "email" ? "Email" : "GitHub" })]),
        el("small", { text: K.ROLE_SAYS[m.role] + (m.since ? " Since " + K.day(m.since) + "." : "") }),
        note,
      ]),
      el("div", { class: "ra" }, acts),
    ]);
  }

  function changeRole(m, sel, note) {
    var was = m.role, to = sel.value, w = K.who(m);
    sel.disabled = true;
    K.api("PUT", "/orgs/" + slug + "/members/" + encodeURIComponent(w.principal), { role: to }, { key: K.key() }).then(function (res) {
      sel.disabled = false;
      if (res.ok) {
        toast(w.label + " is now " + (K.ROLE_NAME[to] || to).toLowerCase() + ".");
        show("people");
        return;
      }
      if (res.body.error === "not-signed-in") { K.forget(); signedOut(); return; }
      sel.value = was;
      var x = words(res, { what: "change that role" });
      note.textContent = x.title + ". " + x.body;
      note.hidden = false;
    });
  }

  function removeMember(m, mine) {
    var w = K.who(m), name = S.org.name || slug;
    var key = K.key();
    confirmSheet({
      title: mine ? "Leave " + name + "?" : "Remove " + w.label + " from " + name + "?",
      body: mine
        ? "Its apps stop opening for you at once. Your own apps and your account stay as they are. To come back you need a new invitation."
        : "Their access ends at once, the next time they open an app. Their own apps and account are untouched, and what they sent or approved stays on record.",
      yes: mine ? "Leave" : "Remove",
      busy: mine ? "Leaving" : "Removing",
      ctx: { what: "remove " + w.label, whoCan: "an owner, or an admin for people who are not owners" },
      act: function () { return K.api("DELETE", "/orgs/" + slug + "/members/" + encodeURIComponent(w.principal), null, { key: key }); },
      done: function () {
        if (mine) { location.href = "/orgs/"; return; }
        toast(w.label + " was removed. Their next open is refused.");
        show("people");
      },
    });
  }

  function inviteCard(roles) {
    var by = "github";
    var card = el("div", { class: "card" });
    var seg = el("div", { class: "seg", role: "group", "aria-label": "Invite by" });
    var input = el("input", { class: "inp", id: "invWho", autocomplete: "off", autocapitalize: "none", spellcheck: "false", "aria-label": "GitHub username", placeholder: "GitHub username, like octocat" });
    var sel = el("select", { class: "sel", id: "invRole", "aria-label": "Role" });
    var says = el("p", { class: "rsays" });
    var out = el("div", { id: "invOut" });
    function setBy(v) {
      by = v;
      [].forEach.call(seg.children, function (b) { b.setAttribute("aria-pressed", b.dataset.by === v ? "true" : "false"); });
      input.type = v === "email" ? "email" : "text";
      input.placeholder = v === "email" ? "Work email, like sam@acme.com" : "GitHub username, like octocat";
      input.setAttribute("aria-label", v === "email" ? "Work email" : "GitHub username");
      input.value = "";
      input.focus();
    }
    [["github", "GitHub username"], ["email", "Work email"]].forEach(function (p) {
      var b = el("button", { type: "button", text: p[1], "aria-pressed": p[0] === by ? "true" : "false" });
      b.dataset.by = p[0];
      b.addEventListener("click", function () { setBy(p[0]); });
      seg.appendChild(b);
    });
    roles.forEach(function (r) {
      var o = el("option", { value: r, text: K.ROLE_NAME[r] });
      if (r === "developer") o.selected = true;
      sel.appendChild(o);
    });
    var sayRole = function () { says.textContent = K.ROLE_NAME[sel.value] + ": " + K.ROLE_SAYS[sel.value]; };
    sel.addEventListener("change", sayRole);
    sayRole();
    var go = el("button", { class: "btn dark", type: "submit", text: "Create invitation" });
    var form = el("form", { novalidate: true }, [el("div", { class: "invrow" }, [input, sel, says, go])]);
    var pending = null;
    form.addEventListener("submit", function (e) {
      e.preventDefault();
      var v = input.value.trim();
      var body = { role: sel.value };
      if (by === "github") {
        v = v.replace(/^@/, "").toLowerCase();
        if (!/^[a-z0-9](?:[a-z0-9-]{0,38})$/.test(v)) return hint("That is not a GitHub username", "A username is letters, digits and dashes, like octocat. It is the name in their github.com address.");
        body.github = v;
      } else {
        v = v.toLowerCase();
        if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(v)) return hint("That is not an email address", "Use the address they sign in with, like sam@acme.com.");
        body.email = v;
      }
      var sig = JSON.stringify(body);
      if (!pending || pending.sig !== sig) pending = { sig: sig, key: K.key() };
      go.disabled = true;
      go.textContent = "Creating";
      K.api("POST", "/orgs/" + slug + "/invites", body, { key: pending.key }).then(function (res) {
        go.disabled = false;
        go.textContent = "Create invitation";
        out.textContent = "";
        if (!res.ok) {
          if (res.body.error === "not-signed-in") { K.forget(); signedOut(); return; }
          out.appendChild(K.refusalBlock(words(res, { what: "invite a " + body.role, whoCan: body.role === "owner" || body.role === "admin" ? "an owner" : "an owner or an admin" }), "bad"));
          return;
        }
        pending = null;
        input.value = "";
        out.appendChild(inviteResult(res.body, body));
      });
    });
    function hint(t, b) {
      out.textContent = "";
      out.appendChild(K.refusalBlock({ title: t, body: b, action: null }, "warn"));
      input.focus();
    }
    card.appendChild(el("h3", { text: "Invite someone" }));
    card.appendChild(el("p", { text: "By GitHub username, or by the work email they sign in with. You get a link to send them; it works once, only for them." }));
    card.appendChild(el("div", { style: "margin-top:14px" }, [seg]));
    card.appendChild(form);
    card.appendChild(out);
    return card;
  }

  function inviteResult(ans, sent) {
    var whoLabel = sent.github ? "@" + sent.github : sent.email;
    var link = K.joinUrl(ans);
    var box = el("div", { class: "invres" });
    var inp = el("input", { class: "inp", readonly: true, value: link, "aria-label": "Join link" });
    var cp = el("button", { class: "btn ghost", type: "button", text: "Copy link", onclick: function () { inp.select(); K.copy(link, cp); } });
    var withdraw = el("button", { class: "btn link", type: "button", text: "Withdraw this invitation" });
    withdraw.addEventListener("click", function () {
      withdraw.disabled = true;
      K.api("DELETE", "/orgs/" + slug + "/invites/" + encodeURIComponent(ans.code), null, { key: K.key() }).then(function (res) {
        withdraw.disabled = false;
        box.textContent = "";
        if (res.ok) {
          box.appendChild(el("p", { text: "Withdrawn. The link for " + whoLabel + " no longer works." }));
          return;
        }
        box.appendChild(K.refusalBlock(words(res, { what: "withdraw invitations" }), "bad"));
      });
    });
    // Mailed or not (Addendum 3 A): say which, and why not.
    var mail = P.inviteMail(ans, sent);
    box.appendChild(el("p", { class: "invfor", text: "Invitation for " + whoLabel + " as " + (K.ROLE_NAME[sent.role] || sent.role).toLowerCase() }));
    box.appendChild(el("b", { text: mail.title }));
    box.appendChild(el("p", { text: mail.body }));
    box.appendChild(el("div", { class: "linkbox" }, [inp, cp]));
    box.appendChild(withdraw);
    return box;
  }

  // ---------------------------------------------------------- activity
  function activity(pane) {
    var names = K.api("GET", "/orgs/" + slug + "/candidates").then(function (res) {
      if (res.ok && Array.isArray(res.body)) res.body.forEach(function (c) { S.names[c.digest] = K.appName(c); });
    });
    var audit = K.api("GET", "/orgs/" + slug + "/audit");
    Promise.all([names, audit]).then(function (both) {
      var res = both[1];
      head(pane, "Activity", "Every change and every refused attempt, newest first. Each entry is chained to the one before it.");
      if (!res.ok) return failed(pane, res, { what: "read the activity", whoCan: "owners and admins" });
      var ev = (res.body.events || []).slice().reverse();
      var ok = res.body.chain_ok === true;
      var badge = el("div", { class: "card" }, [
        el("div", { class: "row", style: "display:flex;align-items:center;gap:12px;flex-wrap:wrap" }, [
          el("span", { class: "tag chain " + (ok ? "ok" : "warn"), text: ok ? "Chain intact" : "Chain broken" }),
          el("span", { class: "sub", style: "margin:0", text: ok
            ? "The hub rechecked all " + ev.length + " entries just now. None was changed after it was written."
            : "An entry was changed after it was written. The hub checks the whole chain every time it is read; tell Krate support." }),
        ]),
      ]);
      pane.appendChild(badge);
      var card = el("div", { class: "card rows" });
      pane.appendChild(card);
      var shown = 0;
      function more() {
        ev.slice(shown, shown + 60).forEach(function (e) {
          var denied = e.result === "denied";
          card.appendChild(el("div", { class: "ev" + (denied ? " denied" : "") }, [
            el("p", {}, [denied ? el("span", { class: "tag warn", text: "Refused" }) : null, K.eventWords(e, S.names)]),
            el("time", { datetime: new Date(e.at * 1000).toISOString(), text: K.moment(e.at) }),
          ]));
        });
        shown += 60;
        if (shown < ev.length) {
          var b = el("button", { class: "btn ghost sm", type: "button", text: "Show older", style: "margin:12px 0" });
          b.addEventListener("click", function () { b.remove(); more(); });
          card.appendChild(b);
        }
      }
      more();
    });
  }

  // ---------------------------------------------------------- settings
  function settings(pane) {
    K.api("GET", "/orgs/" + slug).then(function (res) {
      head(pane, "Settings", "The organisation's root key, and how an app is sent to " + (S.org.name || slug) + ".");
      if (!res.ok) return failed(pane, res, {});
      take(res.body);
      var o = S.org, owner = roleOf() === "owner";
      if (o.state === "closed") {
        head(pane, "Settings", (o.name || slug) + " is closed. Its export is what remains.");
        P.settings(pane, null);
        return;
      }
      var key = el("div", { class: "card" }, [el("h3", { text: "Root key" })]);
      if (o.root_key) {
        key.appendChild(el("p", { text: "Every app sent here is signed by this key, or by a release key it delegated. Compare the fingerprint with the owner's copy." }));
        key.appendChild(el("p", { class: "fp", text: K.fingerprint(o.root_key) }));
        key.appendChild(el("dl", { class: "kv" }, [
          el("dt", { text: "Set" }), el("dd", { text: (K.moment(o.root_key_set_at) || "Unknown") + (o.root_key_set_by ? " by " + K.who(o.root_key_set_by).label : "") }),
          el("dt", { text: "Full key" }), el("dd", { class: "dg", text: o.root_key }),
        ]));
        if (owner) key.appendChild(el("p", { class: "sub", text: "Replace it here, or run the command below with the new key. Activity records the change." }));
      } else {
        key.appendChild(el("p", { text: "No root key yet. Until an owner registers one, nothing can be sent to " + (o.name || slug) + "." }));
      }
      if (owner) key.appendChild(el("pre", { class: "k", text: "krate org root-key " + slug + " " + slug + "-root.pkcs8" }));
      pane.appendChild(key);

      var steps = [
        ["Once, an owner gives each developer a release key", "krate delegate \\\n  --root " + slug + "-root.pkcs8 \\\n  --key release.pkcs8 \\\n  --namespace '" + slug + "/*' \\\n  -o release-" + slug + ".json"],
        ["The developer marks the app as yours and signs it", "krate sign app.krate --org " + slug + " \\\n  --key release.pkcs8 \\\n  --delegation release-" + slug + ".json"],
        ["And sends it here", "krate publish app.krate --org " + slug],
      ];
      pane.appendChild(el("div", { class: "card" }, [
        el("h3", { text: "Send an app to " + (o.name || slug) }),
        el("p", { text: "Three commands. The app then waits in Waiting for approval until someone other than the sender approves it." }),
        el("ol", { class: "cmds" }, steps.map(function (s) {
          return el("li", {}, [el("span", { text: s[0] }), el("pre", { class: "k", text: s[1] })]);
        })),
      ]));

      pane.appendChild(el("div", { class: "card" }, [
        el("h3", { text: "Organisation" }),
        el("dl", { class: "kv" }, [
          el("dt", { text: "Name" }), el("dd", { text: o.name || slug }),
          el("dt", { text: "Short name" }), el("dd", { text: o.slug || slug }),
          el("dt", { text: "Created" }), el("dd", { text: K.day(o.created_at) + (o.created_by ? " by " + K.who(o.created_by).label : "") }),
        ]),
      ]));
      P.settings(pane, key);
    });
  }

  // -------------------------------------------------------------- start
  function take(body) {
    S.org = body.org || S.org;
    S.me = body.me || S.me;
    S.members = Array.isArray(body.members) ? body.members : S.members;
    if (P) P.banner($("banner"));
  }

  /// Ask the store again after a change to the organisation itself
  /// (closing, cancelling, an objection): the banner and tabs follow.
  function refresh() {
    K.api("GET", "/orgs/" + slug).then(function (res) {
      if (!res.ok) { stop(res.body.error, res.body.message); return; }
      take(res.body);
      show(S.tab);
    });
  }

  // Release control and groups (releases.js); absent, the flat list.
  var R = null;
  P = window.KrateOrgParts({
    S: S, slug: slug, words: words, confirmSheet: confirmSheet, head: head, failed: failed,
    toast: toast, show: function (id) { show(id); }, refresh: refresh, roleOf: roleOf, isMe: isMe,
  });
  if (window.KrateOrgReleases) {
    R = window.KrateOrgReleases({
      S: S, slug: slug, words: words, confirmSheet: confirmSheet, head: head, failed: failed,
      toast: toast, show: function (id) { show(id); }, roleOf: roleOf, P: P, flat: flatReleases,
    });
  }

  if (!slug) { location.replace("/orgs/"); return; }
  if (!/^[a-z][a-z0-9-]{1,38}$/.test(slug)) { stop("no-such-org"); return; }
  if (!K.token()) { signedOut(); return; }

  K.api("GET", "/orgs/" + slug).then(function (res) {
    if (!res.ok) { stop(res.body.error, res.body.message); return; }
    take(res.body);
    var name = S.org.name || slug;
    document.title = name + " on Krate";
    $("oName").textContent = name;
    $("oTile").textContent = name.slice(0, 1).toUpperCase();
    var r = K.ROLE_NAME[roleOf()] || roleOf();
    $("oMeta").textContent = slug + " · you are " + (/^[AEIOU]/.test(r) ? "an " : "a ") + r.toLowerCase();
    $("in").hidden = false;
    if (q.get("new") === "1") toast(name + " is ready, and you are its owner. Invite your team below.");
    // Back from a company sign-in: what it did, in words.
    var signedVia = K.takeSsoNotice(undefined, name);
    if (signedVia) toast(signedVia);
    show(S.tab);
    countWaiting();
  });
})();
