/* /orgs/: the organisations you are in, and a new one (IC-947).
 *
 * The list is the hub's GET /orgs, a display index. Opening one asks that
 * organisation's own store (R5), on /org/.
 */
(function () {
  "use strict";
  var K = window.KrateOrg;
  var el = K.el;
  var $ = function (id) { return document.getElementById(id); };
  var HERE = "/orgs/";
  var SLUG = /^[a-z][a-z0-9-]{1,38}$/;

  function signedOut() {
    $("in").hidden = true;
    var box = $("outChoices");
    box.textContent = "";
    box.appendChild(K.signInChoices(HERE));
    $("out").hidden = false;
  }

  if (!K.token()) { signedOut(); return; }
  $("in").hidden = false;

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
      list.appendChild(el("div", { class: "oempty" }, [
        el("b", { text: "You are not in an organisation yet" }),
        el("p", { text: "Create one for your team, or open the invitation link someone sent you." }),
      ]));
      openCreate();
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
  var pending = null; // {body, key}: the same form sent twice is one request

  function openCreate() {
    $("create").hidden = false;
    $("newBtn").hidden = true;
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

  $("newBtn").addEventListener("click", function () { openCreate(); $("cName").focus(); });
  $("cCancel").addEventListener("click", closeCreate);
  $("cName").addEventListener("input", function () {
    if (!slugTouched) $("cSlug").value = slugFrom($("cName").value);
  });
  $("cSlug").addEventListener("input", function () {
    slugTouched = true;
    var v = $("cSlug").value.toLowerCase().replace(/[^a-z0-9-]/g, "");
    if (v !== $("cSlug").value) $("cSlug").value = v;
  });
  $("createForm").addEventListener("submit", function (e) {
    e.preventDefault();
    var name = $("cName").value.trim();
    var slug = $("cSlug").value.trim();
    if (!name) { why({ title: "Give it a name", body: "The name people see, for example Acme.", action: null }); $("cName").focus(); return; }
    if (!SLUG.test(slug)) { why(K.refusal("slug-invalid")); $("cSlug").focus(); return; }
    var body = { slug: slug, name: name };
    var sig = JSON.stringify(body);
    if (!pending || pending.sig !== sig) pending = { sig: sig, key: K.key() };
    var go = $("cGo");
    go.disabled = true;
    go.textContent = "Creating";
    K.api("POST", "/orgs", body, { key: pending.key }).then(function (res) {
      go.disabled = false;
      go.textContent = "Create organisation";
      if (res.ok) {
        location.href = "/org/?o=" + encodeURIComponent(slug) + "&tab=people&new=1";
        return;
      }
      if (res.body.error === "not-signed-in") { K.forget(); signedOut(); return; }
      why(K.refusal(res.body.error, { slug: slug, here: HERE, message: res.body.message }));
    });
  });

  load();
})();
