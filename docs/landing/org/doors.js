/* The organisation dashboard's IC-955 Settings cards: company sign-in by
 * SAML, provisioning by SCIM, and sending the activity to the company's
 * own logs (audit export). parts.js puts them in Settings; each asks the
 * hub when it is drawn and says every refusal in words.
 *
 * A secret the hub shows once (a SCIM token, the export's signing secret)
 * is drawn into the card that asked for it and nowhere else: never stored
 * in the browser, gone when the page is left.
 */
(function (root) {
  "use strict";

  root.KrateOrgDoors = function (D) {
    var K = root.KrateOrg;
    var el = K.el;
    var slug = D.slug;
    var path = function (p) { return "/orgs/" + slug + (p || ""); };
    var nameOf = function () { return (D.S.org && D.S.org.name) || slug; };
    var fld = function (label, input, hint) { return el("label", { class: "fld" }, [el("span", { text: label }), input, hint ? el("small", { text: hint }) : null]); };
    var copyRow = function (label, value, id) {
      var b = el("button", { class: "btn ghost sm", type: "button", text: "Copy" });
      b.addEventListener("click", function () { K.copy(value, b); });
      return el("div", { class: "rec" }, [el("span", { text: label }), el("code", { class: "k", id: id || null, text: value }), b]);
    };
    var plural = function (n, one, many) { return n + " " + (n === 1 ? one : many); };
    var bad = function (out, res, ctx, cls) { out.textContent = ""; out.appendChild(K.refusalBlock(D.words(res, ctx), cls || "bad")); };
    var hint = function (out, title, body) { out.textContent = ""; out.appendChild(K.refusalBlock({ title: title, body: body, action: null }, "warn")); };
    /// A secret shown once, with Copy, and what to do with it now.
    var once = function (title, value, then, id) {
      var b = el("button", { class: "btn dark sm", type: "button", text: "Copy" });
      b.addEventListener("click", function () { K.copy(value, b); });
      return el("div", { class: "proof once", id: id }, [
        el("b", { text: title }),
        el("p", { class: "sub", text: "Shown once. Krate keeps only a fingerprint of it, so copy it now. " + then }),
        el("div", { class: "linkbox" }, [el("code", { class: "k", text: value }), b]),
      ]);
    };

    // ------------------------------------------------------------- SAML

    function samlCard() {
      var card = el("div", { class: "card", id: "samlCard" }, [
        el("h3", { text: "Company sign-in by SAML" }),
        el("p", { text: "For identity providers set up for SAML 2.0, such as Okta, Microsoft Entra or Google Workspace. Add Krate there as a SAML app with the values below, then paste what it gives you back." }),
        el("p", { class: "loading", text: "Loading" }),
      ]);
      K.api("GET", path("/saml")).then(function (res) {
        card.lastChild.remove();
        if (!res.ok) {
          // An older hub has no SAML route: say so plainly.
          if (res.status === 404) { card.appendChild(el("p", { class: "sub", text: "SAML sign-in is not on this hub yet. OpenID Connect above works today." })); return; }
          card.appendChild(K.refusalBlock(D.words(res, { what: "set up SAML sign-in", whoCan: "an owner" })));
          return;
        }
        samlForm(card, res.body || {});
      });
      return card;
    }

    function samlForm(card, cur) {
      var idp = cur.idp || {};
      var sp = cur.sp || {};
      card.appendChild(el("div", { class: "proof" }, [
        el("b", { text: "Give these to your identity provider" }),
        el("div", { class: "recs" }, [
          copyRow("ACS URL", sp.acs_url || "", "samlAcs"),
          copyRow("Entity ID", sp.entity_id || "", "samlEntity"),
        ]),
        el("p", { class: "sub", text: "Name ID: the person's email address. Sign the assertion. Or import the metadata at the Entity ID's address." }),
      ]));
      var entity = el("input", { class: "inp", id: "samlIdpEntity", value: idp.entity_id || "", autocomplete: "off", spellcheck: "false", placeholder: "http://www.okta.com/exk..." });
      var url = el("input", { class: "inp", id: "samlIdpUrl", value: idp.sso_url || "", autocomplete: "off", spellcheck: "false", inputmode: "url", placeholder: "https://acme.okta.com/app/.../sso/saml" });
      var cert = el("textarea", { class: "inp", id: "samlCert", rows: "5", spellcheck: "false", placeholder: idp.certificates && idp.certificates.length ? "Stored. Paste a new one only to replace it" : "-----BEGIN CERTIFICATE-----" });
      var domain = el("input", { class: "inp", id: "samlDomain", value: cur.domain || "", autocomplete: "off", spellcheck: "false", placeholder: "acme.com" });
      if (cur.domain) domain.disabled = true;
      var out = el("div");
      var save = el("button", { class: "btn dark", type: "button", text: idp.entity_id ? "Save changes" : "Turn on SAML sign-in" });
      save.addEventListener("click", function () {
        var body = { idp_entity_id: entity.value.trim(), idp_sso_url: url.value.trim() };
        if (cert.value.trim()) body.idp_cert = cert.value.trim();
        if (!cur.domain) body.domain = domain.value.trim().toLowerCase().replace(/^@/, "");
        if (!body.idp_entity_id) return hint(out, "The identity provider's entity ID is missing", "It is called Issuer or Identity Provider Identifier on its SAML set-up page.");
        if (!/^https:\/\/[^\s<>]+$/.test(body.idp_sso_url)) return hint(out, "The sign-in URL is a web address", "It starts with https:// and is called SSO URL or Login URL on the SAML set-up page.");
        // No certificate pasted on a change keeps the stored one.
        if (!body.idp_cert && !(idp.certificates && idp.certificates.length)) return hint(out, "The signing certificate is missing", "Download it from the SAML set-up page (Base64 or PEM) and paste its text here.");
        if (!cur.domain && !/^[a-z0-9.-]+\.[a-z]{2,}$/.test(body.domain)) return hint(out, "The domain is a name like acme.com", "The part of your people's work addresses after the @.");
        save.disabled = true;
        save.textContent = "Saving";
        K.api("PUT", path("/saml"), body, { key: K.key() }).then(function (r) {
          save.disabled = false;
          save.textContent = "Save changes";
          if (!r.ok) return bad(out, r, { what: "set up SAML sign-in", whoCan: "an owner" });
          D.toast("SAML sign-in saved." + (r.body && r.body.domain_verified ? "" : " Prove the domain under Company sign-in to switch it on."));
          D.show("settings");
        });
      });
      card.appendChild(el("div", { class: "form sso" }, [
        fld("Identity provider entity ID", entity, "Also called Issuer."),
        fld("Sign-in URL", url, "Also called SSO URL or Login URL. Krate sends people here to sign in."),
        fld("Signing certificate", cert, idp.certificates && idp.certificates.length
          ? "Stored: " + idp.certificates.map(function (c) { return "sha256 " + String(c.sha256).slice(0, 16).replace(/(.{4})(?=.)/g, "$1 ") + (c.not_after ? ", valid until " + K.day(c.not_after) : ""); }).join("; ") + "."
          : "Only assertions this certificate signed are accepted."),
        cur.domain ? null : fld("Company domain", domain, "Only addresses at this domain sign in as your people. You prove it with a DNS record next."),
      ]));
      card.appendChild(out);
      card.appendChild(el("div", { class: "row2" }, [save]));
      if (cur.domain) {
        card.appendChild(el("p", { class: "sub" }, [
          cur.domain_verified ? el("span", { class: "tag ok", text: "Verified" }) : el("span", { class: "tag warn", text: "Not verified" }),
          " " + cur.domain + (cur.domain_verified ? ". Who joins and whether it is required are set under Company sign-in." : ". Prove it under Company sign-in; until then nobody signs in by SAML."),
        ]));
      }
      if (idp.entity_id) {
        var link = location.origin + "/login/?org=" + slug;
        var cp = el("button", { class: "btn ghost sm", type: "button", text: "Copy", onclick: function () { K.copy(link, cp); } });
        card.appendChild(el("div", { class: "proof" }, [el("b", { text: "Your people sign in here" }), el("div", { class: "linkbox" }, [el("code", { class: "k", text: link }), cp])]));
        var off = el("button", { class: "btn quiet sm", type: "button", text: "Turn off SAML sign-in" });
        off.addEventListener("click", function () {
          var key = K.key();
          D.confirmSheet({
            title: "Turn off SAML sign-in for " + nameOf() + "?",
            body: "Nobody can sign in through it any more. The domain and its proof stay. Members stay members and sign in with GitHub, Google, email, or OpenID Connect if it is set up.",
            yes: "Turn it off", no: "Keep it on", busy: "Turning off",
            ctx: { what: "turn off SAML sign-in", whoCan: "an owner" },
            act: function () { return K.api("DELETE", path("/saml"), null, { key: key }); },
            done: function () { D.toast("SAML sign-in is off for " + nameOf() + "."); D.show("settings"); },
          });
        });
        card.appendChild(el("div", { class: "row2" }, [off]));
      }
    }

    // ------------------------------------------------------------- SCIM

    var ROLE_PICK = [["recipient", "Recipient: opens released apps"], ["developer", "Developer: also sends apps"], ["reviewer", "Reviewer: also approves apps"]];

    function scimCard() {
      var card = el("div", { class: "card", id: "scimCard" }, [
        el("h3", { text: "Provisioning (SCIM)" }),
        el("p", { text: "Let your identity provider add and remove " + nameOf() + "'s people and groups. Someone deactivated there is removed here, and their apps stop opening the next time they connect." }),
        el("p", { class: "loading", text: "Loading" }),
      ]);
      K.api("GET", path("/scim")).then(function (res) {
        card.lastChild.remove();
        if (!res.ok) {
          if (res.status === 404) { card.appendChild(el("p", { class: "sub", text: "Provisioning is not on this hub yet." })); return; }
          card.appendChild(K.refusalBlock(D.words(res, { what: "set up provisioning", whoCan: "an owner" })));
          return;
        }
        scimBody(card, res.body || {});
      });
      return card;
    }

    function scimBody(card, cur, fresh) {
      var tokens = cur.tokens || [];
      if (fresh) card.appendChild(once("Your SCIM token", fresh, "Paste it into your identity provider as the Bearer token (Okta: API Token. Entra: Secret Token).", "scimOnce"));
      card.appendChild(el("div", { class: "proof" }, [
        el("b", { text: "Give these to your identity provider" }),
        el("div", { class: "recs" }, [copyRow("Base URL", cur.base_url || "", "scimBase")]),
        el("p", { class: "sub", text: "Okta: SCIM connector base URL, unique identifier userName, HTTP Header authentication. Entra: Tenant URL and Secret Token. userName must be the person's email address." }),
      ]));
      // The default role.
      var sel = el("select", { class: "sel", id: "scimRole", "aria-label": "Role for people the directory adds" });
      ROLE_PICK.forEach(function (o) { var opt = el("option", { value: o[0], text: o[1] }); if (cur.default_role === o[0]) opt.selected = true; sel.appendChild(opt); });
      var roleOut = el("div");
      sel.addEventListener("change", function () {
        sel.disabled = true;
        K.api("PUT", path("/scim"), { default_role: sel.value }, { key: K.key() }).then(function (r) {
          sel.disabled = false;
          if (!r.ok) return bad(roleOut, r, { what: "choose the role", whoCan: "an owner" });
          roleOut.textContent = "";
          D.toast("People your directory adds now join as " + (K.ROLE_NAME && K.ROLE_NAME[sel.value] ? K.ROLE_NAME[sel.value].toLowerCase() : sel.value) + ".");
        });
      });
      card.appendChild(el("div", { class: "form sso" }, [fld("People it adds join as", sel, "Never owner or admin: those are given by an owner, in People.")]));
      card.appendChild(roleOut);
      // Tokens.
      var list = el("div", { class: "rows", id: "scimTokens" });
      if (!tokens.length) list.appendChild(el("p", { class: "sub", style: "padding:6px 0 14px", text: "No token yet. Make one, then paste it into your identity provider." }));
      tokens.forEach(function (t) {
        var rv = el("button", { class: "btn ghost sm", type: "button", text: "Revoke" });
        rv.addEventListener("click", function () {
          var key = K.key();
          D.confirmSheet({
            title: "Revoke this SCIM token?",
            body: "Your identity provider can no longer add or remove people with it, from its next request. People already here stay.",
            yes: "Revoke it", no: "Keep it", busy: "Revoking",
            ctx: { what: "revoke a SCIM token", whoCan: "an owner" },
            act: function () { return K.api("DELETE", path("/scim/tokens/" + encodeURIComponent(t.id)), null, { key: key }); },
            done: function () { D.toast("SCIM token revoked."); D.show("settings"); },
          });
        });
        list.appendChild(el("div", { class: "rw flat" }, [
          el("div", { class: "t" }, [
            el("b", { text: t.hint || t.id }),
            el("small", { text: "Made " + K.day(t.created_at) + " by " + K.who(t.created_by).label + " · " + (t.last_used_at ? "last used " + K.moment(t.last_used_at) : "not used yet") }),
          ]),
          el("div", { class: "ra" }, [rv]),
        ]));
      });
      card.appendChild(list);
      var out = el("div");
      var make = el("button", { class: "btn dark sm", type: "button", text: tokens.length ? "Make another token" : "Make a SCIM token" });
      make.addEventListener("click", function () {
        make.disabled = true;
        K.api("POST", path("/scim/tokens"), {}, {}).then(function (r) {
          make.disabled = false;
          if (!r.ok) return bad(out, r, { what: "make a SCIM token", whoCan: "an owner" });
          card.textContent = "";
          card.appendChild(el("h3", { text: "Provisioning (SCIM)" }));
          card.appendChild(el("p", { text: "Let your identity provider add and remove " + nameOf() + "'s people and groups. Someone deactivated there is removed here, and their apps stop opening the next time they connect." }));
          scimBody(card, Object.assign({}, r.body.settings || cur, { base_url: r.body.base_url || cur.base_url }), r.body.token);
        });
      });
      card.appendChild(out);
      card.appendChild(el("div", { class: "row2" }, [make, el("span", { class: "lbl", text: plural(cur.people || 0, "person", "people") + " and " + plural(cur.groups || 0, "group", "groups") + " from the directory" })]));
    }

    // ------------------------------------------------------ audit export

    function exportCard() {
      var card = el("div", { class: "card", id: "auditCard" }, [
        el("h3", { text: "Send the activity to your logs" }),
        el("p", { text: "Every event in Activity, posted as JSON to your SIEM or log collector as it happens, signed so it can check each one came from Krate." }),
        el("p", { class: "loading", text: "Loading" }),
      ]);
      K.api("GET", path("/audit-export")).then(function (res) {
        card.lastChild.remove();
        if (!res.ok) {
          if (res.status === 404) { card.appendChild(el("p", { class: "sub", text: "The audit export is not on this hub yet." })); return; }
          card.appendChild(K.refusalBlock(D.words(res, { what: "see the audit export", whoCan: "owners and admins" })));
          return;
        }
        exportBody(card, res.body);
      });
      return card;
    }

    var STATES = function (d) {
      if (!d) return ["Not set up", ""];
      if (d.pending && d.last_error) return ["Retrying", "warn"];
      if (d.pending) return ["Sending", ""];
      if (d.delivered) return ["Delivered", "ok"];
      return ["Waiting for the first event", ""];
    };

    function exportBody(card, cur, secret) {
      var manage = K.can(D.roleOf(), "audit-export.manage") === "yes";
      if (secret) card.appendChild(once("Your signing secret", secret, "Your receiver checks X-Krate-Signature with it.", "auditOnce"));
      var out = el("div");
      if (cur) {
        var d = cur.delivery || {};
        var st = STATES(d);
        var dl = [
          el("dt", { text: "Sent to" }), el("dd", { id: "auditHost", text: cur.host || cur.url }),
          el("dt", { text: "Delivered" }), el("dd", { text: String(d.delivered || 0) + (d.last_delivered_at ? ", last " + K.moment(d.last_delivered_at) : "") }),
          el("dt", { text: "Waiting" }), el("dd", { text: String(d.pending || 0) + (d.pending && d.next_attempt_at ? ", next try " + K.moment(d.next_attempt_at) : "") }),
        ];
        if (d.given_up) { dl.push(el("dt", { text: "Not delivered" })); dl.push(el("dd", { text: d.given_up + " (refused for 24 hours, then given up)" })); }
        if (d.last_error && d.pending) { dl.push(el("dt", { text: "Last problem" })); dl.push(el("dd", { id: "auditError", text: d.last_error + (d.last_error_at ? ", " + K.moment(d.last_error_at) : "") })); }
        card.appendChild(el("div", { class: "row2" }, [el("span", { class: "lbl", text: "Delivery" }), el("span", { class: "tag " + st[1], id: "auditState", text: st[0] })]));
        card.appendChild(el("dl", { class: "kv" }, dl));
        card.appendChild(el("p", { class: "sub", text: "Each event is retried, oldest first, from 30 seconds to an hour apart, for 24 hours. Check X-Krate-Signature: t=<unix>,v1=<hex HMAC-SHA256 of \"<t>.<body>\"> with your secret." }));
      }
      if (!manage) {
        if (!cur) card.appendChild(el("p", { class: "sub", text: "Not set up. An owner of " + nameOf() + " sets it up here." }));
        return;
      }
      var url = el("input", { class: "inp", id: "auditUrl", value: cur ? cur.url : "", autocomplete: "off", spellcheck: "false", inputmode: "url", placeholder: "https://siem.acme.com/krate" });
      var save = el("button", { class: "btn dark", type: "button", text: cur ? "Save address" : "Start sending" });
      save.addEventListener("click", function () {
        var v = url.value.trim();
        var local = /^(127\.0\.0\.1|localhost)$/.test(location.hostname) && /^http:\/\/(127\.0\.0\.1|localhost)(:\d+)?\//.test(v + "/");
        if (!/^https:\/\/[^\s<>]+$/.test(v) && !local) return hint(out, "The address is a web address", "It starts with https://. Your SIEM or log collector shows it when you add an HTTP or webhook source.");
        save.disabled = true;
        save.textContent = "Saving";
        K.api("PUT", path("/audit-export"), { url: v }, { key: K.key() }).then(function (r) {
          save.disabled = false;
          save.textContent = "Save address";
          if (!r.ok) return bad(out, r, { what: "set up the audit export", whoCan: "an owner" });
          redraw(r.body, r.body.secret);
          D.toast(r.body.secret ? "Sending the activity. Copy the signing secret now." : "Address saved.");
        });
      });
      card.appendChild(el("div", { class: "form sso" }, [fld("Receiver address", url, "https only. Put a receiver's own key in the address's query if it needs one.")]));
      card.appendChild(out);
      var acts = [save];
      if (cur) {
        var test = el("button", { class: "btn ghost sm", type: "button", text: "Send a test event" });
        test.addEventListener("click", function () {
          test.disabled = true;
          K.api("POST", path("/audit-export/test"), {}, {}).then(function (r) {
            test.disabled = false;
            if (!r.ok) return bad(out, r, { what: "send a test event", whoCan: "an owner" });
            D.toast("Test event queued. It arrives in a few seconds; refresh to see it delivered.");
            redraw(r.body.export);
          });
        });
        var rot = el("button", { class: "btn ghost sm", type: "button", text: "New secret" });
        rot.addEventListener("click", function () {
          D.confirmSheet({
            title: "Make a new signing secret?",
            body: "The old one stops signing at once. Events your receiver checks with the old secret will fail until you give it the new one.",
            yes: "Make a new secret", no: "Keep the old one", busy: "Making",
            ctx: { what: "make a new secret", whoCan: "an owner" },
            act: function () { return K.api("POST", path("/audit-export/secret"), {}, {}); },
            done: function (r) { redraw(r.body, r.body.secret); },
          });
        });
        var off = el("button", { class: "btn quiet sm", type: "button", text: "Stop sending" });
        off.addEventListener("click", function () {
          var key = K.key();
          D.confirmSheet({
            title: "Stop sending the activity?",
            body: "Nothing more is sent, and events still waiting are dropped. Activity here keeps everything, as it always does.",
            yes: "Stop sending", no: "Keep sending", busy: "Stopping",
            ctx: { what: "stop the audit export", whoCan: "an owner" },
            act: function () { return K.api("DELETE", path("/audit-export"), null, { key: key }); },
            done: function () { D.toast("The activity is no longer sent anywhere."); D.show("settings"); },
          });
        });
        acts.push(test, rot, off);
      }
      card.appendChild(el("div", { class: "row2" }, acts));

      function redraw(next, s) {
        card.textContent = "";
        card.appendChild(el("h3", { text: "Send the activity to your logs" }));
        card.appendChild(el("p", { text: "Every event in Activity, posted as JSON to your SIEM or log collector as it happens, signed so it can check each one came from Krate." }));
        exportBody(card, next, s);
      }
    }

    return { samlCard: samlCard, scimCard: scimCard, exportCard: exportCard };
  };
})(typeof window !== "undefined" ? window : globalThis);
