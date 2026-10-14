// Company sign-in by SAML 2.0 for an organisation (IC-955), beside the
// OIDC sign-in in org-sso.js and sharing its rules.
//
// SP-initiated: https://krate.tech/login/?org=<slug> goes to the hub's
// /login/oidc/start, which sends an organisation set up for SAML to
// /login/saml/start. That sends the browser to the company's identity
// provider with an AuthnRequest (HTTP-Redirect binding); the provider posts
// a signed SAML Response back to /login/saml/acs/<slug> (HTTP-POST binding).
//
// What a Response must be before anyone is signed in, every one checked:
//
// - its assertion signed by a certificate the owner gave (an unsigned
//   assertion, a signature by any other key, and any change to the signed
//   bytes are refused), and only one assertion in it;
// - issued by the identity provider the owner named;
// - for this organisation: its Audience is this organisation's entity id,
//   and its bearer confirmation's Recipient is this organisation's ACS URL;
// - in time: NotBefore and NotOnOrAfter, with a minute's clock skew, and a
//   confirmation NotOnOrAfter must be there at all;
// - an answer to a request this hub made in the last ten minutes
//   (InResponseTo), and with an assertion ID never used before (an assertion
//   is good once; a replay within its life is refused).
//
// The signature and the XML canonicalisation behind it are not written
// here: they are @node-saml/node-saml 5.1.0 over xml-crypto, the
// maintained Node SAML stack (passport-saml's core), which runs on Workers
// with the nodejs_compat flag. This module adds the checks it leaves to its
// caller (issuer, recipient, one use, the address).
//
// Then the domain rules are company sign-in's, unchanged: nothing works
// until the organisation has proved its domain with the DNS record
// (org-sso.js), the person is `email:<address>` only for an address at
// that domain, a sign-in joins with the auto-join role when there is one,
// someone removed is never brought back, and the session is marked as
// this organisation's company sign-in, so `enforce` counts it.

import { SAML, ValidateInResponseTo } from "@node-saml/node-saml";
import { X509Certificate, createHash } from "node:crypto";
import { Org, extendOrg, parsePrincipal, sha256HexSync } from "./org.js";
import { atDomain, DOMAIN, failed, markSsoSession, reachable } from "./org-sso.js";

const REQUEST_SECONDS = 600;
const CLOCK_SKEW_MS = 60_000;
const SEEN_KEPT_SECONDS = 24 * 60 * 60;
const CERTS_MAX = 2;

const SCHEMA = [
  `CREATE TABLE IF NOT EXISTS saml (
     id INTEGER PRIMARY KEY CHECK (id = 1),
     idp_entity_id TEXT NOT NULL, idp_sso_url TEXT NOT NULL, idp_certs TEXT NOT NULL,
     set_at INTEGER NOT NULL, set_by TEXT NOT NULL)`,
  `CREATE TABLE IF NOT EXISTS saml_seen (assertion_id TEXT PRIMARY KEY, until INTEGER NOT NULL)`,
];

function domainToken() {
  const alphabet = "abcdefghjkmnpqrstuvwxyz23456789";
  return `krate-verify=${[...crypto.getRandomValues(new Uint8Array(24))].map((b) => alphabet[b % alphabet.length]).join("")}`;
}

