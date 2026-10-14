// The doors for an organisation's lifecycle (IC-948, Addendum 3 D, shapes
// from Addendum 4): close with approval and a window, the standard export,
// recovery of a lost owner by Krate support, and the scheduled sweep that
// deletes a closed organisation's files once its export window has passed.
// The store (org-lifecycle-store.js) decides; these routes read and write
// what the store cannot: R2 bytes, the zip, and mail.
//
// `h` is what index.js's organisation router hands every module: the
// request, env, slug, the caller's principals, and its helpers.

import { mailRecovery } from "./org-mail.js";
import { chainOk } from "./org.js";

/// The KV index of organisations with a close in progress, so the nightly
/// sweep can find them. Display and scheduling only: the store decides.
const closingKey = (slug) => `org-closing:${slug}`;

const exportKey = (slug, id) => `org-exports/${slug}/${id}.zip`;

export async function orgLifecycleRoute(h) {
  const { method, rest } = h;
  if (rest[0] === "close") {
    if (rest.length === 1 && method === "POST") {
      const answer = await h.mutateRaw("close", (b) => ({ confirm_slug: b.confirm_slug }));
      if (answer.status < 300 && answer.body.close && answer.body.close.state === "scheduled") await h.env.APPS.put(closingKey(h.slug), "1").catch(() => {});
      return h.orgAnswer(answer);
    }
    if (rest.length === 3 && rest[2] === "approve" && method === "POST") {
      const answer = await h.mutateRaw("close-approve", (b) => ({ id: rest[1], statement_digest: b.statement_digest }));
      if (answer.status < 300) await h.env.APPS.put(closingKey(h.slug), "1").catch(() => {});
      return h.orgAnswer(answer);
    }
    if (rest.length === 2 && method === "DELETE") {
      const answer = await h.mutateRaw("close-cancel", () => ({ id: rest[1] }));
      if (answer.status === 204) await h.env.APPS.delete(closingKey(h.slug)).catch(() => {});
      return h.orgAnswer(answer);
    }
    return null;
  }
  if (rest[0] === "export" && rest.length === 1 && method === "POST") return buildExport(h);
  if (rest[0] === "exports") {
    if (rest.length === 1 && method === "GET") return h.orgAnswer(await h.askRaw("exports"));
    if (rest.length === 2 && method === "GET") {
      const answer = await h.askRaw("export-get", { id: rest[1] });
      if (answer.status !== 200) return h.orgAnswer(answer);
      const object = await h.env.BUNDLES.get(exportKey(h.slug, rest[1]));
      if (!object) return h.json({ error: "export-not-ready", message: "that package is gone -- build a new export" }, 409);
      return new Response(object.body, {
        headers: {
          "content-type": "application/zip",
          "content-disposition": `attachment; filename="${h.slug}-export-${rest[1]}.zip"`,
          "content-length": String(object.size),
          "cache-control": "private, no-store",
        },
      });
    }
    return null;
  }
  if (rest[0] === "recover") {
    if (rest.length === 1 && method === "POST") return startRecovery(h);
    if (rest.length === 3 && rest[2] === "object" && method === "POST") {
      return h.orgAnswer(await h.mutateRaw("recover-object", () => ({ id: rest[1] })));
    }
    return null;
  }
  return null;
}

// ------------------------------------------------------------- recovery

/// POST /orgs/<slug>/recover {principal, reason}: a Krate admin only --
/// GitHub-vouched and in KRATE_ADMINS. Nobody gains anything now; the
/// store makes the principal an owner after 72 hours unless an owner or
/// admin objects, and everyone it can reach is mailed.
async function startRecovery(h) {
  const admin = await h.isAdmin(h.request, h.env);
  if (!admin) {
    return h.json({ error: "role-cannot", message: "only Krate support can recover an organisation's ownership" }, 403);
  }
  const body = await h.readBody();
  if (body.bad || body.tooLarge) return h.json({ error: "bad-request", message: "the body is JSON, under 64 KiB" }, 400);
  const answer = await h.orgAsk(h.env, h.slug, {
    op: "recover-start",
    args: { principal: body.value.principal, reason: body.value.reason, admin: `github:${String(admin.login).toLowerCase()}` },
  });
  if (answer.status !== 201) return h.orgAnswer(answer);
  const mail = await mailRecovery(h.env, h.smtpSend, {
    slug: h.slug,
    orgName: answer.body.org_name,
    recovery: answer.body.recovery,
    notify: answer.body.notify,
    site: h.site,
  });
  return h.json({ recovery: answer.body.recovery, mailed: mail.mailed, not_mailed: mail.not_mailed }, 201);
}

