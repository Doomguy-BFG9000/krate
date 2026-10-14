// Company sign-in by OpenID Connect for an organisation (IC-948, the
// contract's Addendum 3 section F and Addendum 4 F1-F3).
//
// An owner gives the organisation an issuer (Google Workspace, Okta,
// Microsoft Entra: each is one issuer URL), a client id and secret, and a
// domain. The domain is proved with a DNS TXT record read through DNS over
// HTTPS. Then a person signs in at that issuer: authorization code with
// PKCE, a `state` held by the hub, an OIDC `nonce` held by the hub, and the
// browser's own K-909 nonce carried through to /login/done. The ID token is
// checked against the issuer's JWKS -- signature, iss, aud, exp, nonce --
// and the person becomes `email:<address>` only when the token says the
// address is verified AND it is at the organisation's verified domain.
//
// Why the domain proof decides everything: an owner chooses the issuer, so
// an owner could run an issuer that says anything about anyone. Without a
// proof that the organisation controls the domain, an issuer it chose could
// claim someone else's address and be handed that person's Krate account.
// So a sign-in at an unproved domain is refused (`domain-not-verified`),
// before any account is found, merged or made.
//
// Two halves live here: the commands the Org Durable Object runs
// (installed when this module is imported, through the store's extendOrg),
// and the Worker's routes, which do everything asynchronous -- encryption,
// DNS, the issuer's endpoints, signature checks -- before the store is
// asked anything.

import { Org, extendOrg, parsePrincipal, ROLES } from "./org.js";

// ------------------------------------------------------------ the store

export const SSO_SCHEMA = [
  `CREATE TABLE IF NOT EXISTS sso (
     id INTEGER PRIMARY KEY CHECK (id = 1),
     issuer TEXT NOT NULL, client_id TEXT NOT NULL, secret TEXT,
     domain TEXT NOT NULL, domain_token TEXT NOT NULL,
     domain_verified_at INTEGER, auto_join_role TEXT, enforce INTEGER NOT NULL DEFAULT 0,
     set_at INTEGER NOT NULL, set_by TEXT NOT NULL)`,
];

/// The roles a sign-in may join with on its own. Never owner or admin:
/// authority over an organisation is given by a person, never by a domain.
export const AUTO_JOIN_ROLES = ["developer", "reviewer", "recipient"];

