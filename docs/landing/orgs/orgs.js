/* /orgs/: the organisations you are in, and a new one (IC-947), set up in
 * the first minute (IC-958 and its one-minute addendum): the name only,
 * then a first app released, then the team invited. Everything else waits
 * in the organisation's Getting started list.
 *
 *   /orgs/                      the list, or Create when there is none
 *   /orgs/?o=<slug>&step=app    the first app, for an organisation you own
 *   /orgs/?o=<slug>&step=done   it is released: the link and the invite
 *
 * The step is in the address, so a reload goes on where it was.
 */
(function () {
  "use strict";
  var K = window.KrateOrg;
  var el = K.el;
  var $ = function (id) { return document.getElementById(id); };
  var HERE = "/orgs/";
  var SLUG = /^[a-z][a-z0-9-]{1,38}$/;
  var q = new URLSearchParams(location.search);

  function signedOut() {
    $("in").hidden = true;
    $("wiz").hidden = true;
    var box = $("outChoices");
    box.textContent = "";
    box.appendChild(K.signInChoices(location.pathname + location.search));
    $("out").hidden = false;
  }

  if (!K.token()) { signedOut(); return; }
  // Back from a company sign-in: what it did, in words, above the list.
  var signedVia = K.takeSsoNotice();
  if (signedVia) $("in").insertBefore(el("div", { class: "call ok", role: "status", style: "margin-bottom:16px" }, [el("div", { text: signedVia })]), $("in").firstChild);

  var chev = function () {
    var s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    s.setAttribute("class", "chev");
    s.setAttribute("viewBox", "0 0 12 12");
    s.setAttribute("fill", "none");
    s.setAttribute("stroke", "currentColor");
    s.setAttribute("stroke-width", "1.6");
    s.setAttribute("aria-hidden", "true");
    var p = document.createElementNS("http://www.w3.org/2000/svg", "path");
    p.setAttribute("d", "M4.5 2.5L8 6l-3.5 3.5");
    s.appendChild(p);
    return s;
  };

  function showList(orgs) {
    var list = $("list");
    list.textContent = "";
    if (!orgs.length) {
      // A first visit: only the one thing to do, nothing to cancel back to.
      list.hidden = true;
      document.querySelector(".ohead").hidden = true;
      $("cCancel").hidden = true;
      $("createFine").hidden = false;
      openCreate();
      try { $("cName").focus(); } catch (e) {}
      return;
    }
    orgs.forEach(function (o) {
      var name = o.name || o.slug;
      list.appendChild(el("a", { class: "orow", href: "/org/?o=" + encodeURIComponent(o.slug) }, [
        el("span", { class: "otile", "aria-hidden": "true", text: name.slice(0, 1).toUpperCase() }),
        el("span", { class: "t" }, [el("b", { text: name }), el("small", { text: o.slug })]),
        el("span", { class: "tag" + (o.role === "owner" ? " blue" : ""), text: K.ROLE_NAME[o.role] || o.role }),
        chev(),
      ]));
    });
  }

  function load() {
    $("in").hidden = false;
    K.api("GET", "/orgs").then(function (res) {
      if (res.ok) { showList(Array.isArray(res.body) ? res.body : []); return; }
      if (res.body.error === "not-signed-in") { K.forget(); signedOut(); return; }
      var list = $("list");
      list.textContent = "";
      list.appendChild(K.refusalBlock(K.refusal(res.body.error, { here: HERE, message: res.body.message })));
    });
  }

  // ------------------------------------------------------------ create
  var slugTouched = false;
  var pending = null; // {sig, key}: the same form sent twice is one request
  var checkSeq = 0;
  var checked = {};   // slug -> true (free) / false (taken), from the hub

  function openCreate() {
    $("create").hidden = false;
    $("newBtn").hidden = true;
    // The name only; the address is made from it and waits behind Change.
    if (!slugTouched) $("cSlugBox").hidden = true;
    sayAddr();
  }
  function closeCreate() {
    $("create").hidden = true;
    $("newBtn").hidden = false;
    $("cWhy").hidden = true;
  }
  function slugFrom(name) {
    var s = name.toLowerCase().normalize("NFKD").replace(/[̀-ͯ]/g, "")
      .replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").replace(/^[0-9-]+/, "");
    return s.slice(0, 39).replace(/-+$/, "");
  }
  function why(words) {
    var box = $("cWhy");
    box.textContent = "";
    box.appendChild(K.refusalBlock(words, "bad"));
    box.hidden = false;
  }
  function goLabel() {
    var n = $("cName").value.trim();
    $("cGo").textContent = n ? "Create " + (n.length > 28 ? n.slice(0, 27) + "…" : n) : "Create organisation";
  }

  /// The address line under the name: what it will be, and whether it is
  /// free. A taken one is replaced by a free one at once, so the person
  /// never has to invent a second name; they can still change it.
  function sayAddr(note) {
    var s = $("cSlug").value;
    $("cAddr").hidden = !s || !$("cSlugBox").hidden;
    $("cAddrText").textContent = (note ? note + " " : "") + "Its address: krate.tech/org/" + s;
  }
  function alternatives(base) {
    var b = base.slice(0, 33);
    return [b + "-team", b + "-hq", b + "-co", b + "-2", b + "-3"];
  }
  function checkFree(s) {
    if (checked[s] !== undefined) return Promise.resolve(checked[s]);
    return K.api("GET", "/orgs/" + s + "/available").then(function (res) {
      // An older hub without the check: say nothing, the create answer will.
      if (!res.ok || !res.body || typeof res.body.available !== "boolean") return null;
      checked[s] = res.body.available;
      return res.body.available;
    });
  }
  function liveCheck() {
    var s = $("cSlug").value;
    var seq = ++checkSeq;
    if (!SLUG.test(s)) { sayAddr(); return; }
    setTimeout(function () {
      if (seq !== checkSeq) return;
      checkFree(s).then(function (free) {
        if (seq !== checkSeq || free !== false) return;
        if (slugTouched) { sayAddr(s + " is taken."); $("cSlugHint").textContent = s + " is taken. Try another."; return; }
        var alts = alternatives(s);
        (function next(i) {
          if (i >= alts.length || seq !== checkSeq) return;
          checkFree(alts[i]).then(function (ok) {
            if (seq !== checkSeq) return;
            if (ok) { $("cSlug").value = alts[i]; sayAddr(s + " is taken, so"); return; }
            next(i + 1);
          });
        })(0);
      });
    }, 350);
  }

  $("newBtn").addEventListener("click", function () { openCreate(); $("cName").focus(); });
  $("cCancel").addEventListener("click", closeCreate);
  $("cAddrEdit").addEventListener("click", function () { $("cSlugBox").hidden = false; $("cAddr").hidden = true; $("cSlug").focus(); });
  $("cName").addEventListener("input", function () {
    if (!slugTouched) $("cSlug").value = slugFrom($("cName").value);
    goLabel();
    sayAddr();
    liveCheck();
  });
  $("cSlug").addEventListener("input", function () {
    slugTouched = true;
    var v = $("cSlug").value.toLowerCase().replace(/[^a-z0-9-]/g, "");
    if (v !== $("cSlug").value) $("cSlug").value = v;
    $("cSlugHint").textContent = "Lower-case letters, digits and dashes. It cannot be changed later.";
    liveCheck();
  });
  $("createForm").addEventListener("submit", function (e) {
    e.preventDefault();
    var name = $("cName").value.trim();
    var slug = $("cSlug").value.trim();
    if (!name) { why({ title: "Give it a name", body: "The name your team sees, for example Acme.", action: null }); $("cName").focus(); return; }
    if (!SLUG.test(slug)) { $("cSlugBox").hidden = false; $("cAddr").hidden = true; why(K.refusal("slug-invalid")); $("cSlug").focus(); return; }
    var body = { slug: slug, name: name };
    // The organisation's days start at midnight where its owner is (F-319).
    if (K.zone()) body.time_zone = K.zone();
    var sig = JSON.stringify(body);
    if (!pending || pending.sig !== sig) pending = { sig: sig, key: K.key() };
    var go = $("cGo");
    go.disabled = true;
    go.textContent = "Creating " + name;
    K.api("POST", "/orgs", body, { key: pending.key }).then(function (res) {
      go.disabled = false;
      goLabel();
      if (res.ok) {
        var org = (res.body && res.body.org) || { slug: slug, name: name };
        wizard(org, { role: "owner", principal: (res.body && res.body.membership && res.body.membership.principal) || "" }, [], "app");
        return;
      }
      if (res.body.error === "not-signed-in") { K.forget(); signedOut(); return; }
      if (res.body.error === "slug-taken") { $("cSlugBox").hidden = false; $("cAddr").hidden = true; }
      why(K.refusal(res.body.error, { slug: slug, here: HERE, message: res.body.message }));
    });
  });

  // ------------------------------------------------------------ wizard
  /// The steps after Create. `step` is "app" or "done".
  function wizard(org, me, members, step, release) {
    var slug = org.slug;
    var setup = window.KrateOrgSetup({
      slug: slug, here: location.pathname + location.search,
      org: function () { return org; }, role: function () { return me.role; }, members: function () { return members; },
    });
    $("in").hidden = true;
    $("out").hidden = true;
    $("wiz").hidden = false;
    var dash = "/org/?o=" + encodeURIComponent(slug) + "&new=1";
    function mark(n) {
      var box = $("wSteps");
      box.textContent = "";
      ["Name", "First app", "Your team"].forEach(function (t, i) {
        var li = el("li", { class: i < n ? "done" : i === n ? "on" : "", text: t });
        if (i === n) li.setAttribute("aria-current", "step");
        box.appendChild(li);
      });
    }
    function at(s, extra) {
      try { history.replaceState(null, "", "/orgs/?o=" + encodeURIComponent(slug) + "&step=" + s + (extra || "")); } catch (e) {}
    }
    var foot = $("wFoot");
    function footLink(label) {
      foot.textContent = "";
      foot.appendChild(el("a", { class: "btn link sm", href: dash, text: label }));
    }
    document.title = "Set up " + (org.name || slug);
    if (step === "done") {
      mark(2);
      setup.released($("wBody"), release, K.appName(release), {});
      footLink("Go to " + (org.name || slug) + "'s dashboard");
      return;
    }
    mark(1);
    at("app");
    footLink("Skip for now");
    setup.firstApp($("wBody"), {
      onDone: function (rel, label) {
        mark(2);
        at("done");
        setup.released($("wBody"), rel, label, {});
        footLink("Go to " + (org.name || slug) + "'s dashboard");
      },
      onWaiting: function (label) {
        // The two-person rule held it: someone else approves it.
        location.href = "/org/?o=" + encodeURIComponent(slug) + "&tab=waiting&sent=" + encodeURIComponent(label);
      },
    });
  }

  /// A reload in the middle: back to the step, for an organisation this
  /// person can set up; anything else goes to its dashboard.
  function resume(slug, step) {
    $("in").hidden = true;
    K.api("GET", "/orgs/" + slug).then(function (res) {
      if (!res.ok) {
        if (res.body.error === "not-signed-in") { K.forget(); signedOut(); return; }
        try { history.replaceState(null, "", HERE); } catch (e) {}
        load();
        return;
      }
      var org = res.body.org, me = res.body.me || {};
      if (me.role !== "owner" && me.role !== "admin") { location.replace("/org/?o=" + encodeURIComponent(slug)); return; }
      K.api("GET", "/orgs/" + slug + "/releases").then(function (r) {
        var rels = r.ok && Array.isArray(r.body) ? r.body : [];
        if (rels.length) wizard(org, me, res.body.members || [], "done", rels[0]);
        else if (step === "app") wizard(org, me, res.body.members || [], "app");
        else location.replace("/org/?o=" + encodeURIComponent(slug));
      });
    });
  }

  var rs = (q.get("o") || "").toLowerCase(), rstep = q.get("step") || "";
  if (SLUG.test(rs) && (rstep === "app" || rstep === "done")) resume(rs, rstep);
  else load();
})();
