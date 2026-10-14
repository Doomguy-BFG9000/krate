// Company connections (IC-952, ORG-FIRST-PLAN.md "IC-952 contract").
//
// An owner or admin defines a connection once: a name (`finance-api`), the
// base it covers (an https address: host and path prefix), the credential
// (one header, or a user and password sent as Basic), what an app may do
// with it (methods and path prefixes; GET only unless they say otherwise),
// who may use it (everyone, or groups) and which apps (any of the
// organisation's, or named app ids).
//
// The credential is write-only. For now the Worker encrypts it at rest with
// the hub's organisation secret key (ORG_SECRET_KEY, AES-GCM, bound to the
// organisation and the connection's name) before the store sees it, so the
// hub could read it; F-316 moves it onto the organisation keyring through
// the credential seam below, after which only members' devices can. No
// answer but the one to an allowed device, no audit event and no log line
// ever carries it, and it is replaced,
// never edited. Moving a connection to another base needs a new
// credential: otherwise an admin who never knew the secret could send it
// to a server of their own and read it there.
//
// A device fetches the credential when an app that asks for
// `net.connection:<name>` opens (GET .../connections/<name>/credential/
// <digest>). That fetch is checked the way a lease is: the person's
// standing, the release (not withdrawn, inside its audience), then the
// connection's own audience, its app list, and that the release itself
// asked for the connection. The runtime keeps the credential in memory for
// that one run and adds it to the app's requests to the base; the app never
// sees it. Each use is counted per day and the count goes in the audit as
// one line a day.
//
// What this cannot do, and the docs say: the request goes from the
// person's device, so the credential is on that device, in Krate's memory,
// for the run. Someone who controls their own machine can read their own
// memory. The guards here decide who gets it at all.

import { Org, extendOrg, addReasons, allows } from "./org.js";
// Release control first: a credential fetch reuses its audience and
// withdrawn checks, and this module's order is its own, not index.js's.
import { GROUP, appOf } from "./org-releases.js";
import { sealSecret, openSecret, keyedDigest } from "./org-sso.js";

export const CONNECTION = /^[a-z][a-z0-9-]{1,38}$/;
export const METHODS = ["GET", "HEAD", "OPTIONS", "POST", "PUT", "PATCH", "DELETE"];
const APP_ID = /^[a-z0-9][a-z0-9._-]{0,119}$/;
const MAX_PATHS = 50;
const MAX_APPS = 100;
/// The runtime refuses a header value over 4 KiB (adapter-common net.rs),
/// so a longer credential could never be sent.
const MAX_VALUE = 4096;
/// Headers the transport owns. A credential in one would be rewritten or
/// refused on the way out, so it is refused here, when it can be fixed.
const TRANSPORT_HEADERS = new Set([
  "host", "content-length", "transfer-encoding", "connection", "upgrade", "te", "trailer", "keep-alive", "expect",
]);
const DAY = 24 * 60 * 60;

addReasons({
  "no-such-connection": {
    status: 404,
    message: (o, d) => d || `${o} has no connection by that name -- an owner or admin of ${o} sets one up in Settings`,
  },
  "connection-taken": { status: 409, message: (o, d) => d || `${o} already has a connection by that name` },
  "connection-invalid": { status: 400, message: (o, d) => d || "that connection is not complete" },
  "credential-required": {
    status: 400,
    message: () =>
      "moving a connection to another address needs its credential again: the stored one is never sent anywhere new",
  },
  "connection-not-in-audience": {
    status: 403,
    message: (o, d) => d || `this connection is only for some groups in ${o} -- ask an admin of ${o} to add you to one`,
  },
  "app-not-allowed": {
    status: 403,
    message: (o, d) => d || `${o} has not let this app use that connection -- an owner or admin of ${o} can add it`,
  },
  "connection-not-requested": {
    status: 403,
    message: (o, d) => d || "this version of the app did not ask for that connection, so it does not get it",
  },
});