const ops = {
  samlRow() {
    return this.row("SELECT * FROM saml WHERE id = 1");
  },

  samlView(ctx) {
    const s = this.samlRow();
    const sso = this.row("SELECT domain, domain_verified_at FROM sso WHERE id = 1");
    return {
      sp: { entity_id: ctx.args.sp_entity || null, acs_url: ctx.args.acs || null, metadata_url: ctx.args.sp_entity || null },
      domain: sso ? sso.domain : null,
      domain_verified: Boolean(sso && sso.domain_verified_at),
      idp: s
        ? {
            entity_id: s.idp_entity_id,
            sso_url: s.idp_sso_url,
            certificates: JSON.parse(s.idp_certs).map((c) => ({ sha256: c.sha256, subject: c.subject, not_after: c.not_after })),
            set_at: s.set_at,
            set_by: s.set_by,
          }
        : null,
    };
  },

  samlOwner(ctx, action, target) {
    const { me, refusal } = this.standing(ctx, action, target);
    if (refusal) return { refusal };
    if (me.role !== "owner") return { refusal: this.deny(ctx, action, target, "role-cannot", `only an owner manages ${ctx.slug}'s company sign-in`) };
    return { me };
  },

  /// GET /orgs/<slug>/saml (owner).
  "saml-get"(ctx) {
    const { refusal } = this.samlOwner(ctx, "saml.view", "saml");
    if (refusal) return refusal;
    return this.ok(this.samlView(ctx));
  },

  /// PUT /orgs/<slug>/saml (owner): the identity provider's entity id,
  /// sign-in URL and signing certificates (checked and fingerprinted by the
  /// Worker), and the company domain if the organisation has none yet. The
  /// domain is company sign-in's one domain: proved once, by the same DNS
  /// record, whichever protocol signs people in.
  "saml-set"(ctx) {
    const a = ctx.args;
    const { me, refusal } = this.samlOwner(ctx, "saml.set", "saml");
    if (refusal) return refusal;
    const bad = (message) => ({ status: 400, body: { error: "bad-request", message } });
    const entity = String(a.idp_entity_id || "").trim();
    if (!entity || entity.length > 500) return bad("the identity provider's entity id (Issuer) is required -- copy it from its SAML set-up page");
    if (!a.idp_sso_url) return bad("the identity provider's sign-in URL (SSO URL) is required");
    if (!Array.isArray(a.certs) || !a.certs.length) return bad("paste the identity provider's signing certificate");
    let sso = this.row("SELECT * FROM sso WHERE id = 1");
    const domain = a.domain === undefined || a.domain === null || a.domain === "" ? null : String(a.domain).trim().toLowerCase();
    if (sso && domain && domain !== sso.domain) {
      return bad(`${ctx.slug}'s company domain is ${sso.domain} -- change it under company sign-in first, then prove the new one`);
    }
    if (!sso) {
      if (!domain || !DOMAIN.test(domain)) return bad("give your company's email domain, such as acme.com -- you prove it with a DNS record next");
      // Company sign-in's settings, with no OIDC issuer: the domain alone.
      this.sql.exec(
        `INSERT INTO sso (id, issuer, client_id, secret, domain, domain_token, domain_verified_at, auto_join_role, enforce, set_at, set_by)
         VALUES (1, '', '', NULL, ?, ?, NULL, NULL, 0, ?, ?)`,
        domain,
        domainToken(),
        ctx.now,
        me.principal,
      );
      this.audit(me.principal, "sso.set", `sso:${domain}`, "allowed", "domain only, for SAML");
      sso = this.row("SELECT * FROM sso WHERE id = 1");
    }
    const before = this.samlRow();
    this.sql.exec(
      `INSERT INTO saml (id, idp_entity_id, idp_sso_url, idp_certs, set_at, set_by) VALUES (1, ?, ?, ?, ?, ?)
       ON CONFLICT (id) DO UPDATE SET idp_entity_id = excluded.idp_entity_id, idp_sso_url = excluded.idp_sso_url,
         idp_certs = excluded.idp_certs, set_at = excluded.set_at, set_by = excluded.set_by`,
      entity,
      String(a.idp_sso_url),
      JSON.stringify(a.certs.slice(0, CERTS_MAX)),
      ctx.now,
      me.principal,
    );
    this.audit(me.principal, before ? "saml.update" : "saml.set", `saml:${sso.domain}`, "allowed", a.certs.map((c) => String(c.sha256).slice(0, 16)).join(","));
    return this.ok(this.samlView(ctx));
  },

  /// DELETE /orgs/<slug>/saml (owner). The domain and its proof stay.
  "saml-clear"(ctx) {
    const { me, refusal } = this.samlOwner(ctx, "saml.clear", "saml");
    if (refusal) return refusal;
    if (this.samlRow()) {
      this.sql.exec("DELETE FROM saml WHERE id = 1");
      this.audit(me.principal, "saml.clear", "saml", "allowed");
    }
    return this.ok(null, 204);
  },

  /// What the sign-in routes need. Asked by the hub itself, never answered
  /// to a person.
  "saml-config"(ctx) {
    const o = this.org();
    const s = this.samlRow();
    const sso = this.row("SELECT * FROM sso WHERE id = 1");
    if (!o || !s || !sso) return { status: 404, body: { error: "sso-not-set-up" } };
    return this.ok({
      org: { slug: o.slug, name: o.name },
      idp_entity_id: s.idp_entity_id,
      idp_sso_url: s.idp_sso_url,
      certs: JSON.parse(s.idp_certs).map((c) => c.pem),
      domain: sso.domain,
      domain_verified: Boolean(sso.domain_verified_at),
    });
  },

  /// One use of an assertion: refused the second time, for as long as the
  /// assertion could still be presented (and a day at least).
  "saml-consume"(ctx) {
    const id = String(ctx.args.assertion_id || "");
    if (!id || id.length > 256) return { status: 400, body: { error: "sso-failed", message: "no assertion id" } };
    this.sql.exec("DELETE FROM saml_seen WHERE until < ?", ctx.now);
    const key = sha256HexSync(`saml-assertion:${id}`);
    if (this.row("SELECT assertion_id FROM saml_seen WHERE assertion_id = ?", key)) {
      return { status: 409, body: { error: "sso-failed", message: "that sign-in was already used" } };
    }
    const until = Math.max(Number(ctx.args.until) || 0, ctx.now + SEEN_KEPT_SECONDS);
    this.sql.exec("INSERT INTO saml_seen (assertion_id, until) VALUES (?, ?)", key, until);
    return this.ok({ used: true });
  },
};