// --------------------------------------------------------------- export

/// The most release bytes one export carries (ORG_EXPORT_MAX_BYTES, 64
/// MiB): a Worker holds the zip in memory. A release past it is listed in
/// the manifest's exclusions with its digest and size, never dropped
/// silently.
const maxExportBytes = (env) => {
  const n = Number(env.ORG_EXPORT_MAX_BYTES);
  return Number.isFinite(n) && n > 0 ? n : 64 * 1024 * 1024;
};

export const EXPORT_FORMAT = "krate.org-export.v1";

/// What the standard export never carries, said in the manifest (R28).
export const EXCLUSIONS = [
  { class: "signing keys", why: "private keys never reach Krate; the organisation's root public key is not part of the export either" },
  { class: "tokens and sessions", why: "sign-in tokens and sessions are credentials, never exported" },
  { class: "secrets", why: "no secret held for the organisation (sign-in client secrets included) is exported" },
  { class: "invitation codes", why: "an open invitation's code is a credential; who was invited is in the audit" },
  { class: "idempotency records", why: "request bookkeeping, not organisation data" },
  { class: "billing identifiers", why: "Stripe customer and subscription ids stay with Krate; invoices come from Stripe" },
];

async function buildExport(h) {
  const begun = await h.askRaw("export-begin");
  if (begun.status !== 202) return h.orgAnswer(begun);
  const { id } = begun.body.export;
  const data = begun.body.data;
  let done;
  try {
    const zip = await exportZip(h.env, h.slug, id, data);
    const sha = await sha256Hex(zip);
    await h.env.BUNDLES.put(exportKey(h.slug, id), zip, {
      httpMetadata: { contentType: "application/zip" },
      customMetadata: { org: h.slug, export: id },
    });
    done = await h.orgAsk(h.env, h.slug, { op: "export-done", args: { id, state: "ready", size: zip.length, sha256: sha } });
  } catch (err) {
    done = await h.orgAsk(h.env, h.slug, {
      op: "export-done",
      args: { id, state: "failed", problem: String((err && err.message) || err).slice(0, 200) },
    });
  }
  return h.json({ export: done.body.export }, 202);
}

/// The zip: export-manifest.json first, then each data class as JSON and
/// the releases' own .krate files.
export async function exportZip(env, slug, id, data) {
  const enc = new TextEncoder();
  const files = [];
  const classes = [];
  const add = async (cls, path, bytes) => {
    files.push({ path, bytes });
    let entry = classes.find((c) => c.class === cls);
    if (!entry) classes.push((entry = { class: cls, files: [] }));
    entry.files.push({ path, size: bytes.length, sha256: await sha256Hex(bytes) });
  };
  const asJson = (v) => enc.encode(JSON.stringify(v, null, 2) + "\n");
  await add("organisation", "org.json", asJson(data.org));
  await add("members and roles", "members.json", asJson(data.members));
  await add("candidates", "candidates.json", asJson(data.candidates));
  const chain = await chainOk(data.audit);
  await add("audit", "audit.json", asJson({ chain_ok: chain, events: data.audit }));

  const excluded = [...EXCLUSIONS];
  let budget = maxExportBytes(env);
  for (const c of data.candidates.filter((x) => x.state === "released")) {
    const object = await releaseObject(env, c.digest);
    const name = `releases/${String(c.name || "app").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "app"}-${String(c.version || "").replace(/[^0-9A-Za-z.]+/g, "-")}-${c.digest.slice(0, 12)}.krate`;
    if (!object) {
      excluded.push({ class: "release file", digest: c.digest, why: "the bytes were not found in the store" });
      continue;
    }
    if (object.size > budget) {
      excluded.push({ class: "release file", digest: c.digest, size: object.size, why: "larger than one export carries; download it as a release while the organisation is open" });
      try {
        await object.body?.cancel?.();
      } catch (_) {}
      continue;
    }
    const bytes = new Uint8Array(await new Response(object.body).arrayBuffer());
    budget -= bytes.length;
    await add("releases", name, bytes);
  }
  const manifest = {
    format: EXPORT_FORMAT,
    export_id: id,
    org: data.org.slug,
    generated_at: Math.floor(Date.now() / 1000),
    classes,
    audit_chain_ok: chain,
    excluded,
  };
  return zipStore([{ path: "export-manifest.json", bytes: asJson(manifest) }, ...files]);
}