const SCHEMA = [
  `CREATE TABLE IF NOT EXISTS connections (
     name TEXT PRIMARY KEY, base TEXT NOT NULL,
     methods TEXT NOT NULL, paths TEXT NOT NULL,
     audience TEXT, apps TEXT,
     kind TEXT NOT NULL, header_name TEXT NOT NULL, sealed TEXT NOT NULL,
     created_at INTEGER NOT NULL, created_by TEXT NOT NULL,
     changed_at INTEGER NOT NULL, changed_by TEXT NOT NULL,
     credential_at INTEGER NOT NULL, credential_by TEXT NOT NULL)`,
  `CREATE TABLE IF NOT EXISTS connection_uses (
     name TEXT NOT NULL, day TEXT NOT NULL, n INTEGER NOT NULL,
     audited INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (name, day))`,
];

// Which connections each candidate asked for (its manifest's
// `net.connection:<name>` capabilities), kept when it is uploaded.
const COLUMNS = [["candidates", "connections", "TEXT"]];

const isoDay = (seconds) => new Date(seconds * 1000).toISOString().slice(0, 10);

const list = (text) => {
  if (!text) return null;
  try {
    const v = JSON.parse(text);
    return Array.isArray(v) ? v : null;
  } catch (_) {
    return [];
  }
};