function install(OrgClass) {
  const helpers = {};
  const commands = {};
  for (const [k, fn] of Object.entries(ops)) (k.startsWith("saml-") ? commands : helpers)[k] = fn;
  Object.assign(OrgClass.prototype, helpers);
  extendOrg({ schema: SCHEMA, ops: commands, mutations: ["saml-clear"] });
  // Company sign-in's config says whether SAML is set, so /login/oidc/start
  // can send the browser to the right door.
  const config = OrgClass.prototype.op_sso_config;
  OrgClass.prototype.op_sso_config = function (ctx) {
    const answer = config.call(this, ctx);
    if (answer.status === 200) answer.body.saml = Boolean(this.samlRow());
    return answer;
  };
}

install(Org);

// ------------------------------------------------------------------ helpers

const base = (env) => String(env.PUBLIC_BASE || "").replace(/\/$/, "");
export const spEntityOf = (env, slug) => `${base(env)}/saml/${slug}`;
export const acsOf = (env, slug) => `${base(env)}/login/saml/acs/${slug}`;

/// A signing certificate as pasted: PEM, or the bare base64 an identity
/// provider's page shows. Answers `{pem, sha256, subject, not_after}` or
/// `{problem}`.
export function readCert(raw) {
  let text = String(raw || "").trim();
  if (!text) return { problem: "paste the identity provider's signing certificate" };
  if (!text.includes("-----BEGIN")) {
    const body = text.replace(/\s+/g, "");
    if (!/^[A-Za-z0-9+/=]+$/.test(body)) return { problem: "that is not a certificate -- paste the PEM text, starting -----BEGIN CERTIFICATE-----" };
    text = `-----BEGIN CERTIFICATE-----\n${body.match(/.{1,64}/g).join("\n")}\n-----END CERTIFICATE-----`;
  }
  try {
    const cert = new X509Certificate(text);
    return {
      pem: cert.toString(),
      sha256: createHash("sha256").update(cert.raw).digest("hex"),
      subject: String(cert.subject || "").slice(0, 200),
      not_after: Math.floor(Date.parse(cert.validTo) / 1000) || null,
    };
  } catch (_) {
    return { problem: "that certificate could not be read -- paste the PEM text, starting -----BEGIN CERTIFICATE-----" };
  }
}

