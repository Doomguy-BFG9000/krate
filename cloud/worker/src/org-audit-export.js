// Audit export over a signed webhook (IC-955).
//
// An owner gives the organisation a webhook URL in Settings. From then on
// every event appended to the organisation's audit chain is POSTed there as
// JSON, one event per request, in chain order, so a SIEM (Splunk, Datadog,
// Sentinel, anything that takes an HTTP event) holds the same record the
// hub does and can check the chain itself: each event carries its
// `prev_hash` and `hash`.
//
// The request is signed the way Stripe signs its webhooks:
//
//   X-Krate-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256>
//
// where the MAC is over the bytes `<t>.<body>` with the secret as the key
// (its UTF-8 bytes, `whsec_...` included). The secret is shown once, when it
// is made, and stored encrypted with ORG_SECRET_KEY (AES-GCM, the
// organisation's slug bound in as additional data, as company sign-in's
// client secret is). A receiver checks the MAC and refuses a `t` more than a
// few minutes old, so a captured request cannot be replayed later.
// `X-Krate-Event-Id` (`<slug>:<seq>`) lets it drop a repeat.
//
// Delivery cannot happen inside the store's transaction (it cannot wait on
// the network), so an event is queued in the same transaction that appends
// it, and the store's Durable Object alarm delivers the queue: the oldest
// event first, the next only once it is accepted (any 2xx). A refusal, a
// redirect, a timeout or a network failure keeps it queued and retries with
// backoff (30 s, doubling, at most an hour apart). An event still not
// delivered 24 hours after it was queued is given up and counted, so a dead
// receiver never holds the queue forever. The state is on the dashboard;
// the audit itself is never touched by any of this.

import { Org, extendOrg, allows } from "./org.js";
import { sealSecret, openSecret, reachable, devHub } from "./org-sso.js";

export const EXPORT_GIVE_UP_SECONDS = 24 * 60 * 60;
export const EXPORT_BATCH = 25;
const BACKOFF_FIRST = 30;
const BACKOFF_MAX = 60 * 60;
const DELIVERY_TIMEOUT_MS = 10_000;
const URL_MAX = 2000;

/// How long after the `n`th failed attempt the next one waits.
export function backoffSeconds(n) {
  return Math.min(BACKOFF_MAX, BACKOFF_FIRST * 2 ** Math.max(0, n - 1));
}

const SCHEMA = [
  `CREATE TABLE IF NOT EXISTS audit_export (
     id INTEGER PRIMARY KEY CHECK (id = 1),
     url TEXT NOT NULL, host TEXT NOT NULL, secret TEXT NOT NULL,
     set_at INTEGER NOT NULL, set_by TEXT NOT NULL,
     delivered INTEGER NOT NULL DEFAULT 0, given_up INTEGER NOT NULL DEFAULT 0,
     last_delivered_at INTEGER, last_delivered_seq INTEGER,
     last_error TEXT, last_error_at INTEGER, last_status INTEGER)`,
  `CREATE TABLE IF NOT EXISTS audit_outbox (
     seq INTEGER PRIMARY KEY, queued_at INTEGER NOT NULL,
     attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL)`,
];