export const DOMAIN = /^(?=.{4,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$/;

/// Is `principal` an address at `domain`? Exactly that domain: a
/// subdomain, or a name that only ends in the same letters, is not it.
export function atDomain(principal, domain) {
  const p = parsePrincipal(principal);
  if (!p || p.kind !== "email" || !domain) return false;
  return p.value.slice(p.value.lastIndexOf("@") + 1) === String(domain).toLowerCase();
}

function domainToken() {
  const alphabet = "abcdefghjkmnpqrstuvwxyz23456789";
  const bytes = crypto.getRandomValues(new Uint8Array(24));
  return `krate-verify=${[...bytes].map((b) => alphabet[b % alphabet.length]).join("")}`;
}

/// A refusal with a status of its own. Audited like every other refusal
/// (only for a signed-in principal; see Org.deny).
function refuse(org, ctx, action, target, reason, message, status) {
  const answer = org.deny(ctx, action, target, reason, message);
  if (status) answer.status = status;
  return answer;
}

function ssoOps() {
  return {
    sso() {
      return this.row("SELECT * FROM sso WHERE id = 1");
    },

    /// Is SSO enforced for this principal here? Only when the owner asked for
    /// it AND the domain is proved, and only for an address at that domain.
    ssoEnforcedFor(principal) {
      const s = this.sso();
      return Boolean(s && s.enforce && s.domain_verified_at && atDomain(principal, s.domain));
    },

    ssoView() {
      const s = this.sso();
      if (!s) return null;
      // Addendum 4, F1. The secret is never in it: only whether one is held.
      return {
        issuer: s.issuer,
        client_id: s.client_id,
        has_secret: Boolean(s.secret),
        domain: s.domain,
        domain_verified: Boolean(s.domain_verified_at),
        verified_at: s.domain_verified_at || null,
        auto_join_role: s.auto_join_role || null,
        enforce: Boolean(s.enforce),
      };
    },

    ownerOnly(ctx, action, target) {
      const { me, refusal } = this.standing(ctx, action, target);
      if (refusal) return { refusal };
      if (me.role !== "owner") {
        return { refusal: this.deny(ctx, action, target, "role-cannot", `only an owner manages ${ctx.slug}'s company sign-in`) };
      }
      return { me };
    },

    /// GET /orgs/<slug>/sso (owner): null, or the settings without the secret.
    op_sso_get(ctx) {
      const { refusal } = this.ownerOnly(ctx, "sso.view", "sso");
      if (refusal) return refusal;
      return this.ok(this.ssoView());
    },

    /// PUT /orgs/<slug>/sso (owner). The Worker has already encrypted the
    /// secret; it arrives here as ciphertext, or not at all to keep the one
    /// held. A new domain starts unproved, with a new TXT value.
    op_sso_set(ctx) {
      const a = ctx.args;
      const target = `sso:${String(a.domain || "").slice(0, 80)}`;
      const { refusal } = this.ownerOnly(ctx, "sso.set", target);
      if (refusal) return refusal;
      const bad = (message) => ({ status: 400, body: { error: "bad-request", message } });
      const issuer = String(a.issuer || "");
      const clientId = String(a.client_id || "").trim();
      const domain = String(a.domain || "").trim().toLowerCase();
      // With SAML set (org-saml.js) these settings may carry no OIDC issuer:
      // the domain, auto-join and enforce are company sign-in's either way.
      const samlSet = typeof this.samlRow === "function" && Boolean(this.samlRow());
      if (!issuer && !samlSet) return bad("the issuer is the sign-in provider's URL, such as https://accounts.google.com");
      if (issuer && (!clientId || clientId.length > 256)) return bad("the client id is the one your sign-in provider gave Krate");
      if (!DOMAIN.test(domain)) return bad("the domain is your company's email domain, such as acme.com");
      let autoJoin = a.auto_join_role === undefined || a.auto_join_role === null || a.auto_join_role === "" || a.auto_join_role === "none" ? null : String(a.auto_join_role);
      if (autoJoin !== null && !AUTO_JOIN_ROLES.includes(autoJoin)) {
        return refuse(
          this,
          ctx,
          "sso.set",
          target,
          "role-cannot",
          ROLES.includes(autoJoin)
            ? `a sign-in can never join as ${autoJoin} on its own -- an owner invites owners and admins`
            : `there is no role called ${JSON.stringify(autoJoin)}`,
        );
      }
      const enforce = a.enforce === true;
      const before = this.sso();
      const sameDomain = before && before.domain === domain;
      if (enforce) {
        // Chapter 14, 9.2: enforcing SSO must not be able to lock every owner
        // out when the company's sign-in breaks. Someone who signs in another
        // way -- an owner whose principal is not at the domain -- stays able
        // to repair it.
        const owners = this.rows("SELECT principal FROM members WHERE role = 'owner' AND state = 'active'");
        if (!owners.some((o) => !atDomain(o.principal, domain))) {
          return refuse(
            this,
            ctx,
            "sso.set",
            target,
            "sso-needs-break-glass",
            `keep at least one owner who signs in with GitHub or an address outside ${domain}, so a broken company sign-in cannot lock every owner out`,
            409,
          );
        }
      }
      const secret = a.secret === undefined ? (before ? before.secret : null) : a.secret === null ? null : JSON.stringify(a.secret);
      this.sql.exec(
        `INSERT INTO sso (id, issuer, client_id, secret, domain, domain_token, domain_verified_at, auto_join_role, enforce, set_at, set_by)
         VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (id) DO UPDATE SET issuer = excluded.issuer, client_id = excluded.client_id, secret = excluded.secret,
           domain = excluded.domain, domain_token = excluded.domain_token, domain_verified_at = excluded.domain_verified_at,
           auto_join_role = excluded.auto_join_role, enforce = excluded.enforce, set_at = excluded.set_at, set_by = excluded.set_by`,
        issuer,
        clientId,
        secret,
        domain,
        sameDomain ? before.domain_token : domainToken(),
        sameDomain ? before.domain_verified_at : null,
        autoJoin,
        enforce ? 1 : 0,
        ctx.now,
        ctx.actor,
      );
      // Never the secret, nor whether it changed in any way a reader could
      // use: who set company sign-in, for which domain, joining as what.
      this.audit(ctx.actor, before ? "sso.update" : "sso.set", target, "allowed", `auto_join=${autoJoin || "none"} enforce=${enforce ? "on" : "off"}`);
      return this.ok(this.ssoView());
    },

    /// DELETE /orgs/<slug>/sso (owner): company sign-in switched off.
    op_sso_clear(ctx) {
      const { refusal } = this.ownerOnly(ctx, "sso.clear", "sso");
      if (refusal) return refusal;
      if (this.sso()) {
        const s = this.sso();
        this.sql.exec("DELETE FROM sso WHERE id = 1");
        this.audit(ctx.actor, "sso.clear", `sso:${s.domain}`, "allowed");
      }
      return this.ok(null, 204);
    },

    /// GET /orgs/<slug>/sso/domain-token (owner), Addendum 4 F2.
    op_sso_domain_token(ctx) {
      const { refusal } = this.ownerOnly(ctx, "sso.domain", "sso");
      if (refusal) return refusal;
      const s = this.sso();
      if (!s) return { status: 404, body: { error: "sso-not-set-up", message: `${ctx.slug} has no company sign-in yet -- set the issuer, client and domain first` } };
      return this.ok({ name: `_krate-verify.${s.domain}`, value: s.domain_token, domain: s.domain, domain_verified: Boolean(s.domain_verified_at) });
    },

    /// POST /orgs/<slug>/sso/verify-domain, second half: the Worker read DNS
    /// and says what it found for which domain and value. Recorded only if
    /// that is still this organisation's domain and value -- a domain changed
    /// while DNS was being read proves nothing about the new one.
    op_sso_domain_checked(ctx) {
      const s = this.sso();
      const target = `sso:${s ? s.domain : "none"}`;
      const { refusal } = this.ownerOnly(ctx, "sso.verify-domain", target);
      if (refusal) return refusal;
      if (!s) return { status: 404, body: { error: "sso-not-set-up", message: `${ctx.slug} has no company sign-in yet` } };
      const found = ctx.args.found === true && ctx.args.domain === s.domain && ctx.args.value === s.domain_token;
      if (!found) {
        return refuse(
          this,
          ctx,
          "sso.verify-domain",
          target,
          "domain-not-verified",
          ctx.args.problem ||
            `no TXT record at _krate-verify.${s.domain} says ${s.domain_token} yet -- add it at your DNS provider, wait a few minutes, and try again`,
          409,
        );
      }
      if (!s.domain_verified_at) {
        this.sql.exec("UPDATE sso SET domain_verified_at = ? WHERE id = 1", ctx.now);
        this.audit(ctx.actor, "sso.verify-domain", target, "allowed");
      }
      return this.ok({ domain_verified: true, domain: s.domain, verified_at: this.sso().domain_verified_at });
    },

    /// What the Worker's sign-in routes need: the issuer, the client, the
    /// encrypted secret, and the domain's state. Asked by the hub itself, not
    /// by a person -- no route answers this to anyone.
    op_sso_config(ctx) {
      const o = this.org();
      const s = this.sso();
      if (!o || !s) return { status: 404, body: { error: "sso-not-set-up" } };
      return this.ok({
        org: { slug: o.slug, name: o.name },
        issuer: s.issuer,
        client_id: s.client_id,
        secret: s.secret ? JSON.parse(s.secret) : null,
        domain: s.domain,
        domain_verified: Boolean(s.domain_verified_at),
        auto_join_role: s.auto_join_role || null,
      });
    },

    /// A sign-in through this organisation's issuer has finished, for
    /// `principal`, whose ID token the Worker verified. Joins them with the
    /// auto-join role if there is one, the domain is proved and the address
    /// is at it. One transaction, so a removal racing it is in some order:
    /// someone an owner removed is never brought back by signing in.
    op_sso_join(ctx) {
      const o = this.org();
      const s = this.sso();
      const principal = String(ctx.args.principal || "");
      const target = `member:${principal}`;
      if (!o || !s) return this.ok({ joined: false, state: "no-sso" });
      if (!s.domain_verified_at || !atDomain(principal, s.domain)) return this.ok({ joined: false, state: "off-domain" });
      const current = this.member(principal);
      if (current && current.state === "active") return this.ok({ joined: false, state: "member", role: current.role });
      if (current) return this.ok({ joined: false, state: "removed" });
      const role = s.auto_join_role;
      // The settings refuse owner and admin; this refuses them again, so no
      // stored value -- however it got there -- makes a domain an owner.
      if (!role || !AUTO_JOIN_ROLES.includes(role)) return this.ok({ joined: false, state: "no-auto-join" });
      this.sql.exec(
        "INSERT INTO members (principal, role, state, added_at, added_by) VALUES (?, ?, 'active', ?, ?)",
        principal,
        role,
        ctx.now,
        `sso:${o.slug}`,
      );
      this.audit(principal, "sso.join", target, "allowed", role);
      return this.ok({ joined: true, state: "joined", role, org: { slug: o.slug, name: o.name } });
    },
  };
}

/// Teach the Org class company sign-in: its commands (an owner's alone;
/// the contract's role table stays as it is), and the `sso-required` check on every route that starts
/// from a member's standing.
function installSso(OrgClass) {
  // The commands go on as methods (they call each other and the store's
  // own helpers); the tables and which commands take an Idempotency-Key go
  // through the store's extension point.
  Object.assign(OrgClass.prototype, ssoOps());
  extendOrg({ schema: SSO_SCHEMA, mutations: ["sso-set", "sso-clear", "sso-domain-checked"] });

  // Whether this request's session was signed in through THIS
  // organisation's SSO: the Worker passes `sso:<slug>` beside the person's
  // principals (see ssoMarks below). Read per command, before it runs; a
  // command runs synchronously in one transaction, so nothing else can
  // run between this and the command's own checks.
  const run = OrgClass.prototype.run;
  OrgClass.prototype.run = function (command) {
    const raw = Array.isArray(command && command.principals) ? command.principals : [];
    this.ssoSession = raw.includes(`sso:${String((command && command.slug) || "").toLowerCase()}`);
    return run.call(this, command);
  };

  const standing = OrgClass.prototype.standing;
  OrgClass.prototype.standing = function (ctx, action, target) {
    const answer = standing.call(this, ctx, action, target);
    if (answer.me && !this.ssoSession && this.ssoEnforcedFor(answer.me.principal)) {
      const s = this.sso();
      const refusal = refuse(
        this,
        ctx,
        action,
        target,
        "sso-required",
        `${ctx.slug} asks everyone at ${s.domain} to sign in with the company's sign-in -- sign in again at https://krate.tech/login/?org=${ctx.slug}`,
        403,
      );
      refusal.body.org = ctx.slug;
      return { refusal };
    }
    return answer;
  };
}

// Importing this module teaches the store company sign-in.
installSso(Org);

// --------------------------------------------------- sessions signed by SSO

const SESSION_SECONDS = 90 * 24 * 3600;
const SSO_SESSIONS_KEPT = 20;

async function sha256HexOf(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/// The organisations whose SSO signed in this `krs_` session. Kept on the
/// account's own record (read on every session check anyway, so this
/// costs no extra KV read), keyed by a hash of the session token, and only
/// for the session's life. A later email-link sign-in is a different
/// session and carries none of them.
export async function ssoSlugsFor(record, token) {
  const held = record && record.sso_sessions;
  if (!held || typeof held !== "object") return [];
  const entry = held[(await sha256HexOf(`krate-sso-session:${token}`)).slice(0, 32)];
  if (!entry || !entry.org || !(Date.now() / 1000 - Number(entry.at || 0) < SESSION_SECONDS)) return [];
  return [String(entry.org)];
}

/// The markers passed to an organisation's store beside the principals:
/// `sso:<slug>`. The store's principal parser ignores them; only its SSO
/// check reads them.
export function ssoMarks(identity) {
  return Array.isArray(identity && identity.sso) ? identity.sso.map((s) => `sso:${String(s).toLowerCase()}`) : [];
}

export async function markSsoSession(env, userId, token, slug) {
  const key = `user:${userId}`;
  const record = JSON.parse((await env.APPS.get(key)) || "null");
  if (!record) return;
  const now = Math.floor(Date.now() / 1000);
  const held = record.sso_sessions && typeof record.sso_sessions === "object" ? record.sso_sessions : {};
  const live = Object.entries(held)
    .filter(([, e]) => e && now - Number(e.at || 0) < SESSION_SECONDS)
    .sort((x, y) => Number(y[1].at) - Number(x[1].at))
    .slice(0, SSO_SESSIONS_KEPT - 1);
  const id = (await sha256HexOf(`krate-sso-session:${token}`)).slice(0, 32);
  record.sso_sessions = Object.fromEntries([[id, { org: slug, at: now }], ...live]);
  await env.APPS.put(key, JSON.stringify(record));
}

// ------------------------------------------------------- secret at rest

/// The AES-GCM key behind every organisation's client secret, from the
/// Worker secret ORG_SECRET_KEY. Absent, nothing can be stored or used.
async function secretKey(env) {
  const raw = String(env.ORG_SECRET_KEY || "");
  if (!raw) return null;
  const material = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(`krate-org-secret:${raw}`));
  return crypto.subtle.importKey("raw", material, { name: "AES-GCM" }, false, ["encrypt", "decrypt"]);
}

const b64 = (bytes) => btoa(String.fromCharCode(...new Uint8Array(bytes)));
const unb64 = (text) => Uint8Array.from(atob(text), (c) => c.charCodeAt(0));

/// Encrypt one organisation's client secret. The organisation's slug is
/// the additional data, so a ciphertext copied into another organisation's
/// store does not open there.
export async function sealSecret(env, slug, plaintext, purpose = "sso") {
  const key = await secretKey(env);
  if (!key) return null;
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const body = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv, additionalData: new TextEncoder().encode(`krate-org-${purpose}:${slug}`) },
    key,
    new TextEncoder().encode(plaintext),
  );
  return { v: 1, iv: b64(iv), body: b64(body) };
}

