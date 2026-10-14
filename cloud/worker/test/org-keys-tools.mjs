/* What the keyring and team-data tests share: fake device keys, envelopes
 * and sealed rows. The hub cannot tell these from real ones -- it never
 * opens any of them -- which is the point: the real sealing is
 * crates/orgkeys, proved there and through the binary. */
import { createHash, randomBytes } from "node:crypto";
import { call } from "./org-tools.mjs";

export const hexOf = (bytes) => Buffer.from(bytes).toString("hex");
export const b64 = (n) => randomBytes(n).toString("base64");

/// A device: a random 32-byte "public key" and the id the hub derives.
export function device(seed = randomBytes(32)) {
  const pub = hexOf(seed);
  const id = createHash("sha256").update(pub).digest("hex").slice(0, 16);
  return { pub, id };
}

export const opaque = (...parts) => createHash("sha256").update(parts.join("\u0000")).digest("hex");

export const recovery = () => ({
  public_key: hexOf(randomBytes(32)),
  params: { alg: "argon2id", salt: hexOf(randomBytes(16)), m_kib: 19456, t: 2, p: 1 },
  blob: b64(200),
});

/// Register `dev` for `who`.
export async function register(env, who, dev, name = "laptop") {
  return call(env, who, "PUT", `/orgs/acme/keys/devices/${dev.id}`, { json: { public_key: dev.pub, name } });
}

/// Alice's device holds version 1; answers her device.
export async function initKey(env) {
  const alice = device();
  const r = await register(env, "alice", alice, "Alice's MacBook");
  if (r.status !== 200) throw new Error(JSON.stringify(r.body));
  const init = await call(env, "alice", "POST", "/orgs/acme/keys/init", {
    json: { device_id: alice.id, public_key: hexOf(randomBytes(32)), envelope: b64(300), recovery: recovery() },
  });
  if (init.status !== 201) throw new Error(JSON.stringify(init.body));
  return alice;
}

/// `who` registers a device and a holder device grants it the key.
export async function memberDevice(env, who, granter, granterWho = "alice") {
  const dev = device();
  await register(env, who, dev);
  const view = await call(env, granterWho, "GET", `/orgs/acme/keys?device=${granter.id}`);
  const r = await call(env, granterWho, "POST", "/orgs/acme/keys/grants", {
    json: { device_id: dev.id, version: view.body.current.version, blob: b64(300), by_device: granter.id },
  });
  if (r.status !== 200) throw new Error(JSON.stringify(r.body));
  return dev;
}

/// A sealed row as a device would send it, for (c, i, f) of app data.
export function sealedRow(c, i, f, s, { kind = f === "" ? "t" : "v", ver = 1, bytes = 60 } = {}) {
  return { r: opaque("r", c, i), k: opaque("f", c, i, f), kind, s, ver, ct: b64(bytes) };
}