const ops = {
  exportConfig() {
    return this.row("SELECT * FROM audit_export WHERE id = 1");
  },

  exportView() {
    const c = this.exportConfig();
    if (!c) return null;
    // The next attempt is the oldest event's: the rest wait behind it.
    const q = this.row(
      "SELECT COUNT(*) AS n, MIN(queued_at) AS oldest, (SELECT next_at FROM audit_outbox ORDER BY seq LIMIT 1) AS next FROM audit_outbox",
    );
    return {
      url: c.url,
      host: c.host,
      has_secret: true,
      set_at: c.set_at,
      set_by: c.set_by,
      signature: "X-Krate-Signature: t=<unix>,v1=<hex HMAC-SHA256 of \"<t>.<body>\">",
      delivery: {
        pending: q.n,
        oldest_pending_at: q.n ? q.oldest : null,
        next_attempt_at: q.n ? q.next : null,
        delivered: c.delivered,
        given_up: c.given_up,
        last_delivered_at: c.last_delivered_at || null,
        last_delivered_seq: c.last_delivered_seq || null,
        last_error: c.last_error || null,
        last_error_at: c.last_error_at || null,
        last_status: c.last_status || null,
      },
    };
  },

  exportMay(ctx, action, rule) {
    const { me, refusal } = this.standing(ctx, action, "audit-export");
    if (refusal) return { refusal };
    if (allows(me.role, rule) !== "yes") {
      const words = rule === "audit-export.view" ? `only an owner or admin of ${ctx.slug} sees the audit export` : `only an owner of ${ctx.slug} sets up the audit export`;
      return { refusal: this.deny(ctx, action, "audit-export", "role-cannot", words) };
    }
    return { me };
  },

  /// GET /orgs/<slug>/audit-export (owner, admin): null, or the settings and
  /// the delivery state. Never the secret.
  "audit-export-get"(ctx) {
    const { refusal } = this.exportMay(ctx, "audit-export.view", "audit-export.view");
    if (refusal) return refusal;
    return this.ok(this.exportView());
  },

  /// PUT /orgs/<slug>/audit-export {url} (owner). The Worker checked the
  /// URL and, the first time, made and sealed the secret. Events from this
  /// one on are sent; the setting itself is the first.
  "audit-export-set"(ctx) {
    const { me, refusal } = this.exportMay(ctx, "audit-export.set", "audit-export.manage");
    if (refusal) return refusal;
    const before = this.exportConfig();
    const url = String(ctx.args.url || "");
    const host = String(ctx.args.host || "");
    const secret = ctx.args.secret ? JSON.stringify(ctx.args.secret) : before ? before.secret : null;
    if (!url || !host || !secret) return { status: 400, body: { error: "bad-request", message: "give the webhook's https URL" } };
    if (before) {
      this.sql.exec("UPDATE audit_export SET url = ?, host = ?, secret = ?, set_at = ?, set_by = ? WHERE id = 1", url, host, secret, ctx.now, me.principal);
    } else {
      this.sql.exec(
        "INSERT INTO audit_export (id, url, host, secret, set_at, set_by) VALUES (1, ?, ?, ?, ?, ?)",
        url,
        host,
        secret,
        ctx.now,
        me.principal,
      );
    }
    // The host only: a URL may carry the receiver's own key in its query.
    this.audit(me.principal, before ? "audit-export.change" : "audit-export.set", `audit-export:${host}`, "allowed");
    return this.ok(this.exportView(), before ? 200 : 201);
  },

  /// POST /orgs/<slug>/audit-export/secret (owner): a new secret, sealed by
  /// the Worker; the old one stops signing at once.
  "audit-export-secret"(ctx) {
    const { me, refusal } = this.exportMay(ctx, "audit-export.secret", "audit-export.manage");
    if (refusal) return refusal;
    const c = this.exportConfig();
    if (!c) return { status: 404, body: { error: "audit-export-off", message: `${ctx.slug} has no audit export yet -- set its webhook URL first` } };
    this.sql.exec("UPDATE audit_export SET secret = ? WHERE id = 1", JSON.stringify(ctx.args.secret));
    this.audit(me.principal, "audit-export.secret", `audit-export:${c.host}`, "allowed");
    return this.ok(this.exportView());
  },

  /// DELETE /orgs/<slug>/audit-export (owner): stop sending. What was still
  /// queued is not sent, and the audit says how much.
  "audit-export-clear"(ctx) {
    const { me, refusal } = this.exportMay(ctx, "audit-export.clear", "audit-export.manage");
    if (refusal) return refusal;
    const c = this.exportConfig();
    if (c) {
      const dropped = this.row("SELECT COUNT(*) AS n FROM audit_outbox").n;
      this.sql.exec("DELETE FROM audit_export WHERE id = 1");
      this.sql.exec("DELETE FROM audit_outbox");
      this.audit(me.principal, "audit-export.clear", `audit-export:${c.host}`, "allowed", `${dropped} not sent`);
    }
    return this.ok(null, 204);
  },

  /// POST /orgs/<slug>/audit-export/test (owner): "Send a test event". It is
  /// a real audit event, so it goes the way every other one does.
  "audit-export-test"(ctx) {
    const { me, refusal } = this.exportMay(ctx, "audit-export.test", "audit-export.manage");
    if (refusal) return refusal;
    const c = this.exportConfig();
    if (!c) return { status: 404, body: { error: "audit-export-off", message: `${ctx.slug} has no audit export yet -- set its webhook URL first` } };
    const event = this.audit(me.principal, "audit-export.test", `audit-export:${c.host}`, "allowed");
    return this.ok({ queued: event.seq, export: this.exportView() }, 202);
  },

  // ---------------------------------------------------- the delivery side

  /// Make sure the alarm will run when the next queued event is due. Called
  /// after every command, outside its transaction.
  async exportArm(force = false) {
    // When the oldest event is due: the rest wait behind it, so waking for
    // a later one that is "due" would only spin.
    const head = this.row("SELECT next_at AS t FROM audit_outbox ORDER BY seq LIMIT 1");
    if (!head) return;
    const next = head.t;
    const storage = this.ctx.storage;
    if (typeof storage.setAlarm !== "function") return;
    const want = next * 1000;
    // Inside the alarm itself the one firing may still read as set, so the
    // alarm sets the next one whatever it reads.
    const have = force ? null : await storage.getAlarm();
    if (have === null || have === undefined || have > want) await storage.setAlarm(want);
  },

  /// One delivery run (the Durable Object's alarm). Answers what it did, for
  /// the tests.
  async exportDeliver() {
    const done = { delivered: 0, failed: 0, given_up: 0 };
    const c = this.exportConfig();
    const o = this.org();
    if (!c || !o) {
      this.sql.exec("DELETE FROM audit_outbox");
      return done;
    }
    const now = this.now();
    // Given up: queued more than a day ago and still not taken.
    this.ctx.storage.transactionSync(() => {
      const old = this.row("SELECT COUNT(*) AS n FROM audit_outbox WHERE queued_at + ? <= ?", EXPORT_GIVE_UP_SECONDS, now).n;
      if (old) {
        this.sql.exec("DELETE FROM audit_outbox WHERE queued_at + ? <= ?", EXPORT_GIVE_UP_SECONDS, now);
        this.sql.exec("UPDATE audit_export SET given_up = given_up + ? WHERE id = 1", old);
        done.given_up = old;
      }
    });
    const secret = await openSecret(this.env, o.slug, JSON.parse(c.secret), `krate-org-webhook:${o.slug}`);
    for (let i = 0; i < EXPORT_BATCH; i++) {
      const head = this.row("SELECT * FROM audit_outbox ORDER BY seq LIMIT 1");
      if (!head || head.next_at > now) break;
      const event = this.row("SELECT seq, at, actor, action, target, result, reason, prev_hash, hash FROM audit WHERE seq = ?", head.seq);
      let ok = false;
      let status = null;
      let problem = null;
      if (!event) {
        ok = true; // Nothing to send (cannot happen: the audit is never edited).
      } else if (secret === null) {
        problem = "the signing secret cannot be opened on this hub -- make a new secret in Settings";
      } else {
        const body = JSON.stringify({ type: "krate.audit.event", v: 1, org: o.slug, attempt: head.attempts + 1, event });
        const t = String(now);
        try {
          const res = await fetch(c.url, {
            method: "POST",
            headers: {
              "content-type": "application/json",
              "user-agent": "Krate-Audit-Export/1",
              "x-krate-event-id": `${o.slug}:${event.seq}`,
              "x-krate-signature": `t=${t},v1=${await hmacHex(secret, `${t}.${body}`)}`,
            },
            body,
            redirect: "manual",
            signal: AbortSignal.timeout(DELIVERY_TIMEOUT_MS),
          });
          status = res.status;
          // Read and drop the answer, so the connection is freed.
          await res.arrayBuffer().catch(() => {});
          ok = status >= 200 && status < 300;
          if (!ok) problem = status >= 300 && status < 400 ? `the receiver answered a redirect (${status}); redirects are not followed` : `the receiver answered ${status}`;
        } catch (err) {
          problem = err && err.name === "TimeoutError" ? "the receiver did not answer within 10 seconds" : `the receiver could not be reached (${String((err && err.message) || err).slice(0, 120)})`;
        }
      }
      const stillQueued = this.ctx.storage.transactionSync(() => {
        // The export may have been switched off or changed while this waited.
        const now2 = this.exportConfig();
        if (!now2 || now2.url !== c.url) return false;
        if (ok) {
          this.sql.exec("DELETE FROM audit_outbox WHERE seq = ?", head.seq);
          this.sql.exec(
            "UPDATE audit_export SET delivered = delivered + 1, last_delivered_at = ?, last_delivered_seq = ?, last_status = ? WHERE id = 1",
            now,
            head.seq,
            status,
          );
          return true;
        }
        const attempts = head.attempts + 1;
        this.sql.exec("UPDATE audit_outbox SET attempts = ?, next_at = ? WHERE seq = ?", attempts, now + backoffSeconds(attempts), head.seq);
        this.sql.exec("UPDATE audit_export SET last_error = ?, last_error_at = ?, last_status = ? WHERE id = 1", problem, now, status);
        return true;
      });
      if (!stillQueued) break;
      if (!ok) {
        done.failed += 1;
        break;
      }
      done.delivered += 1;
    }
    return done;
  },
};

