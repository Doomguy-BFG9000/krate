// Signing nobody has to think about (IC-958 A, D and the one-minute rule).
//
// Every app an organisation releases is signed by the organisation, and no
// person ever handles a key. Three things make that true, and this module
// is the hub's half of each:
//
// - THE SIGNING RING. The organisation's signing key (Ed25519) is made on
//   its first owner device -- a computer running Krate, or the owner's
//   browser -- and its public half becomes the organisation's root. The
//   private half is sealed (HPKE, by crates/orgkeys, on the device) to
//   every OWNER and ADMIN device and to the recovery key, exactly as the
//   data keyring travels (org-keys.js, whose device registrations it
//   reuses). The hub keeps envelopes only. A new owner or admin device gets
//   it from any holder device, after this store says they are an active
//   owner or admin. Versioned; every grant audited.
//
// - AUTOMATIC PUBLISHERS. A developer's Krate makes its release key on its
//   first publish and asks here to publish with it. Any online holder
//   device signs a permission slip (a delegation: the namespace `<org>/*`,
//   365 days) in the background once this store says the requester may
//   publish; the slip is checked here by the validator (the root's
//   signature, the key, the namespace, the window) and kept for the
//   requester, whose Krate installs it. With "approve new publishers by
//   hand" on, an owner or admin clicks once and the next holder device signs.
//
// - AUTOMATIC WITHDRAWAL. Removing a member (by hand or by SCIM) or taking
//   away their right to publish marks their release keys due for
//   withdrawal; the door refuses those keys at once, and the next holder
//   device signs a revocation list the validator checks, so the runtime's
//   own revocation check agrees.
//
// And SOLO RELEASE: while an organisation has nobody but the uploader who
// may approve, its owner may release their own upload. Each such release
// is marked "released by its sender (no second approver yet)" here and in
// the audit. The two-person rule turns on by itself when a second person
// who may approve joins (an owner or reviewer -- the roles the role matrix
// lets approve), and Settings can turn it on from the start.
//
// ----------------------------------------------------------------- routes
//
// Every route is under /orgs/<slug>/, needs a sign-in, answers JSON, and
// refuses as `{error, message}` with a stable reason. Every POST and PUT
// takes an Idempotency-Key. Stable for the dashboard pages:
//
//   GET  /publishers
//        owners, admins -> { by_hand, approvers: [principal],
//          waiting: [{ principal, asked_at, approved_by? }],
//          publishers: [Publisher] }
//        anyone else    -> { by_hand, approvers: [principal],
//          me: { state: "waiting" | "publisher" | "none", asked_at } }
//        approvers: the owners and admins whose devices hold the signing
//        key (who can approve now); before any does, every owner and admin.
//   PUT  /publishers/by-hand {by_hand: bool}   (owner) -> /settings/publishing
//   POST /publishers/<principal>/approve       (owner, admin; `github:carol`,
//        URL-encoded) -> Publisher, state "approved"
//   POST /publishers {public_key: 64-hex Ed25519, name?: "Carol's MacBook"}
//        -> 201 Publisher (200 with the record when that key already asked).
//   GET  /publishers/<key>
//        -> Publisher
//   POST /publishers/<key>/approve            (owner, admin; by-hand mode)
//        -> Publisher, state "approved": the next holder device signs it.
//   POST /publishers/<key>/delegation {delegation: SignedDelegation, by_device}
//        (a holder device)  -> Publisher, state "signed"
//
//   Publisher = { key, principal, display, name, state, requested_at,
//     by_hand, approved_by, approved_at, signed_by, signed_device,
//     signed_at, expires, delegation: SignedDelegation | null,
//     can_sign: ["alice", ...],  // who can approve it now: owners and
//                                // admins whose devices hold the ring
//     waiting_on: "a-holder-device" | "an-approval" | null,
//     closed_reason }
//   state: "waiting"  -- asked; a holder device signs it next
//                        (by_hand: waiting for an owner or admin's click)
//          "approved" -- clicked by hand; a holder device signs it next
//          "signed"   -- may publish until `expires`
//          "revoke-due", "revoked" -- withdrawn (left, or no longer
//                        publishes); refused at the door from the moment
//                        it is due
//          "closed"   -- the person left before it was signed
//
//   GET  /settings/publishing
//        -> { publishers_by_hand, two_person_from_start, two_person,
//             solo_release, approvers: ["bob", ...], signing: bool }
//        two_person: whether every release needs a second person now;
//        solo_release: whether the asking owner may release their own
//        upload now; approvers: who may approve (owners and reviewers).
//   PUT  /settings/publishing {publishers_by_hand?: bool,
//        two_person_from_start?: bool}   (owner) -> the same shape.
//   PUT  /release-rule {two_person: bool}   (owner) -> the same shape:
//        two_person true is "every release needs a second person, from the
//        start"; false lets a lone owner release while nobody else may
//        approve.
//
//   GET  /signing?device=<id>   (a device's view of the ring; for Krate)
//   POST /signing/init {device_id, version: 1, public_key, envelope,
//        recovery_blob}   (owner: the organisation's first signing key)
//   POST /signing/grants {device_id, version, blob, by_device}
//        (a holder device passes the ring on; by_device "recovery" is an
//        owner or admin restoring their own device from the recovery code)
//   GET  /signing/recovery   (owner, admin) -> {version, blob}
//   POST /signing/revocations {list: SignedRevocationList, by_device}
//
// And changes to routes that exist: POST /candidates answers
// `solo_release: true` when the uploader may release it themselves, and
// `approvers: [...]` (who else may approve); POST
// /candidates/<digest>/approve by its own uploader releases it when solo
// release applies -- `{release: {digest, url, solo: true}}`, its audit
// event `candidate.approve` with reason "solo", and `solo: true` in
// /candidates and /releases -- and is refused `self-approval` otherwise.
// GET /orgs/<slug> carries `org.publishing` (/settings/publishing's answer)
// and `org.release_rule`: "solo" or "two-person". GET /keys lists each
// device with `kind`: "browser" or "computer" (org-keys.js).