export async function openSecret(env, slug, sealed, purpose = "sso") {
  const key = await secretKey(env);
  if (!key || !sealed || !sealed.iv || !sealed.body) return null;
  try {
    const out = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: unb64(sealed.iv), additionalData: new TextEncoder().encode(`krate-org-${purpose}:${slug}`) },
      key,
      unb64(sealed.body),
    );
    return new TextDecoder().decode(out);
  } catch (_) {
    return null;
  }
}

// ------------------------------------------------------------ the routes

/// The hub is this machine's loopback: a local `wrangler dev` proof. Only
/// then may an issuer or the DNS resolver be plain http on this machine.
export function devHub(env) {
  return /^http:\/\/(127\.0\.0\.1|localhost|\[::1\])(:\d+)?\/?$/.test(String(env.PUBLIC_BASE || ""));
}

/// An issuer, or an endpoint it names, that the hub will talk to: https,
/// or http on this machine's loopback when the hub itself is a local proof.
export function reachable(raw, env) {
  let u;
  try {
    u = new URL(String(raw || ""));
  } catch (_) {
    return false;
  }
  if (u.username || u.password || u.hash) return false;
  if (u.protocol === "https:") return true;
  return devHub(env) && u.protocol === "http:" && /^(127\.0\.0\.1|localhost|\[::1\])$/.test(u.hostname);
}

