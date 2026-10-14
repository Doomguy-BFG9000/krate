/* Real sealing for the connection tests (F-316): the dashboard's own
 * orgseal.js on the vendored hpke-js, the code that seals a credential in an
 * admin's browser, so what these tests send the hub is what a browser
 * sends. The hub never opens any of it; the tests do, with the
 * organisation key, to prove what it hands out is the blob that came. */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { randomBytes, webcrypto } from "node:crypto";
import vm from "node:vm";

const landing = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "docs", "landing", "org");
const win = { crypto: webcrypto, TextEncoder, TextDecoder, btoa, atob };
win.window = win;
win.globalThis = win;
vm.createContext(win);
vm.runInContext(readFileSync(join(landing, "vendor", "hpke-js-1.9.0.js"), "utf8"), win);
vm.runInContext(readFileSync(join(landing, "orgseal.js"), "utf8"), win);
const S = win.KrateOrgSeal;

/// A fresh organisation key: `{pair, publicHex}`.
export const orgKey = () => S.deriveOrgKey(randomBytes(32).toString("hex"));

export const context = (name, header, base, org = "acme") => S.connectionContext(org, name, header, base);

/// The `credential` a page sends for `value`, sealed to `key` at version `v`.
export async function sealed(key, value, { name = "finance-api", header = "Authorization", base, v = 1, kind = "header" } = {}) {
  return { kind, header, v, sealed: await S.sealForOrg(key.publicHex, context(name, header, base), value) };
}

/// Open a handed-out credential with `key`; null when it does not open.
export async function opened(key, credential, { name = "finance-api", base } = {}) {
  try {
    return await S.openForOrg(key, context(name, credential.header, base), credential.sealed);
  } catch (_) {
    return null;
  }
}