import { Org, MATRIX, extendOrg, addReasons, allows, displayOf } from "./org.js";
import "./org-keys.js";
import "./org-review.js";
import "./org-releases.js";
import "./org-scim.js";

const HEX64 = /^[0-9a-f]{64}$/;
const DEVICE = /^[0-9a-f]{16}$/;
const B64 = /^[A-Za-z0-9+/]+={0,2}$/;
const ENVELOPE_MAX = 64 * 1024;
const NAME_MAX = 80;
/// How far ahead of the hub's clock a slip may start (a signer's clock).
const SKEW = 10 * 60;

const goodBlob = (b) => typeof b === "string" && b.length > 0 && b.length <= ENVELOPE_MAX && b.length % 4 === 0 && B64.test(b);
const short = (k) => String(k || "").slice(0, 16);
const day = (s) => new Date(Number(s) * 1000).toISOString().slice(0, 10);

// Who holds the ring, who approves publishers, who changes these settings.
MATRIX["signing.hold"] = { owner: "yes", admin: "yes" };
MATRIX["publishers.approve"] = { owner: "yes", admin: "yes" };
MATRIX["publishing.settings"] = { owner: "yes" };

addReasons({
  "no-signing-key": {
    status: 409,
    message: (o, d) => d || `${o} cannot sign apps yet -- its first owner opens Krate (or the dashboard) once, and Krate does the rest`,
  },
  "signing-key-exists": { status: 409, message: (o, d) => d || `${o} already signs its apps` },
  "signing-key-not-held": {
    status: 403,
    message: (o, d) => d || `this device cannot sign for ${o} yet -- another owner's or admin's device passes it on the next time it runs Krate`,
  },
  "not-a-publisher": {
    status: 404,
    message: (o, d) => d || `${o} has no request to publish from that key -- publish again and Krate asks for you`,
  },
  "needs-approval": {
    status: 409,
    message: (o, d) => d || `${o} approves new publishers by hand -- an owner or admin approves this one in the dashboard first`,
  },
  "slip-invalid": { status: 422, message: (o, d) => `that permission to publish does not hold: ${d || "it is not one this organisation signed"}` },
});

const SCHEMA = [
  `CREATE TABLE IF NOT EXISTS sign_ring (
     version INTEGER PRIMARY KEY, public_key TEXT NOT NULL,
     created_at INTEGER NOT NULL, created_by TEXT NOT NULL, created_device TEXT NOT NULL)`,
  `CREATE TABLE IF NOT EXISTS sign_envelopes (
     device_id TEXT PRIMARY KEY, version INTEGER NOT NULL, blob TEXT NOT NULL,
     granted_by TEXT NOT NULL, granted_device TEXT NOT NULL, at INTEGER NOT NULL)`,
  `CREATE TABLE IF NOT EXISTS sign_recovery (
     id INTEGER PRIMARY KEY CHECK (id = 1), version INTEGER NOT NULL, blob TEXT NOT NULL,
     set_at INTEGER NOT NULL, set_by TEXT NOT NULL)`,
  `CREATE TABLE IF NOT EXISTS publishers (
     key TEXT PRIMARY KEY, principal TEXT NOT NULL, name TEXT, requested_at INTEGER NOT NULL,
     state TEXT NOT NULL, approved_by TEXT, approved_at INTEGER,
     delegation TEXT, signed_by TEXT, signed_device TEXT, signed_at INTEGER, expires INTEGER,
     revoke_since INTEGER, closed_at INTEGER, closed_reason TEXT)`,
  `CREATE TABLE IF NOT EXISTS publish_settings (
     id INTEGER PRIMARY KEY CHECK (id = 1), by_hand INTEGER NOT NULL DEFAULT 0,
     two_person INTEGER NOT NULL DEFAULT 0, set_at INTEGER, set_by TEXT)`,
];
const COLUMNS = [["candidates", "solo", "INTEGER"]];

/// What the dashboard and the audit say about a solo release.
export const SOLO_NOTE = "released by its sender (no second approver yet)";