/// AuthnRequest IDs the hub issued, kept in KV for ten minutes: what
/// node-saml asks when it checks a Response's InResponseTo, and removes once
/// it matched, so one request is answered once.
function kvCache(env) {
  const key = (k) => `saml-req:${k}`;
  return {
    async saveAsync(k, value) {
      await env.APPS.put(key(k), String(value), { expirationTtl: REQUEST_SECONDS });
      return { value, createdAt: Date.now() };
    },
    async getAsync(k) {
      return (await env.APPS.get(key(k))) ?? null;
    },
    async removeAsync(k) {
      if (k === null || k === undefined) return null;
      const v = await env.APPS.get(key(k));
      await env.APPS.delete(key(k));
      return v ?? null;
    },
  };
}

function samlFor(env, slug, c) {
  return new SAML({
    callbackUrl: acsOf(env, slug),
    entryPoint: c.idp_sso_url,
    issuer: spEntityOf(env, slug),
    audience: spEntityOf(env, slug),
    idpCert: c.certs,
    wantAssertionsSigned: true,
    wantAuthnResponseSigned: false,
    validateInResponseTo: ValidateInResponseTo.always,
    requestIdExpirationPeriodMs: REQUEST_SECONDS * 1000,
    acceptedClockSkewMs: CLOCK_SKEW_MS,
    cacheProvider: kvCache(env),
    // Let the identity provider choose; the address is read below.
    identifierFormat: null,
    disableRequestedAuthnContext: true,
    signatureAlgorithm: "sha256",
    digestAlgorithm: "sha256",
  });
}

/// A short, secret-free word for why a Response was refused: the
/// `x-krate-saml-problem` header, for whoever debugs a set-up.
function problemOf(err) {
  const m = String((err && err.message) || err);
  if (/audience/i.test(m)) return "wrong-audience";
  if (/signature|multiple assertions/i.test(m)) return "bad-signature";
  if (/subject confirmation|NotOnOrAfter|NotBefore|expired|too early|not yet valid/i.test(m)) return "expired";
  if (/InResponseTo/i.test(m)) return "unknown-request";
  if (/status/i.test(m)) return "idp-refused";
  return "unreadable";
}

const EMAIL_ATTRS = [
  "email",
  "mail",
  "emailAddress",
  "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/emailaddress",
  "urn:oid:0.9.2342.19200300.100.1.3",
];

/// The address a validated profile names: an emailAddress-format NameID,
/// else an email attribute, else a NameID that is an address.
export function emailOf(profile) {
  const ok = (v) => {
    const s = String(Array.isArray(v) ? v[0] : v || "").trim().toLowerCase();
    return parsePrincipal(`email:${s}`) ? s : null;
  };
  if (/emailAddress$/.test(String(profile.nameIDFormat || "")) && ok(profile.nameID)) return ok(profile.nameID);
  const attrs = profile.attributes || {};
  for (const a of EMAIL_ATTRS) {
    const v = ok(attrs[a] ?? profile[a]);
    if (v) return v;
  }
  return ok(profile.nameID);
}

/// The signed assertion's own fields node-saml leaves to us: its ID, the
/// bearer confirmations' Recipients and NotOnOrAfter. From the assertion it
/// verified (profile.getAssertion()), never from the unsigned envelope.
function signedFields(profile) {
  const doc = profile.getAssertion ? profile.getAssertion() : null;
  const a = doc && doc.Assertion;
  if (!a || !a.$) return null;
  const confirmations = ((a.Subject && a.Subject[0] && a.Subject[0].SubjectConfirmation) || [])
    .filter((s) => s && s.$ && s.$.Method === "urn:oasis:names:tc:SAML:2.0:cm:bearer")
    .map((s) => (s.SubjectConfirmationData && s.SubjectConfirmationData[0] && s.SubjectConfirmationData[0].$) || {});
  return { id: a.$.ID || null, confirmations };
}

// ------------------------------------------------------------------- routes