function validIssuer(raw, env) {
  const s = String(raw || "").trim();
  if (!s || s.length > 300 || !reachable(s, env)) return null;
  const u = new URL(s);
  if (u.search) return null;
  return s;
}

/// The DNS-over-HTTPS resolver: Cloudflare's. A local proof may name its
/// own (ORG_DOH_URL), because nothing on this machine is in public DNS.
function dohUrl(env) {
  if (devHub(env) && env.ORG_DOH_URL) return String(env.ORG_DOH_URL);
  return "https://cloudflare-dns.com/dns-query";
}

/// Every TXT string at `name`, read through DNS over HTTPS (JSON form).
/// A record split into several quoted strings is joined, as resolvers do.
export async function txtRecords(env, name) {
  const u = new URL(dohUrl(env));
  u.searchParams.set("name", name);
  u.searchParams.set("type", "TXT");
  const res = await fetch(u.toString(), { headers: { accept: "application/dns-json" } });
  if (!res.ok) throw new Error(`the DNS resolver answered ${res.status}`);
  const answer = await res.json();
  return (answer.Answer || [])
    .filter((a) => a && (a.type === 16 || a.type === undefined) && typeof a.data === "string")
    .map((a) => {
      const pieces = a.data.match(/"((?:[^"\\]|\\.)*)"/g);
      return pieces ? pieces.map((p) => p.slice(1, -1).replace(/\\(.)/g, "$1")).join("") : a.data;
    });
}