const ops = {
  publishSettings() {
    const r = this.row("SELECT * FROM publish_settings WHERE id = 1");
    return { by_hand: Boolean(r && r.by_hand), two_person: Boolean(r && r.two_person) };
  },

  currentRing() {
    return this.row("SELECT * FROM sign_ring ORDER BY version DESC LIMIT 1");
  },

  ringEnvelope(id) {
    return this.row("SELECT * FROM sign_envelopes WHERE device_id = ?", id);
  },

  /// Active devices of active owners and admins: who holds the ring.
  ringDevices() {
    return this.liveDevices().filter((d) => {
      const m = this.member(d.principal);
      return m && allows(m.role, "signing.hold") === "yes";
    });
  },

  /// The ring is the organisation's signing key: its root is the ring's.
  ringIsCurrent(ring) {
    return Boolean(ring && ring.public_key === this.rootKey());
  },

  /// Who can approve a publisher right now: owners and admins whose
  /// devices hold the current ring, as people read them.
  ringHolders() {
    const ring = this.currentRing();
    if (!ring || !this.ringIsCurrent(ring)) return [];
    const seen = new Map();
    for (const d of this.ringDevices()) {
      const e = this.ringEnvelope(d.device_id);
      if (!e || e.version !== ring.version) continue;
      if (!seen.has(d.principal)) seen.set(d.principal, []);
      seen.get(d.principal).push(d.name || "a computer");
    }
    return [...seen.entries()].map(([principal, devices]) => ({ principal, display: displayOf(principal), devices }));
  },

  /// Active members, other than `except`, whose role may approve.
  approvers(except = []) {
    return this.rows("SELECT principal, role FROM members WHERE state = 'active' ORDER BY added_at, rowid")
      .filter((m) => allows(m.role, "candidate.approve") === "yes" && !except.includes(m.principal))
      .map((m) => m.principal);
  },

  /// Whether every release needs a second person now.
  twoPersonInForce() {
    return this.publishSettings().two_person || this.approvers().length >= 2;
  },

  /// Whether `principal` may release their own upload: an active owner,
  /// the two-person rule not on from Settings, and nobody else who may
  /// approve.
  soloOpenFor(principals) {
    const list = Array.isArray(principals) ? principals : [principals];
    if (this.publishSettings().two_person) return false;
    const owner = list.map((p) => this.member(p)).find((m) => m && m.state === "active" && m.role === "owner");
    if (!owner) return false;
    return this.approvers(list).length === 0;
  },

  publishingView(me) {
    const s = this.publishSettings();
    return {
      publishers_by_hand: s.by_hand,
      two_person_from_start: s.two_person,
      two_person: this.twoPersonInForce(),
      solo_release: me ? this.soloOpenFor([me.principal]) : false,
      approvers: this.approvers().map(displayOf),
      signing: this.ringIsCurrent(this.currentRing()),
    };
  },

  publisherView(p) {
    const s = this.publishSettings();
    let delegation = null;
    try {
      delegation = p.delegation ? JSON.parse(p.delegation) : null;
    } catch {
      delegation = null;
    }
    const waiting = p.state === "waiting" || p.state === "approved";
    return {
      key: p.key,
      principal: p.principal,
      display: displayOf(p.principal),
      name: p.name || "",
      state: p.state,
      requested_at: p.requested_at,
      by_hand: s.by_hand,
      approved_by: p.approved_by || null,
      approved_at: p.approved_at || null,
      signed_by: p.signed_by || null,
      signed_device: p.signed_device || null,
      signed_at: p.signed_at || null,
      expires: p.expires || null,
      delegation: p.state === "signed" ? delegation : null,
      can_sign: this.ringHolders().map((h) => h.display),
      waiting_on: !waiting ? null : p.state === "waiting" && s.by_hand ? "an-approval" : "a-holder-device",
      closed_reason: p.closed_reason || null,
    };
  },

  publisherRow(key) {
    return this.row("SELECT * FROM publishers WHERE key = ?", String(key || "").toLowerCase());
  },

  /// Whether `principal` may publish: an active member whose role may.
  mayPublish(principal) {
    const m = this.member(principal);
    return Boolean(m && m.state === "active" && allows(m.role, "candidate.publish") === "yes");
  },

  /// A member's release keys stop: signed ones are due for withdrawal (the
  /// door refuses them from now), unsigned requests close.
  endPublisher(actor, principal, now, why) {
    for (const p of this.rows("SELECT * FROM publishers WHERE principal = ? AND state IN ('waiting','approved','signed')", principal)) {
      if (p.state === "signed") {
        this.sql.exec("UPDATE publishers SET state = 'revoke-due', revoke_since = ?, closed_reason = ? WHERE key = ?", now, why, p.key);
        this.audit(actor, "publisher.revoke-due", `release-key:${short(p.key)}`, "allowed", `${displayOf(principal)}: ${why}`);
      } else {
        this.sql.exec("UPDATE publishers SET state = 'closed', closed_at = ?, closed_reason = ? WHERE key = ?", now, why, p.key);
        this.audit(actor, "publisher.close", `release-key:${short(p.key)}`, "allowed", `${displayOf(principal)}: ${why}`);
      }
    }
  },

  /// The caller's standing to act as a holder of the ring.
  ringStanding(ctx, action, target) {
    const { me, refusal } = this.standing(ctx, action, target);
    if (refusal) return { refusal };
    if (allows(me.role, "signing.hold") !== "yes") {
      return { refusal: this.deny(ctx, action, target, "role-cannot", `only ${ctx.slug}'s owners and admins sign for it`) };
    }
    return { me };
  },

  /// The caller's own device, holding the current ring that is the
  /// organisation's signing key; or the refusal.
  holderDevice(ctx, me, action, target, id) {
    const ring = this.currentRing();
    if (!ring || !this.ringIsCurrent(ring)) return { refusal: this.deny(ctx, action, target, "no-signing-key") };
    const dev = this.myDevice(ctx, me, id);
    const env = dev ? this.ringEnvelope(dev.device_id) : null;
    if (!dev || !env || env.version !== ring.version) return { refusal: this.deny(ctx, action, target, "signing-key-not-held") };
    return { ring, dev };
  },

  /// GET /orgs/<slug>/signing?device=<id>
  op_signing_view(ctx) {
    const { me, refusal } = this.standing(ctx, "signing.view", `org:${ctx.slug}`);
    if (refusal) return refusal;
    const mayHold = allows(me.role, "signing.hold") === "yes";
    const ring = this.currentRing();
    const current = this.ringIsCurrent(ring);
    const mine = this.myDevice(ctx, me, ctx.args.device);
    const env = mine && mayHold ? this.ringEnvelope(mine.device_id) : null;
    const holds = Boolean(ring && env && env.version === ring.version);
    const out = {
      ring: ring ? { version: ring.version, public_key: ring.public_key, current } : null,
      root_key: this.rootKey(),
      may_hold: mayHold,
      device: mine ? { device_id: mine.device_id, state: mine.state } : null,
      envelope: env ? { version: env.version, blob: env.blob } : null,
      ring_requests: [],
      publisher_requests: [],
      revoke: [],
      recovery: null,
      holders: this.ringHolders().map(({ principal, display, devices }) => ({ principal, display, devices })),
      settings: this.publishingView(me),
    };
    if (mayHold) {
      const r = this.row("SELECT public_key FROM key_recovery WHERE id = 1");
      if (r) out.recovery = { public_key: r.public_key };
    }
    if (holds && current) {
      out.ring_requests = this.ringDevices()
        .filter((d) => {
          const e = this.ringEnvelope(d.device_id);
          return !e || e.version !== ring.version;
        })
        .map((d) => ({ device_id: d.device_id, principal: d.principal, public_key: d.public_key, name: d.name }));
      const byHand = this.publishSettings().by_hand;
      out.publisher_requests = this.rows("SELECT * FROM publishers WHERE state IN ('waiting','approved') ORDER BY requested_at, key")
        .filter((p) => (p.state === "approved" || !byHand) && this.mayPublish(p.principal))
        .map((p) => ({ key: p.key, principal: p.principal, display: displayOf(p.principal), name: p.name || "", requested_at: p.requested_at }));
      out.revoke = this.rows("SELECT * FROM publishers WHERE state = 'revoke-due' ORDER BY revoke_since, key")
        .map((p) => ({ key: p.key, principal: p.principal, display: displayOf(p.principal), since: p.revoke_since, reason: p.closed_reason || "left the organisation" }));
    }
    return this.ok(out);
  },

  /// POST /orgs/<slug>/signing/init: the organisation's first signing key,
  /// made on an owner's device.
  op_signing_init(ctx) {
    const target = "ring:v1";
    const { me, refusal } = this.standing(ctx, "signing.init", target);
    if (refusal) return refusal;
    if (allows(me.role, "root-key.set") !== "yes") return this.deny(ctx, "signing.init", target, "role-cannot", `only an owner makes ${ctx.slug}'s signing key`);
    if (this.currentRing()) return this.deny(ctx, "signing.init", target, "signing-key-exists");
    const a = ctx.args;
    const dev = this.myDevice(ctx, me, a.device_id);
    if (!dev) return this.deny(ctx, "signing.init", target, "device-unknown", "register this device first");
    const pub = String(a.public_key || "").toLowerCase();
    if (a.version !== 1 || !HEX64.test(pub) || !goodBlob(a.envelope) || (a.recovery_blob != null && !goodBlob(a.recovery_blob))) {
      return { status: 400, body: { error: "bad-request", message: "version 1, a 64-hex public key and its envelope are each required" } };
    }
    const root = this.rootKey();
    if (root && root !== pub) {
      return this.deny(ctx, "signing.init", target, "signing-key-exists", `${ctx.slug} already has a signing key, registered by hand -- keep using it, or register this one with \`krate org root-key\``);
    }
    this.sql.exec(
      "INSERT INTO sign_ring (version, public_key, created_at, created_by, created_device) VALUES (1, ?, ?, ?, ?)",
      pub, ctx.now, me.principal, dev.device_id,
    );
    this.sql.exec(
      "INSERT INTO sign_envelopes (device_id, version, blob, granted_by, granted_device, at) VALUES (?, 1, ?, ?, ?, ?)",
      dev.device_id, a.envelope, me.principal, dev.device_id, ctx.now,
    );
    if (a.recovery_blob) {
      this.sql.exec("INSERT INTO sign_recovery (id, version, blob, set_at, set_by) VALUES (1, 1, ?, ?, ?)", a.recovery_blob, ctx.now, me.principal);
    }
    if (!root) {
      const seq = (this.row("SELECT MAX(seq) AS n FROM root_keys").n || 0) + 1;
      this.sql.exec("INSERT INTO root_keys (seq, public_key, set_at, set_by) VALUES (?, ?, ?, ?)", seq, pub, ctx.now, me.principal);
      this.audit(me.principal, "root-key.register", `root-key:${short(pub)}`, "allowed", `made on ${dev.device_id}`);
    }
    this.audit(me.principal, "signing.init", target, "allowed", `on ${dev.device_id}${a.recovery_blob ? ", with the recovery code" : ""}`);
    return this.ok({ version: 1, public_key: pub }, 201);
  },

  /// POST /orgs/<slug>/signing/grants
  op_signing_grant(ctx) {
    const a = ctx.args;
    const id = String(a.device_id || "");
    const target = `device:${id}`;
    const { me, refusal } = this.ringStanding(ctx, "signing.grant", target);
    if (refusal) return refusal;
    const ring = this.currentRing();
    if (!ring) return this.deny(ctx, "signing.grant", target, "no-signing-key");
    if (a.version !== ring.version) return this.deny(ctx, "signing.grant", target, "key-rotated", `the current signing key is version ${ring.version}`);
    if (!DEVICE.test(id) || !goodBlob(a.blob)) return { status: 400, body: { error: "bad-request", message: "a device id and its envelope, base64 under 64 KiB" } };
    const to = this.deviceRow(id);
    const them = to ? this.member(to.principal) : null;
    if (!to || to.state !== "active" || !them || them.state !== "active" || allows(them.role, "signing.hold") !== "yes") {
      return this.deny(ctx, "signing.grant", target, "device-unknown", "that device is not an active owner's or admin's");
    }
    let how;
    if (a.by_device === "recovery") {
      if (to.principal !== me.principal) return this.deny(ctx, "signing.grant", target, "role-cannot", "the recovery code restores your own device");
      how = "recovery code";
    } else {
      const held = this.holderDevice(ctx, me, "signing.grant", target, a.by_device);
      if (held.refusal) return held.refusal;
      how = held.dev.device_id;
    }
    this.sql.exec(
      `INSERT INTO sign_envelopes (device_id, version, blob, granted_by, granted_device, at) VALUES (?, ?, ?, ?, ?, ?)
       ON CONFLICT (device_id) DO UPDATE SET version = excluded.version, blob = excluded.blob,
         granted_by = excluded.granted_by, granted_device = excluded.granted_device, at = excluded.at`,
      id, ring.version, a.blob, me.principal, how, ctx.now,
    );
    this.audit(me.principal, a.by_device === "recovery" ? "signing.recover" : "signing.grant", target, "allowed", `v${ring.version} to ${displayOf(to.principal)} by ${how}`);
    return this.ok({ device_id: id, version: ring.version });
  },

  /// GET /orgs/<slug>/signing/recovery
  op_signing_recovery(ctx) {
    const target = `org:${ctx.slug}`;
    const { me, refusal } = this.ringStanding(ctx, "signing.recover", target);
    if (refusal) return refusal;
    const r = this.row("SELECT * FROM sign_recovery WHERE id = 1");
    if (!r) return this.deny(ctx, "signing.recover", target, "no-signing-key", `${ctx.slug}'s signing key has no recovery envelope`);
    this.audit(me.principal, "signing.recovery.read", target, "allowed");
    return this.ok({ version: r.version, blob: r.blob });
  },

  /// POST /orgs/<slug>/signing/revocations: a holder device's signed list
  /// (read by the validator at the door: `root` and `revocations`).
  op_signing_revocations(ctx) {
    const a = ctx.args;
    const target = `root-key:${short(a.root)}`;
    const { me, refusal } = this.ringStanding(ctx, "signing.revoke", target);
    if (refusal) return refusal;
    const held = this.holderDevice(ctx, me, "signing.revoke", target, a.by_device);
    if (held.refusal) return held.refusal;
    if (a.root !== held.ring.public_key) {
      return this.deny(ctx, "signing.revoke", target, "slip-invalid", "the withdrawal list is signed by a different key than the organisation's");
    }
    let added = 0;
    for (const r of Array.isArray(a.revocations) ? a.revocations : []) {
      const k = String(r.key || "").toLowerCase();
      if (!HEX64.test(k)) continue;
      const p = this.publisherRow(k);
      if (!this.row("SELECT key FROM revoked_keys WHERE key = ?", k)) {
        this.sql.exec(
          "INSERT INTO revoked_keys (key, compromised_from, reason, root, recorded_at, recorded_by) VALUES (?, ?, ?, ?, ?, ?)",
          k, Number(r.compromised_from) || 0, String(r.reason || "").slice(0, 200), a.root, ctx.now, me.principal,
        );
        this.audit(me.principal, "release-key.revoke", `release-key:${short(k)}`, "allowed", `automatic, by ${held.dev.device_id}${p ? `: ${displayOf(p.principal)}` : ""}`);
        added += 1;
      }
      if (p && p.state !== "revoked") {
        this.sql.exec("UPDATE publishers SET state = 'revoked', closed_at = ? WHERE key = ?", ctx.now, k);
      }
    }
    return this.ok({ revoked_now: added });
  },

  /// POST /orgs/<slug>/publishers {public_key, name}
  op_publisher_ask(ctx) {
    const key = String(ctx.args.public_key || "").toLowerCase();
    const target = `release-key:${short(key)}`;
    const { me, refusal } = this.standing(ctx, "publisher.request", target);
    if (refusal) return refusal;
    if (allows(me.role, "candidate.publish") !== "yes") {
      return this.deny(ctx, "publisher.request", target, "role-cannot", `a ${me.role} cannot publish to ${ctx.slug} -- ask an owner to make you a developer`);
    }
    if (!HEX64.test(key)) return { status: 400, body: { error: "bad-request", message: "a release key is a 64-hex Ed25519 public key" } };
    const was = this.publisherRow(key);
    if (was) {
      if (was.principal !== me.principal) return this.deny(ctx, "publisher.request", target, "role-cannot", "that release key belongs to someone else");
      if (was.state === "closed") {
        this.sql.exec("UPDATE publishers SET state = 'waiting', requested_at = ?, closed_at = NULL, closed_reason = NULL WHERE key = ?", ctx.now, key);
        this.audit(me.principal, "publisher.request", target, "allowed", "again");
      }
      return this.ok(this.publisherView(this.publisherRow(key)));
    }
    if (this.row("SELECT key FROM revoked_keys WHERE key = ?", key)) {
      return this.deny(ctx, "publisher.request", target, "not-signed-for-this-org", "that release key was withdrawn -- Krate makes a new one when you publish again");
    }
    const name = String(ctx.args.name || "").slice(0, NAME_MAX);
    this.sql.exec(
      "INSERT INTO publishers (key, principal, name, requested_at, state) VALUES (?, ?, ?, ?, 'waiting')",
      key, me.principal, name, ctx.now,
    );
    this.audit(me.principal, "publisher.request", target, "allowed", name || null);
    return this.ok(this.publisherView(this.publisherRow(key)), 201);
  },

  /// GET /orgs/<slug>/publishers/<key>
  op_publisher_show(ctx) {
    const key = String(ctx.args.key || "").toLowerCase();
    const target = `release-key:${short(key)}`;
    const { me, refusal } = this.standing(ctx, "publisher.view", target);
    if (refusal) return refusal;
    const p = this.publisherRow(key);
    if (!p || (p.principal !== me.principal && allows(me.role, "publishers.approve") !== "yes")) {
      return { status: 404, body: { error: "not-a-publisher", message: `${ctx.slug} has no request to publish from that key -- publish again and Krate asks for you` } };
    }
    return this.ok(this.publisherView(p));
  },

  /// Who approves publishers: the owners and admins whose devices hold
  /// the signing key, or, before any does, every owner and admin.
  publisherApprovers() {
    const holders = this.ringHolders().map((h) => h.principal);
    if (holders.length) return holders;
    return this.rows("SELECT principal, role FROM members WHERE state = 'active' ORDER BY added_at, rowid")
      .filter((m) => allows(m.role, "publishers.approve") === "yes")
      .map((m) => m.principal);
  },

  /// GET /orgs/<slug>/publishers
  op_publishers_list(ctx) {
    const { me, refusal } = this.standing(ctx, "publisher.list", `org:${ctx.slug}`);
    if (refusal) return refusal;
    const all = allows(me.role, "publishers.approve") === "yes";
    const rows = this.rows("SELECT * FROM publishers ORDER BY requested_at, key");
    const out = { by_hand: this.publishSettings().by_hand, approvers: this.publisherApprovers() };
    if (all) {
      out.waiting = rows
        .filter((p) => (p.state === "waiting" || p.state === "approved") && this.mayPublish(p.principal))
        .map((p) => ({ principal: p.principal, asked_at: p.requested_at, ...(p.approved_by ? { approved_by: p.approved_by } : {}) }));
      out.publishers = rows.map((p) => this.publisherView(p));
    } else {
      const mine = rows.filter((p) => p.principal === me.principal);
      const signed = mine.find((p) => p.state === "signed");
      const asking = mine.find((p) => p.state === "waiting" || p.state === "approved");
      out.me = signed
        ? { state: "publisher", asked_at: signed.requested_at }
        : asking
          ? { state: "waiting", asked_at: asking.requested_at }
          : { state: "none", asked_at: null };
    }
    return this.ok(out);
  },

  /// POST /orgs/<slug>/publishers/<key or principal>/approve: one click,
  /// by hand. A principal approves every request that person is waiting on.
  op_publisher_approve(ctx) {
    let key = String(ctx.args.key || "").toLowerCase();
    const who = ctx.args.principal ? String(ctx.args.principal).toLowerCase() : null;
    const target = who ? `publisher:${who}` : `release-key:${short(key)}`;
    const { me, refusal } = this.standing(ctx, "publisher.approve", target);
    if (refusal) return refusal;
    if (allows(me.role, "publishers.approve") !== "yes") return this.deny(ctx, "publisher.approve", target, "role-cannot", `only ${ctx.slug}'s owners and admins approve publishers`);
    if (who) {
      const asking = this.rows("SELECT * FROM publishers WHERE principal = ? AND state IN ('waiting','approved') ORDER BY requested_at DESC", who);
      if (!asking.length) return this.deny(ctx, "publisher.approve", target, "not-a-publisher", `${displayOf(who)} is not waiting to publish to ${ctx.slug}`);
      for (const p of asking.slice(1)) this.approveOne(ctx, me, p);
      key = asking[0].key;
    }
    const p = this.publisherRow(key);
    if (!p) return this.deny(ctx, "publisher.approve", target, "not-a-publisher");
    if (!this.mayPublish(p.principal)) return this.deny(ctx, "publisher.approve", target, "not-a-member", `${displayOf(p.principal)} cannot publish to ${ctx.slug} now`);
    this.approveOne(ctx, me, p);
    return this.ok(this.publisherView(this.publisherRow(key)));
  },

  approveOne(ctx, me, p) {
    if (p.state !== "waiting") return;
    this.sql.exec("UPDATE publishers SET state = 'approved', approved_by = ?, approved_at = ? WHERE key = ?", me.principal, ctx.now, p.key);
    this.audit(me.principal, "publisher.approve", `release-key:${short(p.key)}`, "allowed", `${displayOf(p.principal)}, by hand`);
  },

  /// POST /orgs/<slug>/publishers/<key>/delegation: a holder device's
  /// signed slip, read by the validator at the door (`slip`).
  op_publisher_delegation(ctx) {
    const a = ctx.args;
    const key = String(a.key || "").toLowerCase();
    const target = `release-key:${short(key)}`;
    const { me, refusal } = this.ringStanding(ctx, "publisher.delegate", target);
    if (refusal) return refusal;
    const held = this.holderDevice(ctx, me, "publisher.delegate", target, a.by_device);
    if (held.refusal) return held.refusal;
    const p = this.publisherRow(key);
    if (!p || !["waiting", "approved", "signed"].includes(p.state)) return this.deny(ctx, "publisher.delegate", target, "not-a-publisher");
    if (!this.mayPublish(p.principal)) return this.deny(ctx, "publisher.delegate", target, "not-a-member", `${displayOf(p.principal)} cannot publish to ${ctx.slug} now`);
    if (p.state === "waiting" && this.publishSettings().by_hand) return this.deny(ctx, "publisher.delegate", target, "needs-approval");
    const slip = a.slip || {};
    const why =
      slip.root !== held.ring.public_key ? "it is signed by a key that is not the organisation's"
      : slip.key !== key ? "it is for a different release key"
      : slip.namespace !== `${ctx.slug}/*` ? `it covers ${slip.namespace}, not ${ctx.slug}/*`
      : !(Number(slip.expires) > ctx.now) ? "it has run out"
      : Number(slip.not_before) > ctx.now + SKEW ? "it starts in the future"
      : null;
    if (why) return this.deny(ctx, "publisher.delegate", target, "slip-invalid", why);
    if (p.state === "signed") return this.ok(this.publisherView(p));
    this.sql.exec(
      "UPDATE publishers SET state = 'signed', delegation = ?, signed_by = ?, signed_device = ?, signed_at = ?, expires = ? WHERE key = ?",
      JSON.stringify(a.delegation), me.principal, held.dev.device_id, ctx.now, Number(slip.expires), key,
    );
    this.audit(
      me.principal,
      "publisher.delegate",
      target,
      "allowed",
      `automatic: ${displayOf(p.principal)} may publish, signed by ${held.dev.device_id}, until ${day(slip.expires)}`,
    );
    return this.ok(this.publisherView(this.publisherRow(key)));
  },

  /// GET /orgs/<slug>/settings/publishing
  op_publishing_show(ctx) {
    const { me, refusal } = this.standing(ctx, "publishing.view", `org:${ctx.slug}`);
    if (refusal) return refusal;
    return this.ok(this.publishingView(me));
  },

  /// PUT /orgs/<slug>/settings/publishing
  op_publishing_set(ctx) {
    const target = `org:${ctx.slug}`;
    const { me, refusal } = this.standing(ctx, "publishing.set", target);
    if (refusal) return refusal;
    if (allows(me.role, "publishing.settings") !== "yes") return this.deny(ctx, "publishing.set", target, "role-cannot", `only ${ctx.slug}'s owners change how it publishes`);
    const s = this.publishSettings();
    const next = { ...s };
    for (const [arg, field] of [["publishers_by_hand", "by_hand"], ["two_person_from_start", "two_person"]]) {
      if (ctx.args[arg] === undefined || ctx.args[arg] === null) continue;
      if (typeof ctx.args[arg] !== "boolean") return { status: 400, body: { error: "bad-request", message: `${arg} is true or false` } };
      next[field] = ctx.args[arg];
    }
    const wasInForce = this.twoPersonInForce();
    this.sql.exec(
      `INSERT INTO publish_settings (id, by_hand, two_person, set_at, set_by) VALUES (1, ?, ?, ?, ?)
       ON CONFLICT (id) DO UPDATE SET by_hand = excluded.by_hand, two_person = excluded.two_person, set_at = excluded.set_at, set_by = excluded.set_by`,
      next.by_hand ? 1 : 0, next.two_person ? 1 : 0, ctx.now, me.principal,
    );
    if (next.by_hand !== s.by_hand) this.audit(me.principal, "settings.publishers", target, "allowed", next.by_hand ? "approve new publishers by hand" : "approve new publishers automatically");
    if (next.two_person !== s.two_person) this.audit(me.principal, "settings.two-person", target, "allowed", next.two_person ? "every release needs a second person" : "the owner may release alone while nobody else can approve");
    if (!wasInForce && this.twoPersonInForce()) this.audit(me.principal, "review.two-person.on", target, "allowed", "turned on in Settings");
    return this.ok(this.publishingView(me));
  },
};