const b64url = (bytes) => btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

/// HMAC-SHA256 of `text` under `secret` (its UTF-8 bytes), as lower hex.
export async function hmacHex(secret, text) {
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const mac = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(text));
  return [...new Uint8Array(mac)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/// Check a delivery the way a receiver should: the MAC over `<t>.<body>`,
/// in constant time, and `t` within `tolerance` seconds of now. For the
/// docs and the tests.
export async function verifySignature(secret, header, body, { now = Math.floor(Date.now() / 1000), tolerance = 300 } = {}) {
  const parts = Object.fromEntries(
    String(header || "")
      .split(",")
      .map((p) => p.trim().split("="))
      .filter((kv) => kv.length === 2),
  );
  const t = Number(parts.t);
  if (!Number.isInteger(t) || !/^[0-9a-f]{64}$/.test(parts.v1 || "")) return false;
  if (Math.abs(now - t) > tolerance) return false;
  const want = await hmacHex(secret, `${parts.t}.${body}`);
  let diff = 0;
  for (let i = 0; i < 64; i++) diff |= want.charCodeAt(i) ^ parts.v1.charCodeAt(i);
  return diff === 0;
}

/// A new webhook secret: `whsec_` and 256 random bits.
export function newWebhookSecret() {
  return `whsec_${b64url(crypto.getRandomValues(new Uint8Array(32)))}`;
}

function install(OrgClass) {
  const helpers = {};
  const commands = {};
  for (const [k, fn] of Object.entries(ops)) (k.startsWith("audit-export-") ? commands : helpers)[k] = fn;
  Object.assign(OrgClass.prototype, helpers);
  extendOrg({ schema: SCHEMA, ops: commands, mutations: ["audit-export-clear"] });
  const P = OrgClass.prototype;

  // Every event appended while an export is set is queued in the same
  // transaction: if the command rolls back, so does its place in the queue.
  const audit = P.audit;
  P.audit = function (...args) {
    const event = audit.apply(this, args);
    if (this.exportConfig()) {
      this.sql.exec("INSERT INTO audit_outbox (seq, queued_at, attempts, next_at) VALUES (?, ?, 0, ?)", event.seq, event.at, event.at);
    }
    return event;
  };

  // After each command, outside its transaction: the alarm is set for
  // whatever it queued.
  const fetch = P.fetch;
  P.fetch = async function (request) {
    const res = await fetch.call(this, request);
    try {
      await this.exportArm();
    } catch (err) {
      console.error("audit export: could not set the alarm", err && err.message);
    }
    return res;
  };

  // The Durable Object's alarm: deliver, then wake again for what is left.
  const alarm = P.alarm;
  P.alarm = async function (...args) {
    if (typeof alarm === "function") await alarm.apply(this, args);
    const done = await this.exportDeliver();
    await this.exportArm(true);
    return done;
  };
}

install(Org);

// ---------------------------------------------------------------- routes

/// Private and loopback addresses written as IP literals. The hub runs on
/// Cloudflare's network, not the company's, so these name nothing a company
/// could mean; refusing them keeps the export from being pointed at
/// anything but a real receiver.
function privateLiteral(host) {
  const h = host.replace(/^\[|\]$/g, "").toLowerCase();
  if (h === "localhost" || h.endsWith(".localhost")) return true;
  const v4 = /^(\d+)\.(\d+)\.(\d+)\.(\d+)$/.exec(h);
  if (v4) {
    const [a, b] = [Number(v4[1]), Number(v4[2])];
    return a === 10 || a === 127 || a === 0 || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 100 && b >= 64 && b <= 127);
  }
  return h.includes(":") && (h === "::1" || h.startsWith("fc") || h.startsWith("fd") || h.startsWith("fe80") || h === "::");
}