async function releaseObject(env, digest) {
  let object = await env.BUNDLES.get(digest);
  if (!object) {
    const blob = await env.APPS.get(`bigkey:${digest}`).catch(() => null);
    if (blob) object = await env.BUNDLES.get(blob);
  }
  return object;
}

// ------------------------------------------------------------- the sweep

/// The nightly sweep: for every organisation with a close in progress, let
/// its store apply what is due, and once a closed organisation's export
/// window has passed, delete its release bytes and export packages and let
/// the store record what was deleted.
export async function sweepOrgs(env, orgAsk) {
  if (!env.ORGS) return [];
  let listed;
  try {
    listed = await env.APPS.list({ prefix: "org-closing:" });
  } catch (_) {
    return [];
  }
  const out = [];
  for (const { name } of listed.keys || []) {
    const slug = name.slice("org-closing:".length);
    const due = await orgAsk(env, slug, { op: "sweep" });
    if (!due.body || !due.body.due) continue;
    const deleted = [];
    for (const { digest } of due.body.digests) {
      const head = await env.BUNDLES.head(digest);
      if (head && head.customMetadata && head.customMetadata.org === slug) {
        await env.BUNDLES.delete(digest);
        deleted.push(digest);
      }
      const blob = await env.APPS.get(`bigkey:${digest}`).catch(() => null);
      if (blob) {
        const bigHead = await env.BUNDLES.head(blob);
        if (bigHead && bigHead.customMetadata && bigHead.customMetadata.org === slug) {
          await env.BUNDLES.delete(blob);
          await env.APPS.delete(`bigkey:${digest}`);
          deleted.push(digest);
        }
      }
    }
    for (const id of due.body.exports) {
      await env.BUNDLES.delete(exportKey(slug, id));
      deleted.push(`export:${id}`);
    }
    await orgAsk(env, slug, { op: "swept", args: { deleted } });
    await env.APPS.delete(name);
    out.push({ slug, deleted });
  }
  return out;
}

// ------------------------------------------------------------ zip, sha

async function sha256Hex(bytes) {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

const CRC = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(bytes) {
  let c = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) c = CRC[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/// A zip of stored (uncompressed) entries: the .krate files inside are
/// already compressed, and this is small enough to be plainly right.
export function zipStore(entries) {
  const enc = new TextEncoder();
  const chunks = [];
  const central = [];
  let offset = 0;
  for (const { path, bytes } of entries) {
    const name = enc.encode(path);
    const crc = crc32(bytes);
    const local = new DataView(new ArrayBuffer(30));
    local.setUint32(0, 0x04034b50, true);
    local.setUint16(4, 20, true);
    local.setUint16(6, 0x0800, true); // names are UTF-8
    local.setUint16(8, 0, true); // stored
    local.setUint32(14, crc, true);
    local.setUint32(18, bytes.length, true);
    local.setUint32(22, bytes.length, true);
    local.setUint16(26, name.length, true);
    chunks.push(new Uint8Array(local.buffer), name, bytes);
    const cd = new DataView(new ArrayBuffer(46));
    cd.setUint32(0, 0x02014b50, true);
    cd.setUint16(4, 20, true);
    cd.setUint16(6, 20, true);
    cd.setUint16(8, 0x0800, true);
    cd.setUint32(16, crc, true);
    cd.setUint32(20, bytes.length, true);
    cd.setUint32(24, bytes.length, true);
    cd.setUint16(28, name.length, true);
    cd.setUint32(42, offset, true);
    central.push(new Uint8Array(cd.buffer), name);
    offset += 30 + name.length + bytes.length;
  }
  const cdSize = central.reduce((n, c) => n + c.length, 0);
  const end = new DataView(new ArrayBuffer(22));
  end.setUint32(0, 0x06054b50, true);
  end.setUint16(8, entries.length, true);
  end.setUint16(10, entries.length, true);
  end.setUint32(12, cdSize, true);
  end.setUint32(16, offset, true);
  const all = [...chunks, ...central, new Uint8Array(end.buffer)];
  const out = new Uint8Array(all.reduce((n, c) => n + c.length, 0));
  let at = 0;
  for (const c of all) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}
