/* The organisation dashboard's Apps tab (IC-954): every app the
 * organisation has, with the record IT asks for -- who owns it, who last
 * sent it, how it was made, what it can do, the company connections it
 * uses, who it is for, how many people opened it in the last 30 days (and,
 * for owners and admins, who), and its state. An app whose owner was
 * removed comes first and says so, with the one control that fixes it.
 * The whole list downloads as a spreadsheet.
 *
 * dashboard.js owns the page and hands this part what it needs (D). It
 * asks the hub when it is drawn, draws only the controls the role matrix
 * gives this person, and says refusals in words.
 */
(function (root) {
  "use strict";

  root.KrateOrgInventory = function (D) {
    var K = root.KrateOrg;
    var el = K.el;
    var slug = D.slug;
    var path = function (p) { return "/orgs/" + slug + (p || ""); };
    var nameOf = function () { return (D.S.org && D.S.org.name) || slug; };
    var can = function (action) { return K.can(D.roleOf(), action) === "yes"; };

    function line(res, ctx) {
      var w = D.words(res, ctx);
      return w.title + ". " + w.body;
    }

    var STATE = {
      released: { text: "In use", tag: "ok" },
      waiting: { text: "Waiting for approval", tag: "" },
      withdrawn: { text: "Withdrawn", tag: "warn" },
      none: { text: "Nothing released", tag: "" },
    };

    function forWords(a) {
      return Array.isArray(a.audience) && a.audience.length
        ? "Only for " + K.groupsSaid(a.audience)
        : "For everyone in " + nameOf();
    }

    function peopleWords(a) {
      var n = a.people_30_days || 0;
      var said = n === 0 ? "Nobody opened it in the last 30 days" : (n === 1 ? "1 person" : n + " people") + " opened it in the last 30 days";
      if (Array.isArray(a.people) && a.people.length) {
        said += ": " + a.people.map(function (p) { return K.who(p.principal).label; }).join(", ");
      }
      return said;
    }

    function canWords(a) {
      var words = (a.capabilities || []).map(function (c) { return c.words || c.cap; }).filter(Boolean);
      return words.length ? "Can " + words.join("; ") : "Asks for nothing on a computer";
    }

    // ---------------------------------------------------------------- pane

    function pane(p) {
      K.api("GET", path("/inventory")).then(function (res) {
        D.head(p, "Apps", "Every app " + nameOf() + " has: who owns it, how it was made, what it can do and who uses it.");
        if (!res.ok) return D.failed(p, res, { what: "see the organisation's apps", whoCan: "owners, admins and reviewers" });
        var apps = (res.body && res.body.apps) || [];
        if (!apps.length) {
          p.appendChild(el("div", { class: "card rows" }, [el("div", { class: "empty" }, [
            el("b", { text: "No apps yet" }),
            el("p", { text: "When someone sends an app to " + nameOf() + " -- in Krate Studio, Share, then Send to " + nameOf() + " for approval -- it appears here with who made it and who uses it." }),
          ])]));
          return;
        }
        p.appendChild(exportBar(apps));
        apps.forEach(function (a) { p.appendChild(appCard(a, p)); });
      });
    }

    /// The one action for the whole list: take it to a spreadsheet.
    function exportBar(apps) {
      var note = el("p", { class: "note", role: "status", hidden: true });
      var b = el("button", { class: "btn ghost sm", type: "button", text: "Download the list as a spreadsheet" });
      b.addEventListener("click", function () {
        b.disabled = true;
        b.textContent = "Preparing the spreadsheet";
        K.download(K.hub() + path("/inventory.csv"), slug + "-apps.csv").then(function (out) {
          b.disabled = false;
          b.textContent = "Download the list as a spreadsheet";
          note.textContent = out.ok ? "Saved as " + slug + "-apps.csv, one row per app." : line({ body: out.body }, { what: "download the list" });
          note.hidden = false;
        });
      });
      var needs = apps.filter(function (a) { return a.needs_owner; }).length;
      return el("div", { class: "invbar" }, [
        el("p", { class: "sum", text: apps.length + (apps.length === 1 ? " app" : " apps") + (needs ? " · " + needs + " need" + (needs === 1 ? "s" : "") + " an owner" : "") }),
        b,
        note,
      ]);
    }

    function appCard(a, p) {
      var st = STATE[a.state] || STATE.none;
      var card = el("div", { class: "card rows appc inv" + (a.needs_owner ? " needs" : "") });
      card.appendChild(el("div", { class: "apph" }, [
        el("div", { class: "t" }, [
          el("h3", {}, [a.name || a.app_id, " ", el("span", { class: "tag " + st.tag, text: st.text })]),
          el("small", { text: (a.version ? "Version " + a.version : "No version yet") + " · " + forWords(a) }),
        ]),
      ]));
      card.appendChild(ownerRow(a, p));
      var facts = [
        el("small", { text: (a.built ? "Made with " + a.built_words : "Made by hand") + (a.built && a.built.revisions ? ", changed " + a.built.revisions + (a.built.revisions === 1 ? " time" : " times") + " since" : "") }),
        el("small", { text: canWords(a) }),
      ];
      if (a.connections && a.connections.length) {
        facts.push(el("small", { text: "Uses the " + a.connections.join(", ") + " connection" + (a.connections.length === 1 ? "" : "s") + ": the app never holds the credential" }));
      }
      facts.push(el("small", { text: "Last sent by " + K.who(a.last_sent_by).label + (a.last_sent_at ? " on " + K.day(a.last_sent_at) : "") }));
      facts.push(el("small", { class: "ppl", text: peopleWords(a) }));
      card.appendChild(el("div", { class: "rw flat" }, [el("div", { class: "t" }, facts)]));
      // The exact terms, for whoever wants them, behind Details.
      var details = [el("summary", { text: "Details" }), el("small", { text: "App id: " + a.app_id })];
      if (a.built && a.built.prompt) details.push(el("small", { text: "Asked for: “" + a.built.prompt + "”" }));
      if (a.built && a.built.finished_at) details.push(el("small", { text: "Built on " + K.day(a.built.finished_at) }));
      (a.capabilities || []).forEach(function (c) { details.push(el("small", { class: "dg", text: c.cap })); });
      card.appendChild(el("details", { class: "invdet" }, details));
      return card;
    }

    function ownerRow(a, p) {
      var note = el("p", { class: "note", role: "status", hidden: true });
      var say = function (t) { note.textContent = t; note.hidden = !t; };
      var owner = a.owner ? K.who(a.owner.principal).label : "nobody";
      var facts = a.needs_owner
        ? [el("b", {}, [el("span", { class: "tag warn", text: "Needs an owner" })]),
           el("small", { text: (a.owner ? owner + ", who owned it, is no longer in " + nameOf() + "." : "Nobody owns it.") + " The owner is who your IT team asks about this app." })]
        : [el("b", { text: "Owned by " + owner }), el("small", { text: "Who your IT team asks about this app" })];
      facts.push(note);
      var row = el("div", { class: "rw flat own" }, [el("div", { class: "t" }, facts)]);
      if (!can("app.owner")) return row;
      // The members who can own it, the current owner first.
      var choices = (D.S.members || []).filter(function (m) { return m.state === "active" || !m.state; });
      if (!choices.length) return row;
      var sel = el("select", { class: "sel", "aria-label": "Owner of " + (a.name || a.app_id) });
      choices.forEach(function (m) {
        var o = el("option", { value: m.principal, text: K.who(m).label });
        if (a.owner && m.principal === a.owner.principal && !a.needs_owner) o.selected = true;
        sel.appendChild(o);
      });
      var go = el("button", { class: "btn dark sm", type: "button", text: "Make them the owner" });
      go.addEventListener("click", function () {
        var who = sel.value;
        go.disabled = true;
        say("Making " + K.who(who).label + " the owner");
        K.api("PUT", path("/apps/" + encodeURIComponent(a.app_id) + "/owner"), { principal: who }, { key: K.key() }).then(function (res) {
          go.disabled = false;
          if (!res.ok) { say(line(res, { what: "choose the owner", whoCan: "owners and admins" })); return; }
          D.toast(K.who(who).label + " owns " + (a.name || a.app_id) + " now. It is in the activity.");
          D.show("apps");
        });
      });
      var picker = el("div", { class: "ra manage" }, [sel, go]);
      if (a.needs_owner) {
        // The one thing this card needs: the picker is open.
        row.appendChild(picker);
        return row;
      }
      // An app with an owner keeps the change one click away, not open.
      picker.hidden = true;
      var change = el("button", { class: "btn ghost sm", type: "button", text: "Change owner", "aria-expanded": "false" });
      change.addEventListener("click", function () {
        picker.hidden = false;
        change.hidden = true;
        change.setAttribute("aria-expanded", "true");
      });
      row.appendChild(el("div", { class: "ra" }, [change]));
      row.appendChild(picker);
      return row;
    }

    return { pane: pane };
  };
})(typeof window !== "undefined" ? window : globalThis);