/// /orgs/<slug>/sso, /sso/domain-token, /sso/verify-domain. Answers a
/// Response, or null when the path is not one of these.
export async function ssoRoute({ request, env, slug, rest, method, askRaw, readBody, idempotencyOf, path, answer }) {
  if (rest[0] !== "sso") return null;
  const bad = (message) => answer({ status: 400, body: { error: "bad-request", message } });
  if (rest.length === 1 && method === "GET") return answer(await askRaw("sso-get"));
  if (rest.length === 1 && method === "DELETE") {
    return answer(await askRaw("sso-clear", {}, await idempotencyOf(request, path, "")));
  }
  if (rest.length === 1 && method === "PUT") {
    const body = await readBody();
    if (body.tooLarge || body.bad) return bad("the body is JSON, under 64 KiB");
    const v = body.value;
    // Empty only for an organisation that signs in by SAML; the store says.
    const issuer = v.issuer === undefined || v.issuer === null || v.issuer === "" ? "" : validIssuer(v.issuer, env);
    if (issuer === null) return bad("the issuer is an https URL, such as https://accounts.google.com");
    let secret;
    let bound = { ...v };
    if (v.client_secret !== undefined && v.client_secret !== null && v.client_secret !== "") {
      const plain = String(v.client_secret);
      if (plain.length > 1024) return bad("that client secret is too long");
      secret = await sealSecret(env, slug, plain);
      if (!secret) {
        return answer({ status: 503, body: { error: "sso-off", message: "company sign-in is not switched on on this hub (it has no ORG_SECRET_KEY)" } });
      }
      // The Idempotency-Key is bound to the request, and the request holds
      // the secret: bind it to a keyed digest of the secret instead, so the
      // store never holds anything derived from it by a public function.
      bound.client_secret = await keyedDigest(env, plain);
    }
    const idempotency = await idempotencyOf(request, path, JSON.stringify(bound));
    return answer(
      await askRaw(
        "sso-set",
        {
          issuer,
          client_id: v.client_id,
          secret,
          domain: v.domain,
          auto_join_role: v.auto_join_role,
          enforce: v.enforce === true,
        },
        idempotency,
      ),
    );
  }
  if (rest.length === 2 && rest[1] === "domain-token" && method === "GET") return answer(await askRaw("sso-domain-token"));
  if (rest.length === 2 && rest[1] === "verify-domain" && method === "POST") {
    // Who may, and what to look for, first: a stranger makes no DNS query.
    const token = await askRaw("sso-domain-token");
    if (token.status !== 200) return answer(token);
    const { name, value, domain } = token.body;
    let found = false;
    let problem;
    try {
      found = (await txtRecords(env, name)).includes(value);
    } catch (err) {
      problem = `the DNS lookup for ${name} did not finish (${err.message}) -- try again in a minute`;
    }
    const idempotency = await idempotencyOf(request, path, `${domain}\n${value}\n${found}`);
    return answer(await askRaw("sso-domain-checked", { found, domain, value, problem }, idempotency));
  }
  return null;
}

