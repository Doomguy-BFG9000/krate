/* The organisation dashboard's company connections (IC-952): the
 * Connections card in Settings.
 *
 * A connection is an API's address and credential, set up once by an owner
 * or admin. The organisation's apps reach it by name (net.connection:<name>)
 * and never hold the credential: Krate adds it to their requests. The card
 * lists each connection (address, what it allows, who and which apps may
 * use it, when its credential was set and how often it was used), and lets
 * an owner or admin make one, replace its credential, change what it allows
 * and who uses it, and remove it. The credential is typed once and never
 * shown again: the hub answers only who set it and when.
 *
 * dashboard.js owns the page and hands this part what it needs (D). Like the
 * rest of the page it hides only what the role matrix says a role can never
 * use, and says refusals in words.
 */
(function (root) {
  "use strict";

  var METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE"];

  /// The entry side of the credential seam (F-316): what this page sends the
  /// hub for a credential. Today it goes as typed, over TLS, and the hub
  /// seals it at rest. Under the organisation keyring this seals it to the
  /// organisation's public key in the browser first, so the hub only ever
  /// holds the blob. Every credential the page sends passes through here.
  function sealForHub(body) {
    return Promise.resolve(body);
  }

  root.KrateOrgConnections = function (D) {
    var K = root.KrateOrg;
    var el = K.el;
    var slug = D.slug;
    var path = function (p) { return "/orgs/" + slug + "/connections" + (p || ""); };
    var nameOf = function () { return (D.S.org && D.S.org.name) || slug; };
    var can = function (action) { return K.can(D.roleOf(), action) === "yes"; };
    var ctx = { what: "set up or change connections", whoCan: "owners and admins" };

    function said(list, last) {
      var l = (list || []).slice();
      if (l.length <= 1) return l.join("");
      return l.slice(0, -1).join(", ") + " " + (last || "and") + " " + l[l.length - 1];
    }

    function scopeWords(c) {
      var who = Array.isArray(c.audience) && c.audience.length ? "Only " + K.groupsSaid(c.audience) : "Everyone in " + nameOf();
      var apps = Array.isArray(c.apps) && c.apps.length ? said(c.apps) : "any of its apps";
      return who + " · " + apps;
    }

    function allowWords(c) {
      var a = c.allow || {};
      var paths = a.paths || ["/"];
      var where = paths.length === 1 && paths[0] === "/" ? "everything under it" : said(paths);
      return said(a.methods || ["GET"], "or") + " on " + where;
    }

    /// Settings: the card, for whoever may see connections.
    function settings(pane) {
      if (!can("connection.view")) return;
      var card = el("div", { class: "card conn", id: "connCard" }, [
        el("h3", { text: "Company connections" }),
        el("p", { text: "An API's address and credential, set up once. Apps in " + nameOf() + " reach it by name and never hold the credential: Krate adds it to their requests." }),
        el("p", { class: "loading", text: "Loading" }),
      ]);
      pane.appendChild(card);
      draw(card);
    }

    function draw(card) {
      while (card.children.length > 2) card.lastChild.remove();
      card.appendChild(el("p", { class: "loading", text: "Loading" }));
      K.api("GET", path()).then(function (res) {
        card.lastChild.remove();
        // A hub from before connections answers 404 here: no card at all,
        // rather than one that can only fail.
        if (!res.ok && res.status === 404 && res.body.error !== "no-such-org") { card.remove(); return; }
        if (!res.ok) { card.appendChild(K.refusalBlock(D.words(res, { what: "see the connections", whoCan: "owners, admins, developers and reviewers" }))); return; }
        var list = (res.body && res.body.connections) || [];
        var rows = el("div", { class: "rows connrows" });
        if (!list.length) {
          rows.appendChild(el("div", { class: "empty" }, [
            el("b", { text: "No connections yet" }),
            el("p", { text: can("connection.manage")
              ? "Add one for an API your apps call, like your finance system. The credential stays with " + nameOf() + "."
              : "An owner or admin of " + nameOf() + " adds them here." }),
          ]));
        }
        list.forEach(function (c) { rows.appendChild(row(card, c)); });
        card.appendChild(rows);
        if (can("connection.manage")) {
          var box = el("div", { class: "connform", hidden: true });
          var add = el("button", { class: "btn dark sm", type: "button", text: "New connection" });
          add.addEventListener("click", function () {
            if (!box.hidden) { box.hidden = true; return; }
            box.textContent = "";
            box.appendChild(createForm(card, box));
            box.hidden = false;
            var first = box.querySelector("input");
            if (first) first.focus();
          });
          card.appendChild(el("div", { class: "row2" }, [add]));
          card.appendChild(box);
        }
        card.appendChild(el("div", { class: "connuse" }, [
          el("span", { text: "An app uses one by naming it in its manifest.toml:" }),
          el("pre", { class: "k", text: "[[capabilities]]\ncap = \"net.connection:" + (list[0] ? list[0].name : "finance-api") + "\"\nrationale = \"Read invoices\"" }),
        ]));
      });
    }

    function row(card, c) {
      var note = el("p", { class: "note", hidden: true });
      var cred = c.credential || {};
      var uses = c.uses || {};
      var facts = [
        el("b", {}, [c.name, " ", el("span", { class: "tag blue", text: allowWords(c) })]),
        el("small", { class: "dg", text: c.base }),
        el("small", { text: scopeWords(c) }),
        el("small", { text: "Credential: the " + (cred.kind === "basic" ? "user and password" : cred.header + " header") + ", set by " + K.who(cred.set_by).label + " on " + K.day(cred.set_at) +
          " · used " + (uses.today || 0) + (uses.today === 1 ? " time" : " times") + " today, " + (uses.last_30_days || 0) + " in 30 days" }),
        note,
      ];
      var r = el("div", { class: "rw flat connrow" }, [el("div", { class: "t" }, facts)]);
      if (!can("connection.manage")) return r;
      var box = el("div", { class: "connform", hidden: true });
      var open = function (make) {
        return function () {
          var showing = !box.hidden && box.dataset.what === make.name;
          box.textContent = "";
          box.hidden = showing;
          if (showing) return;
          box.dataset.what = make.name;
          box.appendChild(make(card, c, box));
          var first = box.querySelector("input");
          if (first) first.focus();
        };
      };
      r.appendChild(el("div", { class: "ra manage" }, [
        el("button", { class: "btn ghost sm", type: "button", text: "Replace credential", onclick: open(credentialForm) }),
        el("button", { class: "btn ghost sm", type: "button", text: "Change", onclick: open(changeForm) }),
        el("button", { class: "btn quiet sm", type: "button", text: "Remove", onclick: function () { remove(card, c); } }),
      ]));
      r.appendChild(box);
      return r;
    }

    // ------------------------------------------------------------- parts

    function fld(label, input, hint) {
      return el("label", { class: "fld" }, [el("span", { text: label }), input, hint ? el("small", { text: hint }) : null]);
    }

    /// The credential: one header, or a user and password.
    function credentialPart() {
      var kind = "header";
      var seg = el("div", { class: "seg", role: "group", "aria-label": "Kind of credential" });
      var headerName = el("input", { class: "inp", name: "headerName", value: "Authorization", autocomplete: "off", spellcheck: "false", autocapitalize: "none" });
      var value = el("input", { class: "inp", name: "credValue", type: "password", autocomplete: "new-password", placeholder: "Bearer ...", spellcheck: "false" });
      var user = el("input", { class: "inp", name: "credUser", autocomplete: "off", spellcheck: "false", autocapitalize: "none" });
      var password = el("input", { class: "inp", name: "credPassword", type: "password", autocomplete: "new-password" });
      var asHeader = el("div", { class: "form" }, [
        fld("Header", headerName, "Usually Authorization, or the one your API names, like X-Api-Key."),
        fld("Value", value, "Stored encrypted. It is never shown again, here or to an app. A server that echoes request headers back would show it to the app."),
      ]);
      var asBasic = el("div", { class: "form", hidden: true }, [fld("User", user), fld("Password", password, "Stored encrypted. It is never shown again.")]);
      [["header", "A header"], ["basic", "User and password"]].forEach(function (p) {
        var b = el("button", { type: "button", "aria-pressed": p[0] === kind ? "true" : "false", text: p[1] });
        b.dataset.kind = p[0];
        b.addEventListener("click", function () {
          kind = p[0];
          [].forEach.call(seg.children, function (x) { x.setAttribute("aria-pressed", x.dataset.kind === kind ? "true" : "false"); });
          asHeader.hidden = kind !== "header";
          asBasic.hidden = kind !== "basic";
        });
        seg.appendChild(b);
      });
      return {
        node: el("div", { class: "fld" }, [el("span", { text: "Credential" }), seg, asHeader, asBasic]),
        body: function () {
          if (kind === "basic") return user.value.trim() ? { basic: { user: user.value.trim(), password: password.value } } : null;
          return value.value.trim() ? { header: { name: headerName.value.trim(), value: value.value } } : null;
        },
        clear: function () { value.value = ""; password.value = ""; },
      };
    }

    /// What it allows: methods and paths.
    function allowPart(cur) {
      cur = cur || { methods: ["GET"], paths: ["/"] };
      var boxes = METHODS.map(function (m) {
        var i = el("input", { type: "checkbox", name: "method", value: m });
        i.checked = (cur.methods || []).indexOf(m) >= 0;
        return el("label", { class: "chk" }, [i, el("span", { text: m })]);
      });
      var paths = el("input", { class: "inp", name: "paths", autocomplete: "off", spellcheck: "false", autocapitalize: "none",
        value: (cur.paths || []).filter(function (p) { return p !== "/"; }).join(", "), placeholder: "Everything under the address" });
      return {
        node: el("div", { class: "form" }, [
          el("div", { class: "fld" }, [el("span", { text: "Apps may" }), el("div", { class: "mchecks" }, boxes), el("small", { text: "GET reads. Tick more only if the apps must change things." })]),
          fld("Paths", paths, "Under the address, comma-separated, like /invoices, /rates. Empty is all of it."),
        ]),
        body: function () {
          var m = boxes.map(function (b) { return b.querySelector("input"); }).filter(function (i) { return i.checked; }).map(function (i) { return i.value; });
          var p = paths.value.split(/[,\s]+/).map(function (x) { return x.trim(); }).filter(Boolean);
          return { methods: m, paths: p.length ? p : ["/"] };
        },
      };
    }

    /// Who may use it, and which apps.
    function scopePart(cur) {
      cur = cur || {};
      var groupsBox = el("div", { class: "gchecks" }, [el("p", { class: "sub", text: "Loading groups" })]);
      var checks = [];
      var everyone = !(Array.isArray(cur.audience) && cur.audience.length);
      var whoSeg = el("div", { class: "seg", role: "group", "aria-label": "Who may use it" });
      var setWho = function (all) {
        everyone = all;
        [].forEach.call(whoSeg.children, function (x) { x.setAttribute("aria-pressed", (x.dataset.all === "1") === all ? "true" : "false"); });
        groupsBox.hidden = all;
      };
      [["1", "Everyone"], ["0", "Some groups"]].forEach(function (p) {
        var b = el("button", { type: "button", text: p[1] });
        b.dataset.all = p[0];
        b.addEventListener("click", function () { setWho(p[0] === "1"); });
        whoSeg.appendChild(b);
      });
      K.api("GET", "/orgs/" + slug + "/groups").then(function (res) {
        groupsBox.textContent = "";
        var groups = res.ok && res.body && Array.isArray(res.body.groups) ? res.body.groups : [];
        if (!groups.length) groupsBox.appendChild(el("p", { class: "sub", text: "No groups yet. Make one on the People tab." }));
        groups.forEach(function (g) {
          var i = el("input", { type: "checkbox", name: "group", value: g.name });
          i.checked = Array.isArray(cur.audience) && cur.audience.indexOf(g.name) >= 0;
          checks.push(i);
          groupsBox.appendChild(el("label", { class: "chk" }, [i, el("span", { text: g.name + " (" + g.members.length + ")" })]));
        });
      });
      setWho(everyone);
      var apps = el("input", { class: "inp", name: "apps", autocomplete: "off", spellcheck: "false", autocapitalize: "none",
        value: Array.isArray(cur.apps) ? cur.apps.join(", ") : "", placeholder: "Any of the organisation's apps" });
      return {
        node: el("div", { class: "form" }, [
          el("div", { class: "fld" }, [el("span", { text: "Who may use it" }), whoSeg, groupsBox]),
          fld("Which apps", apps, "App ids from their manifest, like com.acme.ledger, comma-separated. Empty is any of " + nameOf() + "'s apps."),
        ]),
        body: function () {
          var ids = apps.value.split(/[,\s]+/).map(function (x) { return x.trim().toLowerCase(); }).filter(Boolean);
          var picked = checks.filter(function (i) { return i.checked; }).map(function (i) { return i.value; });
          return {
            audience: everyone ? { everyone: true } : { groups: picked },
            apps: ids.length ? { ids: ids } : { any: true },
            pickedNone: !everyone && !picked.length,
          };
        },
      };
    }

    function outBox() { return el("div", { class: "connout" }); }
    function hint(out, t, b) { out.textContent = ""; out.appendChild(K.refusalBlock({ title: t, body: b, action: null }, "warn")); }

    function saving(button, label) {
      button.disabled = true;
      var was = button.textContent;
      button.textContent = label;
      return function () { button.disabled = false; button.textContent = was; };
    }

    // ------------------------------------------------------------- forms

    function createForm(card, box) {
      var name = el("input", { class: "inp", name: "connName", autocomplete: "off", spellcheck: "false", autocapitalize: "none", placeholder: "finance-api" });
      var base = el("input", { class: "inp", name: "connBase", type: "url", autocomplete: "off", spellcheck: "false", autocapitalize: "none", placeholder: "https://api.acme.com/finance" });
      var cred = credentialPart();
      var allow = allowPart();
      var scope = scopePart();
      var out = outBox();
      var save = el("button", { class: "btn dark", type: "button", text: "Add connection" });
      var cancel = el("button", { class: "btn ghost", type: "button", text: "Cancel", onclick: function () { box.hidden = true; } });
      save.addEventListener("click", function () {
        var n = name.value.trim().toLowerCase();
        var b = base.value.trim();
        if (!/^[a-z][a-z0-9-]{1,38}$/.test(n)) return hint(out, "The name is short and plain", "2 to 39 lower-case letters, digits and dashes, like finance-api. Apps ask for it by this name.");
        var local = /^(127\.0\.0\.1|localhost)$/.test(location.hostname) && /^http:\/\/(127\.0\.0\.1|localhost)(:\d+)?\//.test(b + "/");
        if (!/^https:\/\/[^\s<>]+$/.test(b) && !local) return hint(out, "The address starts with https://", "A host and a path, like https://api.acme.com/finance. Requests under it get the credential.");
        var c = cred.body();
        if (!c) return hint(out, "The credential is missing", "Paste the header's value, or the user and password, your API gave you.");
        var a = allow.body();
        if (!a.methods.length) return hint(out, "Tick at least one thing apps may do", "GET alone lets them read.");
        var s = scope.body();
        if (s.pickedNone) return hint(out, "Pick at least one group", "Or choose Everyone.");
        var body = Object.assign({ name: n, base: b, allow: a, audience: s.audience, apps: s.apps }, c);
        out.textContent = "";
        var done = saving(save, "Adding");
        sealForHub(body).then(function (sealed) { return K.api("POST", path(), sealed, { key: K.key() }); }).then(function (res) {
          done();
          cred.clear();
          if (!res.ok) { out.appendChild(K.refusalBlock(D.words(res, ctx), "bad")); return; }
          D.toast("The " + n + " connection is ready. Apps that ask for net.connection:" + n + " can use it.");
          draw(card);
        });
      });
      return el("div", { class: "connbox" }, [
        el("b", { text: "New connection" }),
        el("div", { class: "form" }, [fld("Name", name, "Apps ask for it as net.connection:<name>."), fld("Address", base, "Requests under this address get the credential. Anything else never does.")]),
        cred.node, allow.node, scope.node, out,
        el("div", { class: "row2" }, [save, cancel]),
      ]);
    }

    function credentialForm(card, c, box) {
      var cred = credentialPart();
      var out = outBox();
      var save = el("button", { class: "btn dark", type: "button", text: "Replace credential" });
      var cancel = el("button", { class: "btn ghost", type: "button", text: "Cancel", onclick: function () { box.hidden = true; } });
      save.addEventListener("click", function () {
        var body = cred.body();
        if (!body) return hint(out, "The new credential is missing", "Paste the new value. The old one is deleted when you save.");
        out.textContent = "";
        var done = saving(save, "Replacing");
        sealForHub(body).then(function (sealed) { return K.api("PUT", path("/" + encodeURIComponent(c.name) + "/credential"), sealed, { key: K.key() }); }).then(function (res) {
          done();
          cred.clear();
          if (!res.ok) { out.appendChild(K.refusalBlock(D.words(res, ctx), "bad")); return; }
          D.toast(c.name + " has a new credential. Apps get it the next time they open.");
          draw(card);
        });
      });
      return el("div", { class: "connbox" }, [
        el("b", { text: "Replace the credential of " + c.name }),
        el("p", { class: "sub", text: "The old one is deleted. Every app that uses it gets the new one the next time it opens." }),
        cred.node, out, el("div", { class: "row2" }, [save, cancel]),
      ]);
    }

    function changeForm(card, c, box) {
      var allow = allowPart(c.allow);
      var scope = scopePart(c);
      var out = outBox();
      var save = el("button", { class: "btn dark", type: "button", text: "Save changes" });
      var cancel = el("button", { class: "btn ghost", type: "button", text: "Cancel", onclick: function () { box.hidden = true; } });
      save.addEventListener("click", function () {
        var a = allow.body();
        if (!a.methods.length) return hint(out, "Tick at least one thing apps may do", "GET alone lets them read.");
        var s = scope.body();
        if (s.pickedNone) return hint(out, "Pick at least one group", "Or choose Everyone.");
        out.textContent = "";
        var done = saving(save, "Saving");
        K.api("PUT", path("/" + encodeURIComponent(c.name)), { allow: a, audience: s.audience, apps: s.apps }, { key: K.key() }).then(function (res) {
          done();
          if (!res.ok) { out.appendChild(K.refusalBlock(D.words(res, ctx), "bad")); return; }
          D.toast(c.name + " is changed. Apps follow it the next time they open.");
          draw(card);
        });
      });
      return el("div", { class: "connbox" }, [
        el("b", { text: "Change " + c.name }),
        el("p", { class: "sub", text: "Its address stays " + c.base + ". To send the credential somewhere else, remove this connection and add it again with the new address." }),
        allow.node, scope.node, out, el("div", { class: "row2" }, [save, cancel]),
      ]);
    }

    function remove(card, c) {
      var key = K.key();
      D.confirmSheet({
        title: "Remove " + c.name + "?",
        body: "Its credential is deleted from Krate. Apps that use it are refused it from the next time they open, and say so.",
        points: [c.base, scopeWords(c)],
        yes: "Remove it",
        busy: "Removing",
        ctx: ctx,
        act: function () { return K.api("DELETE", path("/" + encodeURIComponent(c.name)), null, { key: key }); },
        done: function () { D.toast(c.name + " is removed, with its credential."); draw(card); },
      });
    }

    return { settings: settings };
  };
})(typeof window !== "undefined" ? window : globalThis);