function install(OrgClass) {
  extendOrg({
    schema: SCHEMA,
    columns: COLUMNS,
    mutations: [
      "signing-init", "signing-grant", "signing-revocations",
      "publisher-ask", "publisher-approve", "publisher-delegation", "publishing-set",
    ],
    view(org, me) {
      org.publishing = this.publishingView(me);
      org.release_rule = this.twoPersonInForce() ? "two-person" : "solo";
    },
  });
  for (const [name, fn] of Object.entries(ops)) {
    if (Object.prototype.hasOwnProperty.call(OrgClass.prototype, name)) throw new Error(`org-signing: ${name} already exists`);
    OrgClass.prototype[name] = fn;
  }
  const P = OrgClass.prototype;
  for (const name of ["op_approve", "op_add_candidate", "op_candidates", "op_releases", "op_remove", "op_set_role", "op_accept", "publishRefusal", "scimRemove", "liveDevices", "myDevice"]) {
    if (typeof P[name] !== "function") throw new Error(`org-signing: ${name} is not installed`);
  }

  // The door refuses a release key from the moment it is due for
  // withdrawal, before any device has signed the withdrawal.
  const refuse = P.publishRefusal;
  P.publishRefusal = function (ctx, target) {
    const sig = ctx.args.signature;
    const chain = sig && sig.chain;
    if (chain && chain.key) {
      const p = this.publisherRow(chain.key);
      if (p && (p.state === "revoke-due" || p.state === "revoked")) {
        const { refusal } = this.standing(ctx, "candidate.publish", target);
        if (refusal) return refusal;
        return this.deny(ctx, "candidate.publish", target, "not-signed-for-this-org", `its release key belonged to ${displayOf(p.principal)}, who can no longer publish to ${ctx.slug}`);
      }
    }
    return refuse.call(this, ctx, target);
  };

  // A candidate's answer says whether its sender may release it now, and
  // who else may approve it.
  const add = P.op_add_candidate;
  P.op_add_candidate = function (ctx) {
    const answer = add.call(this, ctx);
    if ((answer.status === 201 || answer.status === 200) && answer.body && typeof answer.body === "object") {
      answer.body.solo_release = this.soloOpenFor(ctx.principals) && answer.body.state === "waiting-for-approval";
      answer.body.approvers = this.approvers(ctx.principals).map(displayOf);
    }
    return answer;
  };

  // Solo release: the uploader releases their own upload while nobody else
  // may approve. Everything else about approving (capabilities to confirm,
  // a withdrawn digest, the current release) is the same command's.
  const approve = P.op_approve;
  P.op_approve = function (ctx) {
    const digest = String(ctx.args.digest || "");
    const c = this.row("SELECT * FROM candidates WHERE digest = ?", digest);
    const own = c && ctx.principals.includes(c.uploaded_by);
    if (!own || c.state !== "waiting-for-approval" || !this.soloOpenFor(ctx.principals)) return approve.call(this, ctx);
    // The approval's audit event says "solo".
    const answer = approve.call(this, { ...ctx, principals: ctx.principals.filter((p) => p !== c.uploaded_by), approveReason: "solo" });
    const after = this.row("SELECT state FROM candidates WHERE digest = ?", digest);
    if (answer.status === 200 && after && after.state === "released") {
      this.sql.exec("UPDATE candidates SET solo = 1 WHERE digest = ?", digest);
      if (answer.body && answer.body.release) answer.body.release.solo = true;
    }
    return answer;
  };

  for (const name of ["op_candidates", "op_releases"]) {
    const list = P[name];
    P[name] = function (ctx) {
      const answer = list.call(this, ctx);
      if (answer.status !== 200 || !Array.isArray(answer.body)) return answer;
      answer.body = answer.body.map((r) => {
        const c = this.row("SELECT solo FROM candidates WHERE digest = ?", r.digest);
        return c && c.solo ? { ...r, solo: true, released_note: SOLO_NOTE } : { ...r, solo: false };
      });
      return answer;
    };
  }

  // The two-person rule turns on by itself when a second person who may
  // approve arrives: said once, in the audit.
  for (const name of ["op_accept", "op_set_role"]) {
    const inner = P[name];
    P[name] = function (ctx) {
      const before = this.org() ? this.twoPersonInForce() : true;
      const who = name === "op_set_role" ? this.targetOf(ctx) : null;
      const roleBefore = who ? (this.member(who) || {}).role : null;
      const answer = inner.call(this, ctx);
      if (answer.status === 200) {
        if (!before && this.twoPersonInForce()) {
          const m = answer.body && answer.body.membership;
          this.audit(ctx.actor, "review.two-person.on", `org:${ctx.slug}`, "allowed", `${m ? m.display : "someone"} can approve now`);
        }
        // Someone who may no longer publish stops publishing.
        if (who && roleBefore && allows(roleBefore, "candidate.publish") === "yes" && !this.mayPublish(who)) {
          this.endPublisher(ctx.actor, who, ctx.now, "no longer publishes");
        }
        // Someone who is no longer an owner or admin stops holding the ring.
        const now = who ? this.member(who) : null;
        if (now && roleBefore && allows(roleBefore, "signing.hold") === "yes" && allows(now.role, "signing.hold") !== "yes") {
          for (const e of this.rows("SELECT e.device_id FROM sign_envelopes e JOIN key_devices d ON d.device_id = e.device_id WHERE d.principal = ?", who)) {
            this.sql.exec("DELETE FROM sign_envelopes WHERE device_id = ?", e.device_id);
            this.audit(ctx.actor, "signing.device.remove", `device:${e.device_id}`, "allowed", `${displayOf(who)} is no longer an owner or admin`);
          }
        }
      }
      return answer;
    };
  }

  const remove = P.op_remove;
  P.op_remove = function (ctx) {
    const who = this.targetOf(ctx);
    const answer = remove.call(this, ctx);
    if (answer.status === 204 && who) this.endMember(ctx.actor, who, ctx.now, "left the organisation");
    return answer;
  };

  // A SCIM deactivation is a removal too: the keyring's devices end (which
  // org-keys.js cannot wrap itself -- scimRemove is installed after it),
  // and so does publishing.
  const scimRemove = P.scimRemove;
  P.scimRemove = function (ctx, m, why) {
    const refusal = scimRemove.call(this, ctx, m, why);
    if (!refusal) {
      this.keysEndMember("scim", m.principal, ctx.now);
      this.endMember("scim", m.principal, ctx.now, "deactivated in the directory");
    }
    return refusal;
  };

  /// What leaving does to signing: release keys stop, and an owner's or
  /// admin's devices lose their envelopes of the ring, so it is never
  /// passed to them again. (What a device already opened, it keeps: the
  /// audit says so, and replacing the organisation's signing key is a
  /// deliberate step, not an automatic one.)
  P.endMember = function (actor, who, now, why) {
    this.endPublisher(actor, who, now, why);
    const held = this.rows(
      "SELECT e.device_id FROM sign_envelopes e JOIN key_devices d ON d.device_id = e.device_id WHERE d.principal = ?",
      who,
    );
    for (const e of held) {
      this.sql.exec("DELETE FROM sign_envelopes WHERE device_id = ?", e.device_id);
      this.audit(actor, "signing.device.remove", `device:${e.device_id}`, "allowed", `${displayOf(who)} ${why}; that device held the signing key`);
    }
  };
}

