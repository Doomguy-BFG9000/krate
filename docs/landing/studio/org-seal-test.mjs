// Sealing to an organisation's key in the browser (F-316), against the
// cross-language vector crates/orgkeys' tests read too
// (crates/orgkeys/tests/vectors/org-seal.json): the page's shipped
// orgseal.js on the vendored hpke-js makes exactly the bytes crates/orgkeys
// makes for the same ephemeral key, opens what crates/orgkeys sealed with a
// random one, and binds the context.
//
//   node docs/landing/studio/org-seal-test.mjs
import { readFileSync } from "node:fs";
import { createHash, webcrypto } from "node:crypto";
import vm from "node:vm";

let bad = 0;
const check = (ok, name) => {
  if (!ok) bad++;
  console.log(`${ok ? "ok  " : "FAIL"} ${name}`);
};

const VENDOR = "docs/landing/org/vendor/hpke-js-1.9.0.js";
const vendored = readFileSync(VENDOR, "utf8");
const vector = JSON.parse(readFileSync("crates/orgkeys/tests/vectors/org-seal.json", "utf8"));

// The vendored file is the bundle its header names, unchanged.
const [, header, body] = vendored.match(/^(\/\*[\s\S]*?\*\/\n)([\s\S]*)$/) || [];
const named = header && (header.match(/sha256[\s*]+([0-9a-f]{64})/) || [])[1];
check(named && createHash("sha256").update(body).digest("hex") === named, "the vendored hpke-js is the bundle its header names, byte for byte");
check(/@hpke\/core\s+1\.9\.0/.test(header) && /@hpke\/dhkem-x25519\s+1\.8\.0/.test(header) && /@hpke\/chacha20poly1305\s+1\.8\.0/.test(header), "and says its exact versions");

// A window like the page's: the browser's WebCrypto, text codecs, base64.
const win = { crypto: webcrypto, TextEncoder, TextDecoder, btoa, atob, console };
win.window = win;
win.globalThis = win;
vm.createContext(win);
vm.runInContext(vendored, win);
vm.runInContext(readFileSync("docs/landing/org/orgseal.js", "utf8"), win);
const S = win.KrateOrgSeal;

check(S.SUITE === vector.suite, "the page names the suite the vector does");
const key = await S.deriveOrgKey(vector.org_ikm);
check(key.publicHex === vector.org_public, "the organisation key derives to the vector's public key");

for (const c of vector.cases) {
  const made = await S.sealForOrg(vector.org_public, c.context, c.plaintext, c.ikm_e);
  check(made === c.sealed, `the page makes crates/orgkeys' exact bytes for ${JSON.stringify(c.context.split("\n")[0])}`);
  check((await S.openForOrg(key, c.context, c.sealed)) === c.plaintext, "and opens them");
  let refused = false;
  try {
    await S.openForOrg(key, c.context.replace("acme/", "globex/"), c.sealed);
  } catch (_) {
    refused = true;
  }
  check(refused, "another context does not open them");
}
check(
  S.connectionContext("acme", "finance-api", "Authorization", "https://api.acme.example/finance") === vector.cases[0].context &&
    S.connectionContext("acme", "payroll", "X-Api-Key", "http://127.0.0.1:9000/payroll") === vector.cases[1].context,
  "the page builds a connection's context as crates/orgkeys does",
);
check((await S.openForOrg(key, vector.rust_random.context, vector.rust_random.sealed)) === vector.rust_random.plaintext, "what crates/orgkeys sealed with a random ephemeral key opens in the page");
const one = await S.sealForOrg(vector.org_public, vector.page_random.context, vector.page_random.plaintext);
const two = await S.sealForOrg(vector.org_public, vector.page_random.context, vector.page_random.plaintext);
check(one !== two && !one.includes("sk-vector") && !atob(one).includes("sk-vector"), "a real seal is fresh each time and carries no plaintext");
check((await S.openForOrg(key, vector.page_random.context, one)) === vector.page_random.plaintext, "and opens");

if (bad) {
  console.log(`\n${bad} failed`);
  process.exit(1);
}
console.log("\nall passed");