/// /orgs/<slug>/saml: the owner's card.
export async function orgSamlRoute(h) {
  const { rest, method, env, slug } = h;
  if (rest[0] !== "saml" || rest.length !== 1) return null;
  const sp = { sp_entity: spEntityOf(env, slug), acs: acsOf(env, slug) };
  if (method === "GET") return h.orgAnswer(await h.askRaw("saml-get", sp));
  if (method === "DELETE") return h.orgAnswer(await h.mutateRaw("saml-clear", () => sp));
  if (method === "PUT") {
    const body = await h.readBody();
    if (body.bad || body.tooLarge) return h.json({ error: "bad-request", message: "the body is JSON, under 64 KiB" }, 400);
    const v = body.value;
    if (!reachable(v.idp_sso_url, env)) return h.json({ error: "bad-request", message: "the identity provider's sign-in URL is an https URL" }, 400);
    const raw = Array.isArray(v.idp_certs) ? v.idp_certs : [v.idp_cert];
    const certs = [];
    for (const r of raw.slice(0, CERTS_MAX)) {
      const c = readCert(r);
      if (c.problem) return h.json({ error: "bad-request", message: c.problem }, 400);
      certs.push(c);
    }
    return h.orgAnswer(await h.askRaw("saml-set", { ...sp, idp_entity_id: v.idp_entity_id, idp_sso_url: v.idp_sso_url, certs, domain: v.domain }));
  }
  return null;
}

/// GET /saml/<slug>: the service provider's metadata, for the identity
/// provider's set-up page. Public, like any SP's metadata: entity id and
/// ACS URL, nothing secret.
export async function samlMetadata(url, env, deps) {
  const slug = String(url.pathname.split("/")[2] || "").toLowerCase();
  if (!deps.SLUG.test(slug)) return new Response("not found", { status: 404 });
  const xml =
    `<?xml version="1.0"?>\n<md:EntityDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata" entityID="${spEntityOf(env, slug)}">` +
    `<md:SPSSODescriptor AuthnRequestsSigned="false" WantAssertionsSigned="true" protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol">` +
    `<md:NameIDFormat>urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress</md:NameIDFormat>` +
    `<md:AssertionConsumerService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" Location="${acsOf(env, slug)}" index="1"/>` +
    `</md:SPSSODescriptor></md:EntityDescriptor>\n`;
  return new Response(xml, { headers: { "content-type": "application/samlmetadata+xml", "cache-control": "public, max-age=300" } });
}

/// GET /login/saml/start?org=<slug>&nonce=<K-909 nonce>[&from=app&app_nonce=]
export async function samlStart(url, env, deps) {
  const slug = String(url.searchParams.get("org") || "").toLowerCase();
  if (!deps.SLUG.test(slug)) return failed(env, "", "sso-not-set-up");
  const start = deps.signInStart(url.searchParams.get("from"), url.searchParams.get("nonce"), url.searchParams.get("app_nonce"));
  if (!start.nonce && !start.app_nonce) return failed(env, slug, "sso-failed");
  const config = await deps.orgAsk(env, slug, { op: "saml-config", principals: [] });
  if (config.status !== 200) return failed(env, slug, "sso-not-set-up");
  if (!config.body.domain_verified) return failed(env, slug, "domain-not-verified");
  const state = [...crypto.getRandomValues(new Uint8Array(24))].map((b) => b.toString(16).padStart(2, "0")).join("");
  await env.APPS.put(`saml:${state}`, JSON.stringify({ ...start, org: slug }), { expirationTtl: REQUEST_SECONDS });
  const location = await samlFor(env, slug, config.body).getAuthorizeUrlAsync(state, undefined, {});
  return Response.redirect(location, 302);
}