install(Org);

/// /orgs/<slug>/{signing,publishers,settings/publishing}... A Response, or
/// null. `validate(name, bytes)` is the Rust validator's export.
export async function orgSigningRoute(h) {
  const { url, rest, method, askRaw, mutateRaw, orgAnswer, readBody, idempotencyOf, request, validate } = h;
  const bad = (message) => orgAnswer({ status: 400, body: { error: "bad-request", message } });
  if (rest[0] === "settings" && rest[1] === "publishing" && rest.length === 2) {
    if (method === "GET") return orgAnswer(await askRaw("publishing-show"));
    if (method === "PUT") {
      return orgAnswer(await mutateRaw("publishing-set", (b) => ({ publishers_by_hand: b.publishers_by_hand, two_person_from_start: b.two_person_from_start })));
    }
    return null;
  }
  if (rest[0] === "signing") {
    if (rest.length === 1 && method === "GET") return orgAnswer(await askRaw("signing-view", { device: url.searchParams.get("device") || "" }));
    if (rest.length === 2 && rest[1] === "init" && method === "POST") return orgAnswer(await mutateRaw("signing-init", (b) => b));
    if (rest.length === 2 && rest[1] === "grants" && method === "POST") return orgAnswer(await mutateRaw("signing-grant", (b) => b));
    if (rest.length === 2 && rest[1] === "recovery" && method === "GET") return orgAnswer(await askRaw("signing-recovery"));
    if (rest.length === 2 && rest[1] === "revocations" && method === "POST") {
      const body = await readBody();
      if (body.tooLarge || body.bad) return bad("the body is JSON, under 64 KiB");
      // Checked by the Rust validator: signed by the root it names.
      const list = validate("krate_revocations", new TextEncoder().encode(JSON.stringify(body.value.list || null)));
      if (!list.ok) return orgAnswer({ status: 422, body: { error: "slip-invalid", message: `that withdrawal list does not hold: ${list.problem}` } });
      const idempotency = await idempotencyOf(request, url.pathname, body.text);
      return orgAnswer(await askRaw("signing-revocations", { root: list.root, revocations: list.revocations, by_device: body.value.by_device }, idempotency));
    }
    return null;
  }
  // The page's two switches (stable shapes for the dashboard).
  if (rest[0] === "release-rule" && rest.length === 1 && method === "PUT") {
    return orgAnswer(await mutateRaw("publishing-set", (b) => ({ two_person_from_start: b.two_person })));
  }
  if (rest[0] !== "publishers") return null;
  if (rest.length === 1 && method === "GET") return orgAnswer(await askRaw("publishers-list"));
  if (rest.length === 1 && method === "POST") return orgAnswer(await mutateRaw("publisher-ask", (b) => ({ public_key: b.public_key, name: b.name })));
  if (rest.length === 2 && rest[1] === "by-hand" && method === "PUT") {
    return orgAnswer(await mutateRaw("publishing-set", (b) => ({ publishers_by_hand: b.by_hand })));
  }
  const key = String(rest[1] || "").toLowerCase();
  // A person, named by principal (`github:carol`), for the approve click.
  if (!HEX64.test(key) && rest.length === 3 && rest[2] === "approve" && method === "POST" && /^(github|email):/.test(key)) {
    return orgAnswer(await mutateRaw("publisher-approve", () => ({ principal: key })));
  }
  if (!HEX64.test(key)) return orgAnswer({ status: 404, body: { error: "not-a-publisher", message: "a release key is named by its 64-hex public key" } });
  if (rest.length === 2 && method === "GET") return orgAnswer(await askRaw("publisher-show", { key }));
  if (rest.length === 3 && rest[2] === "approve" && method === "POST") return orgAnswer(await mutateRaw("publisher-approve", () => ({ key })));
  if (rest.length === 3 && rest[2] === "delegation" && method === "POST") {
    const body = await readBody();
    if (body.tooLarge || body.bad) return bad("the body is JSON, under 64 KiB");
    // Checked by the Rust validator: the root's signature over it, and that
    // it holds now. The store then asks whether that root is this
    // organisation's, and whether this person may publish.
    const at = Math.floor(Date.now() / 1000);
    const slip = validate("krate_delegation", new TextEncoder().encode(JSON.stringify({ delegation: body.value.delegation || null, at })));
    if (!slip.ok) return orgAnswer({ status: 422, body: { error: "slip-invalid", message: `that permission to publish does not hold: ${slip.problem}` } });
    const idempotency = await idempotencyOf(request, url.pathname, body.text);
    return orgAnswer(
      await askRaw(
        "publisher-delegation",
        { key, by_device: body.value.by_device, delegation: body.value.delegation, slip: { root: slip.root, key: slip.key, namespace: slip.namespace, not_before: slip.not_before, expires: slip.expires } },
        idempotency,
      ),
    );
  }
  return null;
}