/// "finance", "finance and ops", "finance, ops and legal".
function spoken(names) {
  if (names.length <= 1) return names.join("");
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

// ------------------------------------------------------- what is valid

/// The hub is this machine's loopback: a local `wrangler dev` proof. Only
/// then may a connection's base be plain http, and only to this machine.
function devHub(env) {
  return /^http:\/\/(127\.0\.0\.1|localhost|\[::1\])(:\d+)?\/?$/.test(String((env && env.PUBLIC_BASE) || ""));
}

/// A connection's base as it is kept: `https://host[:port]/path`, the path
/// without a trailing slash (the root is ""). Answers the text, or null
/// with the words of why.
export function normalBase(raw, env) {
  let u;
  try {
    u = new URL(String(raw || "").trim());
  } catch (_) {
    return { problem: "the address is a full URL, such as https://api.acme.com/finance" };
  }
  if (u.username || u.password) return { problem: "the address cannot carry a user or password: put those in the credential" };
  if (u.search || u.hash || String(raw).includes("?") || String(raw).includes("#")) {
    return { problem: "the address is a host and a path, with no ? or # part" };
  }
  const loop = /^(127\.0\.0\.1|localhost|\[::1\])$/.test(u.hostname);
  if (u.protocol !== "https:" && !(u.protocol === "http:" && loop && devHub(env))) {
    return { problem: "the address must start with https:// so the credential is never sent in the clear" };
  }
  if (u.hostname.startsWith("[")) return { problem: "an IPv6 address cannot be a connection's address yet: use a host name" };
  const path = u.pathname.replace(/\/+$/, "");
  // Judged on what was typed too: the URL parser quietly folds `a/../b`
  // into `b`, and the address kept must be the one the admin read.
  const typed = String(raw).replace(/^[a-z]+:\/\/[^/]*/i, "");
  if (/%|\/\.\.?(\/|$)|\\/.test(path) || /%|\/\.\.?(\/|$)|\\/.test(typed)) {
    return { problem: "the address's path cannot hold %, . or .. parts" };
  }
  const port = u.port ? `:${u.port}` : "";
  const base = `${u.protocol}//${u.hostname.toLowerCase()}${port}${path}`;
  if (base.length > 300) return { problem: "the address is too long" };
  return { base };
}

/// `allow`: `{methods, paths}`, GET on everything under the base unless
/// they say otherwise.
export function normalAllow(raw) {
  const a = raw && typeof raw === "object" ? raw : {};
  const methods = a.methods === undefined || a.methods === null ? ["GET"] : a.methods;
  const paths = a.paths === undefined || a.paths === null ? ["/"] : a.paths;
  if (!Array.isArray(methods) || !methods.length) return { problem: "name at least one method, such as GET" };
  const m = [...new Set(methods.map((x) => String(x || "").trim().toUpperCase()))];
  const badMethod = m.find((x) => !METHODS.includes(x));
  if (badMethod !== undefined) return { problem: `a method is one of ${METHODS.join(", ")}` };
  if (!Array.isArray(paths) || !paths.length || paths.length > MAX_PATHS) {
    return { problem: `name 1 to ${MAX_PATHS} paths under the address, such as /invoices, or / for all of it` };
  }
  const p = [];
  for (const raw of paths) {
    let x = String(raw || "").trim();
    if (!x.startsWith("/")) x = `/${x}`;
    if (x.length > 1) x = x.replace(/\/+$/, "") || "/";
    if (x.length > 200 || /[%?#\\\s]|\/\.\.?(\/|$)/.test(x)) {
      return { problem: `the path ${JSON.stringify(String(raw).slice(0, 40))} is not one Krate can match exactly: no %, ?, #, spaces, . or .. parts` };
    }
    if (!p.includes(x)) p.push(x);
  }
  return { allow: { methods: METHODS.filter((x) => m.includes(x)), paths: p.sort() } };
}

/// The credential a person typed: one header, or Basic. Answers
/// `{kind, header_name, value}` or `{problem}`. Never echoes the value.
export function credentialOf(v) {
  if (v && v.header && typeof v.header === "object") {
    const name = String(v.header.name || "").trim();
    const value = String(v.header.value == null ? "" : v.header.value);
    if (!/^[A-Za-z0-9!#$%&'*+.^_`|~-]{1,64}$/.test(name)) return { problem: "the header name is one word, such as Authorization or X-Api-Key" };
    if (TRANSPORT_HEADERS.has(name.toLowerCase()) || name.toLowerCase().startsWith("proxy-")) {
      return { problem: `${name} is a header the connection itself sets, so it cannot carry a credential` };
    }
    if (!value.trim() || value.length > MAX_VALUE || /[\r\n\0]/.test(value)) {
      return { problem: `the header value is one line of up to ${MAX_VALUE} characters` };
    }
    return { kind: "header", header_name: name, value };
  }
  if (v && v.basic && typeof v.basic === "object") {
    const user = String(v.basic.user || "");
    const password = String(v.basic.password == null ? "" : v.basic.password);
    if (!user || user.includes(":") || /[\r\n\0]/.test(user + password) || user.length > 256 || password.length > 1024) {
      return { problem: "a user (without a colon) and a password, each one line" };
    }
    const bytes = new TextEncoder().encode(`${user}:${password}`);
    const value = `Basic ${btoa(String.fromCharCode(...bytes))}`;
    if (value.length > MAX_VALUE) return { problem: "that user and password are too long" };
    return { kind: "basic", header_name: "Authorization", value };
  }
  return null;
}

// ------------------------------------------------------------ the store

const ops = {
  connectionRow(name) {
    return this.row("SELECT * FROM connections WHERE name = ?", name);
  },

  /// What anyone who may see connections reads: everything but the
  /// credential, which is only ever "set, by whom, when".
  connectionView(c, now) {
    const since = isoDay(now - 29 * DAY);
    const today = isoDay(now);
    const uses = this.rows("SELECT day, n FROM connection_uses WHERE name = ? AND day >= ?", c.name, since);
    return {
      name: c.name,
      base: c.base,
      allow: { methods: JSON.parse(c.methods), paths: JSON.parse(c.paths) },
      audience: list(c.audience),
      apps: list(c.apps),
      credential: { kind: c.kind, header: c.header_name, set_at: c.credential_at, set_by: c.credential_by },
      created_at: c.created_at,
      created_by: c.created_by,
      changed_at: c.changed_at,
      changed_by: c.changed_by,
      uses: {
        today: uses.filter((u) => u.day === today).reduce((s, u) => s + u.n, 0),
        last_30_days: uses.reduce((s, u) => s + u.n, 0),
      },
    };
  },

  /// The command's member, refused unless their role may do `action`.
  connectionMay(ctx, action, target, words) {
    const { me, refusal } = this.standing(ctx, action, target);
    if (refusal) return { refusal };
    if (allows(me.role, action) !== "yes") return { refusal: this.deny(ctx, action, target, "role-cannot", words(me.role)) };
    return { me };
  },

  /// Audience and app list from a command's arguments, checked. `fallback`
  /// is the connection as it is, for a change that leaves one out.
  connectionScope(ctx, action, target, fallback) {
    const a = ctx.args;
    let audience = fallback ? fallback.audience : null;
    if (a.audience !== undefined) {
      const au = a.audience;
      if (au === null || au === "everyone" || (au && au.everyone === true)) {
        audience = null;
      } else {
        const raw = Array.isArray(au) ? au : au && Array.isArray(au.groups) ? au.groups : null;
        if (!raw || !raw.length || raw.length > 50) {
          return { refusal: this.deny(ctx, action, target, "connection-invalid", "name the groups who may use it, or say everyone") };
        }
        const groups = [...new Set(raw.map((g) => String(g || "").trim().toLowerCase()))].sort();
        if (groups.some((g) => !GROUP.test(g))) return { refusal: this.deny(ctx, action, target, "group-invalid") };
        const missing = groups.find((g) => !this.groupRow(g));
        if (missing) return { refusal: this.deny(ctx, action, target, "no-such-group", `${ctx.slug} has no group called ${missing} -- create it first`) };
        audience = JSON.stringify(groups);
      }
    }
    let apps = fallback ? fallback.apps : null;
    if (a.apps !== undefined) {
      const ap = a.apps;
      if (ap === null || ap === "any" || (ap && ap.any === true)) {
        apps = null;
      } else {
        const raw = Array.isArray(ap) ? ap : ap && Array.isArray(ap.ids) ? ap.ids : null;
        if (!raw || !raw.length || raw.length > MAX_APPS) {
          return { refusal: this.deny(ctx, action, target, "connection-invalid", "name the app ids that may use it (com.acme.ledger), or say any") };
        }
        const ids = [...new Set(raw.map((x) => String(x || "").trim().toLowerCase()))].sort();
        const bad = ids.find((x) => !APP_ID.test(x));
        if (bad !== undefined) {
          return { refusal: this.deny(ctx, action, target, "connection-invalid", `${JSON.stringify(bad.slice(0, 40))} is not an app id: it is the id in the app's manifest, such as com.acme.ledger`) };
        }
        apps = JSON.stringify(ids);
      }
    }
    return { audience, apps };
  },

  /// GET /orgs/<slug>/connections.
  op_connections_list(ctx) {
    const { refusal } = this.connectionMay(ctx, "connection.view", `org:${ctx.slug}`, (r) => `a ${r} cannot see ${ctx.slug}'s connections`);
    if (refusal) return refusal;
    return this.ok({ connections: this.rows("SELECT * FROM connections ORDER BY name").map((c) => this.connectionView(c, ctx.now)) });
  },

  /// POST /orgs/<slug>/connections. The Worker has sealed the credential;
  /// it arrives as ciphertext with its kind and header name.
  op_connection_create(ctx) {
    const a = ctx.args;
    const name = String(a.name || "").trim().toLowerCase();
    const target = `connection:${name.slice(0, 40)}`;
    const { me, refusal } = this.connectionMay(ctx, "connection.manage", target, (r) => `a ${r} cannot set up ${ctx.slug}'s connections`);
    if (refusal) return refusal;
    if (!CONNECTION.test(name)) {
      return this.deny(ctx, "connection.manage", target, "connection-invalid", "a connection's name is 2 to 39 characters: lower-case letters, digits and dashes, starting with a letter");
    }
    if (this.connectionRow(name)) return this.deny(ctx, "connection.manage", target, "connection-taken", `${ctx.slug} already has a connection called ${name}`);
    if (!a.base || !a.allow) return this.deny(ctx, "connection.manage", target, "connection-invalid", "give its address and what it allows");
    if (!a.sealed) return this.deny(ctx, "connection.manage", target, "connection-invalid", "give its credential: a header and its value, or a user and password");
    const scope = this.connectionScope(ctx, "connection.manage", target, null);
    if (scope.refusal) return scope.refusal;
    this.sql.exec(
      `INSERT INTO connections (name, base, methods, paths, audience, apps, kind, header_name, sealed,
         created_at, created_by, changed_at, changed_by, credential_at, credential_by)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      name, a.base, JSON.stringify(a.allow.methods), JSON.stringify(a.allow.paths), scope.audience, scope.apps,
      a.kind, a.header_name, JSON.stringify(a.sealed),
      ctx.now, me.principal, ctx.now, me.principal, ctx.now, me.principal,
    );
    this.audit(me.principal, "connection.create", target, "allowed", a.base);
    return this.ok({ connection: this.connectionView(this.connectionRow(name), ctx.now) }, 201);
  },

  /// PUT /orgs/<slug>/connections/<name>: address, allow, audience, apps.
  /// A new address needs a new credential with it.
  op_connection_update(ctx) {
    const a = ctx.args;
    const name = String(a.name || "").toLowerCase();
    const target = `connection:${name.slice(0, 40)}`;
    const { me, refusal } = this.connectionMay(ctx, "connection.manage", target, (r) => `a ${r} cannot change ${ctx.slug}'s connections`);
    if (refusal) return refusal;
    const c = this.connectionRow(name);
    if (!c) return this.deny(ctx, "connection.manage", target, "no-such-connection", `${ctx.slug} has no connection called ${name || "that"}`);
    const base = a.base || c.base;
    if (base !== c.base && !a.sealed) return this.deny(ctx, "connection.manage", target, "credential-required");
    const scope = this.connectionScope(ctx, "connection.manage", target, c);
    if (scope.refusal) return scope.refusal;
    const methods = a.allow ? JSON.stringify(a.allow.methods) : c.methods;
    const paths = a.allow ? JSON.stringify(a.allow.paths) : c.paths;
    const said = [];
    if (base !== c.base) said.push(`address ${base}`);
    if (methods !== c.methods || paths !== c.paths) said.push(`allow ${JSON.parse(methods).join(",")} ${JSON.parse(paths).join(",")}`);
    if (scope.audience !== c.audience) said.push(scope.audience ? `audience groups:${list(scope.audience).join(",")}` : "audience everyone");
    if (scope.apps !== c.apps) said.push(scope.apps ? `apps ${list(scope.apps).join(",")}` : "apps any");
    if (a.sealed) said.push("credential replaced");
    if (said.length) {
      this.sql.exec(
        `UPDATE connections SET base = ?, methods = ?, paths = ?, audience = ?, apps = ?, changed_at = ?, changed_by = ? WHERE name = ?`,
        base, methods, paths, scope.audience, scope.apps, ctx.now, me.principal, name,
      );
      if (a.sealed) this.connectionNewCredential(name, a, me, ctx.now);
      this.audit(me.principal, "connection.change", target, "allowed", said.join("; "));
    }
    return this.ok({ connection: this.connectionView(this.connectionRow(name), ctx.now) });
  },

  connectionNewCredential(name, a, me, now) {
    this.sql.exec(
      "UPDATE connections SET kind = ?, header_name = ?, sealed = ?, credential_at = ?, credential_by = ? WHERE name = ?",
      a.kind, a.header_name, JSON.stringify(a.sealed), now, me.principal, name,
    );
  },

  /// PUT /orgs/<slug>/connections/<name>/credential: replace it. The next
  /// run of every app that uses it gets the new one.
  op_connection_credential(ctx) {
    const a = ctx.args;
    const name = String(a.name || "").toLowerCase();
    const target = `connection:${name.slice(0, 40)}`;
    const { me, refusal } = this.connectionMay(ctx, "connection.manage", target, (r) => `a ${r} cannot change ${ctx.slug}'s connections`);
    if (refusal) return refusal;
    if (!this.connectionRow(name)) return this.deny(ctx, "connection.manage", target, "no-such-connection", `${ctx.slug} has no connection called ${name || "that"}`);
    if (!a.sealed) return this.deny(ctx, "connection.manage", target, "connection-invalid", "give the new credential: a header and its value, or a user and password");
    this.connectionNewCredential(name, a, me, ctx.now);
    this.sql.exec("UPDATE connections SET changed_at = ?, changed_by = ? WHERE name = ?", ctx.now, me.principal, name);
    this.audit(me.principal, "connection.credential", target, "allowed");
    return this.ok({ connection: this.connectionView(this.connectionRow(name), ctx.now) });
  },

  /// DELETE /orgs/<slug>/connections/<name>. The credential goes with it.
  op_connection_remove(ctx) {
    const name = String(ctx.args.name || "").toLowerCase();
    const target = `connection:${name.slice(0, 40)}`;
    const { me, refusal } = this.connectionMay(ctx, "connection.manage", target, (r) => `a ${r} cannot remove ${ctx.slug}'s connections`);
    if (refusal) return refusal;
    if (!this.connectionRow(name)) return this.deny(ctx, "connection.manage", target, "no-such-connection", `${ctx.slug} has no connection called ${name || "that"}`);
    this.connectionFlushUses(ctx.now + DAY, name);
    this.sql.exec("DELETE FROM connections WHERE name = ?", name);
    this.sql.exec("DELETE FROM connection_uses WHERE name = ?", name);
    this.audit(me.principal, "connection.remove", target, "allowed");
    return this.ok(null, 204);
  },

  /// GET /orgs/<slug>/connections/<name>/credential/<digest>: may this
  /// person's copy of this release use this connection now? Checked like a
  /// lease, then counted. Answers the sealed credential for the Worker.
  op_connection_use(ctx) {
    const name = String(ctx.args.name || "").toLowerCase();
    const digest = String(ctx.args.digest || "");
    const target = `connection:${name.slice(0, 40)}`;
    const action = "connection.use";
    const { me, refusal } = this.standing(ctx, action, target);
    if (refusal) return refusal;
    if (allows(me.role, action) !== "yes") return this.deny(ctx, action, target, "role-cannot");
    const c = this.row("SELECT * FROM candidates WHERE digest = ?", digest);
    if (c && c.state === "withdrawn") return this.withdrawnRefusal(ctx, c, action, target);
    if (!c || c.state !== "released") return this.deny(ctx, action, target, "digest-mismatch", `that app is not one of ${ctx.slug}'s releases`);
    const outside = this.audienceRefusal(ctx, me, c, action, target);
    if (outside) return outside;
    const conn = this.connectionRow(name);
    if (!conn) return this.deny(ctx, action, target, "no-such-connection", `${this.org().name || ctx.slug} has no connection called ${name || "that"} any more -- ask an admin of ${this.org().name || ctx.slug}`);
    // From here the person is a member who may open this release, so the
    // address is theirs to know: the device words its refusal with it.
    const refuse = (reason, detail) => {
      const r = this.deny(ctx, action, target, reason, detail);
      r.body.base = conn.base;
      return r;
    };
    const orgName = this.org().name || ctx.slug;
    const audience = list(conn.audience);
    if (audience) {
      const mine = this.groupsOf(me.principal);
      if (!audience.some((g) => mine.has(g))) {
        const r = refuse("connection-not-in-audience", `the ${name} connection is only for the ${spoken(audience)} group${audience.length === 1 ? "" : "s"} in ${orgName} -- ask an admin of ${orgName} to add you`);
        r.body.groups = audience;
        return r;
      }
    }
    const apps = list(conn.apps);
    const app = appOf(c);
    if (apps && !apps.includes(app)) return refuse("app-not-allowed", `${orgName} has not let ${c.name || app} use the ${name} connection -- an owner or admin of ${orgName} can add ${app} to it`);
    const asked = list(c.connections) || [];
    if (!asked.includes(name)) return refuse("connection-not-requested", `this version of ${c.name || app} did not ask for the ${name} connection (net.connection:${name} in its manifest), so it does not get it`);
    this.sql.exec(
      `INSERT INTO connection_uses (name, day, n) VALUES (?, ?, 1)
       ON CONFLICT (name, day) DO UPDATE SET n = n + 1`,
      name,
      isoDay(ctx.now),
    );
    return this.ok({
      name,
      base: conn.base,
      allow: { methods: JSON.parse(conn.methods), paths: JSON.parse(conn.paths) },
      kind: conn.kind,
      header_name: conn.header_name,
      sealed: JSON.parse(conn.sealed),
    });
  },

  /// One audit line per connection per finished day: how many times its
  /// credential was handed out. `before` is the first moment not yet
  /// finished; `only` limits it to one connection.
  connectionFlushUses(before, only) {
    const day = isoDay(before);
    const due = this.rows(
      `SELECT name, day, n FROM connection_uses WHERE audited = 0 AND day < ? ${only ? "AND name = ?" : ""} ORDER BY day, name`,
      ...(only ? [day, only] : [day]),
    );
    for (const u of due) {
      this.audit("system:krate", "connection.use", `connection:${u.name}`, "allowed", `${u.n} use${u.n === 1 ? "" : "s"} on ${u.day}`);
      this.sql.exec("UPDATE connection_uses SET audited = 1 WHERE name = ? AND day = ?", u.name, u.day);
    }
  },
};

function install(OrgClass) {
  Object.assign(OrgClass.prototype, ops);
  extendOrg({
    schema: SCHEMA,
    columns: COLUMNS,
    mutations: ["connection-create", "connection-update", "connection-credential", "connection-remove"],
    // Yesterday's counts become audit lines with the first command of today.
    tick(ctx) {
      this.connectionFlushUses(ctx.now);
    },
  });
  // A candidate keeps the connections its manifest asks for.
  const add = OrgClass.prototype.op_add_candidate;
  OrgClass.prototype.op_add_candidate = function (ctx) {
    const answer = add.call(this, ctx);
    const asked = Array.isArray(ctx.args.connections)
      ? [...new Set(ctx.args.connections.map((n) => String(n || "").toLowerCase()).filter((n) => CONNECTION.test(n)))].sort()
      : [];
    if (answer.status === 201 && asked.length) {
      this.sql.exec("UPDATE candidates SET connections = ? WHERE digest = ? AND connections IS NULL", JSON.stringify(asked), String(ctx.args.digest || ""));
    }
    return answer;
  };
}

install(Org);

/// The connections a judged bundle's manifest asks for.
export function connectionsAsked(judgement) {
  const caps = (judgement && judgement.manifest && judgement.manifest.capabilities) || [];
  return caps
    .map((c) => String((c && c.cap) || ""))
    .filter((cap) => cap.startsWith("net.connection:"))
    .map((cap) => cap.slice("net.connection:".length).toLowerCase());
}

// ------------------------------------------------- the credential seam
//
// Every place a credential is sealed, kept or handed to a device goes
// through these three functions, and nothing else in the hub touches one.
// Today the hub seals it at rest with ORG_SECRET_KEY (AES-GCM, bound to the
// organisation and the connection's name), so the hub itself could open it.
// Founder decision F-316 replaces this with the organisation keyring: the
// admin's browser or CLI seals the credential to the organisation's public
// key (HPKE), the hub keeps that blob as it came, and only a member's
// device opens it. The switch changes these three functions (and their
// partners in the page, the CLI and the runtime) and nothing around them:
// the store keeps whatever `blob` is, and every check made before a
// credential is handed out stays as it is.

const purpose = (slug, name) => `krate-org-connection:${slug}:${name}`;

/// Can this hub take and hand out credentials at all?
export function credentialsOn(env) {
  return Boolean(env && env.ORG_SECRET_KEY);
}

/// What a create or replace carried, made into what the store keeps.
/// Answers null when it carried no credential, `{problem}` for one that
/// cannot be used, `{off: true}` when the hub cannot keep one, or
/// `{blob, kind, header_name, binding}`: the opaque blob the store keeps,
/// and the text the Idempotency-Key is bound to in place of the secret.
export async function credentialToStore(env, slug, name, body) {
  const cred = credentialOf(body);
  if (!cred || cred.problem) return cred;
  const blob = await sealSecret(env, slug, cred.value, purpose(slug, name));
  if (!blob) return { off: true };
  const binding = `${cred.kind}:${cred.header_name}:${await keyedDigest(env, cred.value)}`;
  return { blob, kind: cred.kind, header_name: cred.header_name, binding };
}

/// What a device that may use the connection is handed for it: today the
/// header itself, opened here; null when the blob does not open with this
/// hub's key.
export async function credentialForDevice(env, slug, name, kept) {
  const value = await openSecret(env, slug, kept.blob, purpose(slug, name));
  if (value === null) return null;
  return { header: { name: kept.header_name, value } };
}

// ---------------------------------------------------------------- routes

const noStore = (value, status = 200) =>
  new Response(JSON.stringify(value), { status, headers: { "content-type": "application/json", "cache-control": "private, no-store" } });

const off = () =>
  noStore({ error: "connections-off", message: "company connections are not switched on on this hub (it has no ORG_SECRET_KEY)" }, 503);

/// /orgs/<slug>/connections... Answers a Response, or null.
export async function connectionRoute(h) {
  const { request, env, slug, rest, method, askRaw, readBody, idempotencyOf, orgAnswer, url } = h;
  if (rest[0] !== "connections") return null;
  const bad = (message) => orgAnswer({ status: 400, body: { error: "connection-invalid", message } });

  if (rest.length === 1 && method === "GET") return orgAnswer(await askRaw("connections-list"));

  // The settings and credential of a create or change, checked and sealed
  // here: the store never sees a credential in the clear.
  const prepare = async (name, v, needsCredential) => {
    const args = { name };
    if (v.base !== undefined) {
      const b = normalBase(v.base, env);
      if (b.problem) return { refusal: bad(b.problem) };
      args.base = b.base;
    }
    if (v.allow !== undefined || needsCredential) {
      const a = normalAllow(v.allow);
      if (a.problem) return { refusal: bad(a.problem) };
      args.allow = a.allow;
    }
    if (v.audience !== undefined) args.audience = v.audience;
    if (v.apps !== undefined) args.apps = v.apps;
    const bound = { ...v };
    const stored = await credentialToStore(env, slug, name, v);
    if (stored && stored.problem) return { refusal: bad(stored.problem) };
    if (stored && stored.off) return { refusal: off() };
    if (stored) {
      Object.assign(args, { sealed: stored.blob, kind: stored.kind, header_name: stored.header_name });
      // The Idempotency-Key is bound to the request, and the request holds
      // the credential: bind it to the seam's stand-in for it instead.
      delete bound.header;
      delete bound.basic;
      bound.credential = stored.binding;
    } else if (needsCredential) {
      return { refusal: bad("give its credential: a header and its value, or a user and password") };
    }
    return { args, bound };
  };

  if (rest.length === 1 && method === "POST") {
    if (!credentialsOn(env)) return off();
    const body = await readBody();
    if (body.tooLarge || body.bad) return bad("the body is JSON, under 64 KiB");
    const name = String(body.value.name || "").trim().toLowerCase();
    if (!CONNECTION.test(name)) {
      return bad("a connection's name is 2 to 39 characters: lower-case letters, digits and dashes, starting with a letter");
    }
    if (body.value.base === undefined) return bad("give its address, such as https://api.acme.com/finance");
    const p = await prepare(name, body.value, true);
    if (p.refusal) return p.refusal;
    return orgAnswer(await askRaw("connection-create", p.args, await idempotencyOf(request, url.pathname, JSON.stringify(p.bound))));
  }

  const name = String(rest[1] || "").toLowerCase();
  if (!CONNECTION.test(name)) return orgAnswer({ status: 404, body: { error: "no-such-connection", message: "that is not a connection's name" } });

  if (rest.length === 2 && method === "PUT") {
    const body = await readBody();
    if (body.tooLarge || body.bad) return bad("the body is JSON, under 64 KiB");
    if ((body.value.header || body.value.basic) && !credentialsOn(env)) return off();
    const p = await prepare(name, body.value, false);
    if (p.refusal) return p.refusal;
    return orgAnswer(await askRaw("connection-update", p.args, await idempotencyOf(request, url.pathname, JSON.stringify(p.bound))));
  }
  if (rest.length === 2 && method === "DELETE") {
    return orgAnswer(await askRaw("connection-remove", { name }, await idempotencyOf(request, url.pathname, "")));
  }
  if (rest.length === 3 && rest[2] === "credential" && method === "PUT") {
    if (!credentialsOn(env)) return off();
    const body = await readBody();
    if (body.tooLarge || body.bad) return bad("the body is JSON, under 64 KiB");
    const p = await prepare(name, { header: body.value.header, basic: body.value.basic }, false);
    if (p.refusal) return p.refusal;
    if (!p.args.sealed) return bad("give the new credential: a header and its value, or a user and password");
    return orgAnswer(await askRaw("connection-credential", p.args, await idempotencyOf(request, url.pathname, JSON.stringify(p.bound))));
  }
  if (rest.length === 4 && rest[2] === "credential" && method === "GET") {
    const digest = String(rest[3]).toLowerCase();
    if (!/^[0-9a-f]{64}$/.test(digest)) {
      return orgAnswer({ status: 404, body: { error: "digest-mismatch", message: "a digest is the release's 64-hex sha256" } });
    }
    if (!credentialsOn(env)) return off();
    const decided = await askRaw("connection-use", { name, digest });
    if (decided.status !== 200) return noStore(decided.body, decided.status);
    const d = decided.body;
    const handed = await credentialForDevice(env, slug, name, { blob: d.sealed, kind: d.kind, header_name: d.header_name });
    if (handed === null) {
      return noStore({ error: "connections-off", message: `the ${name} credential does not open with this hub's key -- an owner or admin replaces it in Settings` }, 503);
    }
    return noStore({ name: d.name, base: d.base, allow: d.allow, ...handed });
  }
  return null;
}