async function keyedDigest(env, text) {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(`krate-org-idem:${env.ORG_SECRET_KEY || ""}`),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  return b64(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(text)));
}

// ------------------------------------------------------------- sign-in

const OIDC_STATE_SECONDS = 600;
const CLOCK_SKEW_SECONDS = 60;

const b64url = (bytes) => b64(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
function unb64url(text) {
  const s = String(text).replace(/-/g, "+").replace(/_/g, "/");
  return unb64(s + "=".repeat((4 - (s.length % 4)) % 4));
}
const randomB64url = (n) => b64url(crypto.getRandomValues(new Uint8Array(n)));

/// A sign-in that did not finish: back to the sign-in page for that
/// organisation, with the stable reason (Addendum 4, F3). Tagged so the
/// sign-in counts record it as the failure it is.
export function failed(env, slug, reason) {
  const site = String(env.SITE_BASE || "https://krate.tech").replace(/\/$/, "");
  const q = new URLSearchParams();
  if (slug) q.set("org", slug);
  q.set("error", reason);
  return new Response(null, { status: 302, headers: { location: `${site}/login/?${q.toString()}`, "x-krate-signin": reason } });
}

async function getJson(url, init) {
  const res = await fetch(url, init);
  if (!res.ok) throw new Error(`${new URL(url).host} answered ${res.status}`);
  return res.json();
}

/// The issuer's own description of itself. Its `issuer` must be exactly
/// the one the owner configured, and every endpoint it names must be one
/// the hub may reach.
export async function discover(env, issuer) {
  const doc = await getJson(`${issuer.replace(/\/$/, "")}/.well-known/openid-configuration`, {
    headers: { accept: "application/json" },
  });
  if (doc.issuer !== issuer) throw new Error("the issuer's discovery document names a different issuer");
  for (const k of ["authorization_endpoint", "token_endpoint", "jwks_uri"]) {
    if (!reachable(doc[k], env)) throw new Error(`the issuer's ${k} is not an https URL`);
  }
  return doc;
}

/// GET /login/oidc/start?org=<slug>&nonce=<K-909 nonce>[&from=app&app_nonce=]
export async function oidcStart(url, env, deps) {
  const slug = String(url.searchParams.get("org") || "").toLowerCase();
  if (!deps.SLUG.test(slug)) return failed(env, "", "sso-not-set-up");
  const start = deps.signInStart(url.searchParams.get("from"), url.searchParams.get("nonce"), url.searchParams.get("app_nonce"));
  // A new door, so no page from before K-909 needs it to be lenient: the
  // browser's nonce is required, and it is what /login/done checks.
  if (!start.nonce && !start.app_nonce) return failed(env, slug, "sso-failed");
  const config = await deps.orgAsk(env, slug, { op: "sso-config", principals: [] });
  if (config.status !== 200) return failed(env, slug, "sso-not-set-up");
  const c = config.body;
  // An organisation that signs in by SAML (org-saml.js) is sent there, so
  // https://krate.tech/login/?org=<slug> is the one door for both.
  if (c.saml) return Response.redirect(`${String(env.PUBLIC_BASE).replace(/\/$/, "")}/login/saml/start${url.search}`, 302);
  if (!c.issuer) return failed(env, slug, "sso-not-set-up");
  let doc;
  try {
    doc = await discover(env, c.issuer);
  } catch (err) {
    console.error(`sso start for ${slug}: ${err.message}`);
    return failed(env, slug, "sso-failed");
  }
  const state = randomB64url(32);
  const verifier = randomB64url(32);
  const challenge = b64url(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)));
  const nonce = randomB64url(32);
  await env.APPS.put(
    `oidc:${state}`,
    JSON.stringify({
      ...start,
      org: slug,
      issuer: c.issuer,
      client_id: c.client_id,
      verifier,
      oidc_nonce: nonce,
      token_endpoint: doc.token_endpoint,
      jwks_uri: doc.jwks_uri,
      auth_methods: Array.isArray(doc.token_endpoint_auth_methods_supported) ? doc.token_endpoint_auth_methods_supported : null,
    }),
    { expirationTtl: OIDC_STATE_SECONDS },
  );
  const auth = new URL(doc.authorization_endpoint);
  auth.searchParams.set("client_id", c.client_id);
  auth.searchParams.set("redirect_uri", `${String(env.PUBLIC_BASE).replace(/\/$/, "")}/login/oidc/callback`);
  auth.searchParams.set("response_type", "code");
  auth.searchParams.set("scope", "openid email profile");
  auth.searchParams.set("state", state);
  auth.searchParams.set("nonce", nonce);
  auth.searchParams.set("code_challenge", challenge);
  auth.searchParams.set("code_challenge_method", "S256");
  return Response.redirect(auth.toString(), 302);
}