/// The webhook URL, checked: https (or plain http on this machine when the
/// hub itself is a local proof), no credentials in it, not a private
/// address. Answers `{url, host}` or `{problem}`.
export function webhookUrl(raw, env) {
  const s = String(raw || "").trim();
  if (!s || s.length > URL_MAX) return { problem: "give the receiver's https URL, such as https://siem.example.com/krate" };
  if (!reachable(s, env)) {
    return { problem: "the webhook is an https URL, with no user name or password in it (put a receiver's key in its query or rely on the signature)" };
  }
  const u = new URL(s);
  if (!devHub(env) && privateLiteral(u.hostname)) return { problem: "that is a private or loopback address -- the hub sends from the internet, so give the receiver's public https URL" };
  return { url: u.toString(), host: u.host };
}

/// /orgs/<slug>/audit-export[/secret|/test]. Answers a Response, or null.
export async function auditExportRoute(h) {
  const { rest, method, env, slug } = h;
  if (rest[0] !== "audit-export") return null;
  const off = () => h.json({ error: "audit-export-off", message: "audit export is not switched on on this hub (it has no ORG_SECRET_KEY)" }, 503);
  if (rest.length === 1 && method === "GET") return h.orgAnswer(await h.askRaw("audit-export-get"));
  if (rest.length === 1 && method === "DELETE") return h.orgAnswer(await h.mutateRaw("audit-export-clear", () => ({})));
  if (rest.length === 1 && method === "PUT") {
    const body = await h.readBody();
    if (body.bad || body.tooLarge) return h.json({ error: "bad-request", message: "the body is JSON, under 64 KiB" }, 400);
    const checked = webhookUrl(body.value.url, env);
    if (checked.problem) return h.json({ error: "bad-request", message: checked.problem }, 400);
    // Who may, before a secret is made: a stranger costs no crypto.
    const current = await h.askRaw("audit-export-get");
    if (current.status !== 200) return h.orgAnswer(current);
    let secret;
    let sealed;
    if (!current.body) {
      secret = newWebhookSecret();
      sealed = await sealSecret(env, slug, secret, `krate-org-webhook:${slug}`);
      if (!sealed) return off();
    }
    const answer = await h.askRaw("audit-export-set", { url: checked.url, host: checked.host, secret: sealed });
    if (answer.status >= 300 || !secret) return h.orgAnswer(answer);
    const res = h.json({ ...answer.body, secret }, answer.status);
    res.headers.set("cache-control", "no-store");
    return res;
  }
  if (rest.length === 2 && rest[1] === "secret" && method === "POST") {
    const current = await h.askRaw("audit-export-get");
    if (current.status !== 200) return h.orgAnswer(current);
    const secret = newWebhookSecret();
    const sealed = await sealSecret(env, slug, secret, `krate-org-webhook:${slug}`);
    if (!sealed) return off();
    const answer = await h.askRaw("audit-export-secret", { secret: sealed });
    if (answer.status !== 200) return h.orgAnswer(answer);
    const res = h.json({ ...answer.body, secret }, 200);
    res.headers.set("cache-control", "no-store");
    return res;
  }
  if (rest.length === 2 && rest[1] === "test" && method === "POST") return h.orgAnswer(await h.askRaw("audit-export-test"));
  return null;
}