/// POST /login/saml/acs/<slug>: the identity provider's signed Response.
export async function samlAcs(request, url, env, deps) {
  const slug = String(url.pathname.split("/")[4] || "").toLowerCase();
  if (!deps.SLUG.test(slug)) return failed(env, "", "sso-failed");
  const stop = (reason, problem, why) => {
    if (why) console.error(`saml sign-in for ${slug}: ${why}`);
    const res = failed(env, slug, reason);
    if (problem) res.headers.set("x-krate-saml-problem", problem);
    return res;
  };
  const read = await deps.readBounded(request, 512 * 1024);
  if (read.tooLarge) return stop("sso-failed", "too-large");
  const form = new URLSearchParams(new TextDecoder().decode(read.body));
  const samlResponse = form.get("SAMLResponse");
  const relay = String(form.get("RelayState") || "");
  if (!samlResponse || !/^[0-9a-f]{48}$/.test(relay)) return stop("sso-failed", "no-request");
  // The browser's sign-in, one use: a replayed post finds nothing.
  const raw = await env.APPS.get(`saml:${relay}`);
  if (!raw) return stop("sso-failed", "unknown-request");
  await env.APPS.delete(`saml:${relay}`);
  const record = JSON.parse(raw);
  if (record.org !== slug) return stop("sso-failed", "wrong-organisation");

  const config = await deps.orgAsk(env, slug, { op: "saml-config", principals: [] });
  if (config.status !== 200) return stop("sso-not-set-up");
  const c = config.body;
  let profile;
  try {
    const result = await samlFor(env, slug, c).validatePostResponseAsync({ SAMLResponse: samlResponse });
    profile = result && result.profile;
  } catch (err) {
    return stop("sso-failed", problemOf(err), `refused: ${String((err && err.message) || err).slice(0, 200)}`);
  }
  if (!profile) return stop("sso-failed", "no-assertion");
  if (profile.issuer !== c.idp_entity_id) return stop("sso-failed", "wrong-issuer", "the assertion's issuer is not the configured identity provider");
  const signed = signedFields(profile);
  if (!signed || !signed.id) return stop("sso-failed", "unreadable");
  const acs = acsOf(env, slug);
  const bearer = signed.confirmations.filter((d) => d.Recipient === acs);
  if (!bearer.length) return stop("sso-failed", "wrong-recipient", "no bearer confirmation names this organisation's ACS URL");
  const untils = bearer.map((d) => Date.parse(d.NotOnOrAfter)).filter(Number.isFinite);
  if (untils.length !== bearer.length) return stop("sso-failed", "expired", "a bearer confirmation without NotOnOrAfter");
  const once = await deps.orgAsk(env, slug, {
    op: "saml-consume",
    principals: [],
    args: { assertion_id: signed.id, until: Math.ceil(Math.max(...untils) / 1000) + 120 },
  });
  if (once.status !== 200) return stop("sso-failed", "replayed", "an assertion presented twice");

  // The domain rules are company sign-in's (org-sso.js).
  if (!c.domain_verified) return stop("domain-not-verified");
  const email = emailOf(profile);
  const principal = email ? `email:${email}` : null;
  if (!principal || !atDomain(principal, c.domain)) return stop("sso-wrong-domain");

  const provider = `saml-${createHash("sha256").update(c.idp_entity_id).digest("hex").slice(0, 16)}`;
  const user = await deps.ensureUser(env, provider, String(profile.nameID || email), {
    email,
    verified_email: email,
    name: typeof profile.displayName === "string" ? profile.displayName.slice(0, 120) : "",
    avatar_url: "",
  });
  const token = await deps.newSession(env, user.id);
  await markSsoSession(env, user.id, token, slug);
  const join = await deps.orgAsk(env, slug, { op: "sso-join", principals: [principal, `sso:${slug}`], args: { principal } });
  const joined = join.status === 200 ? join.body : { state: "unknown" };
  if (joined.joined && deps.noteOrgOf) await deps.noteOrgOf(env, principal, slug, c.org.name, joined.role);
  const delivered = deps.deliver(record, user, token);
  const location = `${delivered.headers.get("location")}&org=${encodeURIComponent(slug)}&sso=${encodeURIComponent(joined.state || "unknown")}`;
  return new Response(null, { status: 302, headers: { location } });
}