/// Why an ID token is not good enough, or null when it is. Checks the
/// signature against the issuer's keys (RS256 or ES256 only: never `none`,
/// never a shared-secret algorithm), and then iss, aud (and azp when
/// there are several audiences), exp, nbf, iat and nonce. Answers
/// `{ claims }` or `{ problem }`.
export async function verifyIdToken(jwt, { jwks, issuer, clientId, nonce, now = Math.floor(Date.now() / 1000) }) {
  const parts = String(jwt || "").split(".");
  if (parts.length !== 3) return { problem: "not-a-jwt" };
  let header;
  let claims;
  try {
    header = JSON.parse(new TextDecoder().decode(unb64url(parts[0])));
    claims = JSON.parse(new TextDecoder().decode(unb64url(parts[1])));
  } catch (_) {
    return { problem: "not-a-jwt" };
  }
  const algs = {
    RS256: { kty: "RSA", import: { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, verify: { name: "RSASSA-PKCS1-v1_5" } },
    ES256: { kty: "EC", import: { name: "ECDSA", namedCurve: "P-256" }, verify: { name: "ECDSA", hash: "SHA-256" } },
  };
  const alg = algs[header && header.alg];
  if (!alg) return { problem: "alg-not-allowed" };
  const keys = (jwks && Array.isArray(jwks.keys) ? jwks.keys : []).filter(
    (k) => k && k.kty === alg.kty && (!k.use || k.use === "sig") && (!k.alg || k.alg === header.alg),
  );
  const candidates = header.kid ? keys.filter((k) => k.kid === header.kid) : keys.length === 1 ? keys : [];
  if (candidates.length !== 1) return { problem: "no-matching-key" };
  const jwk = candidates[0];
  let key;
  try {
    const material = alg.kty === "RSA" ? { kty: "RSA", n: jwk.n, e: jwk.e } : { kty: "EC", crv: "P-256", x: jwk.x, y: jwk.y };
    key = await crypto.subtle.importKey("jwk", material, alg.import, false, ["verify"]);
  } catch (_) {
    return { problem: "bad-key" };
  }
  let signature;
  try {
    signature = unb64url(parts[2]);
  } catch (_) {
    return { problem: "bad-signature" };
  }
  const signed = new TextEncoder().encode(`${parts[0]}.${parts[1]}`);
  if (!(await crypto.subtle.verify(alg.verify, key, signature, signed))) return { problem: "bad-signature" };
  if (!claims || typeof claims !== "object") return { problem: "not-a-jwt" };
  if (claims.iss !== issuer) return { problem: "wrong-issuer" };
  const aud = Array.isArray(claims.aud) ? claims.aud : [claims.aud];
  if (!aud.includes(clientId)) return { problem: "wrong-audience" };
  if (aud.length > 1 && claims.azp !== clientId) return { problem: "wrong-audience" };
  if (claims.azp !== undefined && claims.azp !== clientId) return { problem: "wrong-audience" };
  if (!Number.isFinite(claims.exp) || now > claims.exp + CLOCK_SKEW_SECONDS) return { problem: "expired" };
  if (claims.nbf !== undefined && (!Number.isFinite(claims.nbf) || claims.nbf > now + CLOCK_SKEW_SECONDS)) return { problem: "not-yet-valid" };
  if (!Number.isFinite(claims.iat) || claims.iat > now + CLOCK_SKEW_SECONDS) return { problem: "issued-in-the-future" };
  if (typeof nonce !== "string" || !nonce || claims.nonce !== nonce) return { problem: "wrong-nonce" };
  if (typeof claims.sub !== "string" || !claims.sub) return { problem: "no-subject" };
  return { claims };
}

/// GET /login/oidc/callback?code&state
export async function oidcCallback(url, env, deps) {
  const state = String(url.searchParams.get("state") || "");
  const raw = state ? await env.APPS.get(`oidc:${state}`) : null;
  if (!raw) return failed(env, String(url.searchParams.get("org") || "").toLowerCase(), "sso-failed");
  // One use: a replayed callback finds nothing.
  await env.APPS.delete(`oidc:${state}`);
  const record = JSON.parse(raw);
  const slug = record.org;
  if (url.searchParams.get("error")) return failed(env, slug, "sso-failed");
  const stop = (reason, why) => {
    // Why, for us; never a token, a code or a secret.
    if (why) console.error(`sso sign-in for ${slug}: ${why}`);
    return failed(env, slug, reason);
  };
  const config = await deps.orgAsk(env, slug, { op: "sso-config", principals: [] });
  if (config.status !== 200) return stop("sso-not-set-up");
  const c = config.body;
  // The settings changed while the person was at the issuer: start again.
  if (c.issuer !== record.issuer || c.client_id !== record.client_id) return stop("sso-failed", "settings changed mid sign-in");

  let secret = null;
  if (c.secret) {
    secret = await openSecret(env, slug, c.secret);
    if (secret === null) return stop("sso-failed", "the stored client secret does not open with ORG_SECRET_KEY");
  }
  const redirect = `${String(env.PUBLIC_BASE).replace(/\/$/, "")}/login/oidc/callback`;
  const form = new URLSearchParams({
    grant_type: "authorization_code",
    code: String(url.searchParams.get("code") || ""),
    redirect_uri: redirect,
    code_verifier: record.verifier,
    client_id: c.client_id,
  });
  const headers = { "content-type": "application/x-www-form-urlencoded", accept: "application/json" };
  if (secret !== null) {
    // client_secret_post unless the issuer says it only takes basic.
    const methods = record.auth_methods;
    if (methods && !methods.includes("client_secret_post") && methods.includes("client_secret_basic")) {
      const enc = (s) => encodeURIComponent(s);
      headers.authorization = `Basic ${btoa(`${enc(c.client_id)}:${enc(secret)}`)}`;
    } else {
      form.set("client_secret", secret);
    }
  }
  let tokens;
  let jwks;
  try {
    tokens = await getJson(record.token_endpoint, { method: "POST", headers, body: form });
    jwks = await getJson(record.jwks_uri, { headers: { accept: "application/json" } });
  } catch (err) {
    return stop("sso-failed", err.message);
  }
  const checked = await verifyIdToken(tokens && tokens.id_token, {
    jwks,
    issuer: c.issuer,
    clientId: c.client_id,
    nonce: record.oidc_nonce,
  });
  if (checked.problem) return stop("sso-failed", `ID token refused: ${checked.problem}`);
  const claims = checked.claims;

  // The proof the organisation controls the domain comes before anything
  // the issuer says about an address (see the top of this file).
  if (!c.domain_verified) return stop("domain-not-verified");
  if (claims.email_verified !== true) return stop("sso-unverified-email");
  const email = String(claims.email || "").toLowerCase();
  const principal = `email:${email}`;
  if (!parsePrincipal(principal) || !atDomain(principal, c.domain)) return stop("sso-wrong-domain");

  // The account: whoever already proved this inbox, or a new one. A
  // sign-in at one issuer is one provider, whatever the subject says.
  const provider = `oidc-${(await sha256HexOf(c.issuer)).slice(0, 16)}`;
  const user = await deps.ensureUser(env, provider, claims.sub, {
    email,
    verified_email: email,
    name: typeof claims.name === "string" ? claims.name.slice(0, 120) : "",
    avatar_url: "",
  });
  const token = await deps.newSession(env, user.id);
  await markSsoSession(env, user.id, token, slug);

  const join = await deps.orgAsk(env, slug, { op: "sso-join", principals: [principal, `sso:${slug}`], args: { principal } });
  const joined = join.status === 200 ? join.body : { state: "unknown" };
  if (joined.joined && deps.noteOrgOf) await deps.noteOrgOf(env, principal, slug, c.org.name, joined.role);

  // Delivered exactly as every other sign-in is (K-909: the browser's
  // nonce rides back beside the session), plus which organisation and
  // what happened there.
  const delivered = deps.deliver(record, user, token);
  const location = `${delivered.headers.get("location")}&org=${encodeURIComponent(slug)}&sso=${encodeURIComponent(joined.state || "unknown")}`;
  return new Response(null, { status: 302, headers: { location } });
}
