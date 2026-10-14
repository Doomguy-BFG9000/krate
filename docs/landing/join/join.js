/* /join/?o=<slug>&c=<code>: an invitation, then Accept (IC-947).
 *
 * Signed out, the page knows only the organisation's short name, which is
 * in the link anyway. Signed in, the hub shows the invitation to the
 * person it names and to nobody else (CONTRACT.md Addendum 2).
 */
(function () {
  "use strict";
  var K = window.KrateOrg;
  var el = K.el;
  var $ = function (id) { return document.getElementById(id); };
  var q = new URLSearchParams(location.search);
  var slug = (q.get("o") || "").toLowerCase();
  var code = q.get("c") || "";
  var HERE = "/join/?o=" + encodeURIComponent(slug) + "&c=" + encodeURIComponent(code);

  function say(h, p, kids) {
    $("jH").textContent = h;
    var para = $("jP");
    para.textContent = p || "";
    para.className = "";
    para.hidden = !p;
    var body = $("jBody");
    body.textContent = "";
    (kids || []).forEach(function (k) { if (k) body.appendChild(k); });
  }

  function refused(reason, extra) {
    var ctx = { org: slug, slug: slug, here: HERE };
    for (var k in extra || {}) ctx[k] = extra[k];
    var w = K.refusal(reason, ctx);
    document.title = w.title;
    say(w.title, w.body, [
      w.action ? el("div", { class: "stack" }, [el("a", { class: "btn dark", href: w.action.href, text: w.action.label })]) : null,
    ]);
  }

  if (!/^[a-z][a-z0-9-]{1,38}$/.test(slug) || !/^[A-Za-z0-9_-]{6,80}$/.test(code)) {
    say("This invitation link is incomplete", "Copy the whole link from the message you were sent: it ends with a code after c=. Or ask for a new one.", [
      el("div", { class: "stack" }, [el("a", { class: "btn ghost", href: "/orgs/", text: "Your organisations" })]),
    ]);
    return;
  }
  $("jTile").textContent = slug.slice(0, 1).toUpperCase();

  if (!K.token()) {
    say("You have an invitation to " + slug, "Sign in with the account it was sent to, and you come straight back here to accept it.", [
      K.signInChoices(HERE),
      el("p", { class: "jfine", text: "New to Krate? Signing in makes your account. It is free." }),
    ]);
    return;
  }

  /// Who this browser is signed in as, for "this invitation is for someone
  /// else". Best effort: the refusal reads fine without it.
  function whoAmI() {
    return K.api("GET", "/me").then(function (res) {
      var u = res.ok && res.body && res.body.user;
      if (!u) return "";
      return u.email || (u.login ? "@" + u.login : "");
    });
  }

  function refuse(res) {
    var reason = res.body.error;
    if (reason === "not-signed-in") {
      K.forget();
      location.replace(HERE);
      return;
    }
    if (reason === "invite-for-someone-else") {
      whoAmI().then(function (me) { refused(reason, { me: me }); });
      return;
    }
    refused(reason, { message: res.body.message });
  }

  var path = "/orgs/" + slug + "/invites/" + encodeURIComponent(code);
  K.api("GET", path).then(function (res) {
    if (!res.ok) { refuse(res); return; }
    var inv = res.body;
    var org = (inv.org && inv.org.name) || slug;
    var by = K.who(inv.invited_by);
    var forWho = K.who(inv["for"]);
    var role = K.ROLE_NAME[inv.role] || inv.role;
    document.title = "Join " + org;
    $("jTile").textContent = org.slice(0, 1).toUpperCase();
    var accept = el("button", { class: "btn dark", type: "button", text: "Accept and join " + org });
    var key = K.key();
    accept.addEventListener("click", function () {
      accept.disabled = true;
      accept.textContent = "Joining";
      K.api("POST", path + "/accept", {}, { key: key }).then(function (out) {
        accept.disabled = false;
        accept.textContent = "Accept and join " + org;
        if (!out.ok) { refuse(out); return; }
        var r = (out.body.membership && out.body.membership.role) || inv.role;
        var rn = (K.ROLE_NAME[r] || r).toLowerCase();
        document.title = "You joined " + org;
        say("You are in " + org, "You joined as " + (/^[aeiou]/.test(rn) ? "an " : "a ") + rn + ". " + K.ROLE_SAYS[r], [
          el("div", { class: "stack" }, [el("a", { class: "btn dark", href: "/org/?o=" + encodeURIComponent(slug), text: "Open " + org })]),
        ]);
      });
    });
    say("Join " + org, by.label + " invited " + forWho.label + " to " + org + " on Krate.", [
      el("div", { class: "jrole" }, [el("b", { text: "As " + role.toLowerCase() }), el("span", { text: K.ROLE_SAYS[inv.role] || "" })]),
      el("div", { class: "stack" }, [accept]),
      el("p", { class: "jfine", text: "This invitation works once, until " + K.day(inv.expires_at) + "." }),
    ]);
  });
})();
