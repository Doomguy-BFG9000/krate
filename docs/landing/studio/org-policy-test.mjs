// The IT pack krate.tech makes in the browser (docs/landing/kit/policy.js)
// is scripts/build-policy-templates.py's, byte for byte (IC-958 B4).
//
// Runs both on the same inputs -- the Python with --org-pack, the port in
// this process -- and requires the same bytes for every file; and requires
// the port to rebuild the published samples in docs/landing/deploy exactly,
// so a change to either generator that the other does not follow fails here.
//
//   node docs/landing/studio/org-policy-test.mjs     (from the repo root)
import { readFileSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import vm from "node:vm";
import { webcrypto } from "node:crypto";

let bad = 0;
const check = (ok, name) => { if (!ok) bad++; console.log(`${ok ? "ok  " : "FAIL"} ${name}`); };

const ctx = { TextEncoder, Uint8Array, Promise, crypto: webcrypto, String, Number, Array, Object, JSON, parseInt, TypeError };
ctx.globalThis = ctx;
vm.createContext(ctx);
vm.runInContext(readFileSync("docs/landing/kit/policy.js", "utf8"), ctx);
const P = ctx.KratePolicy;
const table = JSON.parse(readFileSync("docs/landing/deploy/policy-keys.json", "utf8"));
const same = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);
const firstDiff = (a, b) => { const s = Buffer.from(a).toString("latin1"), t = Buffer.from(b).toString("latin1"); let i = 0; while (i < s.length && s[i] === t[i]) i++; return JSON.stringify(s.slice(Math.max(0, i - 40), i + 40)) + " vs " + JSON.stringify(t.slice(Math.max(0, i - 40), i + 40)); };

// 1. The published samples, rebuilt by the port.
const samples = await P.samples(table, webcrypto.subtle);
for (const [name, bytes] of Object.entries(samples)) {
  const want = new Uint8Array(readFileSync(join("docs/landing/deploy", name)));
  const got = Array.from(bytes), exp = Array.from(want);
  check(same(got, exp), `the port rebuilds deploy/${name} exactly` + (same(got, exp) ? "" : `: ${firstDiff(bytes, want)}`));
}

// 2. One organisation's pack, by both, for inputs that test the escaping:
// an ampersand and quotes in the name, a dev hub, two roots.
const cases = [
  { name: "Acme", slug: "acme", hub: "https://hub.krate.tech", roots: [{ name: "Acme", key: "3815b24cb41737ce34bfb6e3d577b8d2c7471cc0dcb9e7998768883c048337d1" }] },
  { name: 'Smith & "Sons" <Ltd>', slug: "smith-sons", hub: "http://127.0.0.1:8891", roots: [{ name: "Smith & Sons", key: "ab".repeat(32) }, { name: "Old root", key: "cd".repeat(32) }] },
  { name: "Café Zürich", slug: "cafe-zurich", hub: "https://hub.krate.tech", roots: [{ name: "Café Zürich", key: "ef".repeat(32) }] },
];
for (const c of cases) {
  const dir = mkdtempSync(join(tmpdir(), "krate-pack-"));
  try {
    execFileSync("python3", ["scripts/build-policy-templates.py", "--org-pack", dir, "--name", c.name, "--slug", c.slug, "--hub", c.hub, ...c.roots.flatMap((r) => ["--root", `${r.name}=${r.key}`])]);
    const js = await P.pack(table, c, webcrypto.subtle);
    const pyFiles = readdirSync(dir).sort();
    check(pyFiles.join() === Object.keys(js).sort().join(), `${c.slug}: the same three files (${pyFiles.join(", ")})`);
    for (const f of pyFiles) {
      const py = new Uint8Array(readFileSync(join(dir, f)));
      const ok = same(Array.from(js[f] || []), Array.from(py));
      check(ok, `${c.slug}: ${f} is byte-identical` + (ok ? "" : `: ${firstDiff(js[f] || new Uint8Array(), py)}`));
    }
    const toml = Buffer.from(js["policy.toml"]).toString("utf8");
    check(/\n\[org\]\n(#.*\n)*slug = "/.test(toml) && toml.includes(`url = "${c.hub}"`) && toml.includes(c.roots[0].key), `${c.slug}: the policy names the organisation, its hub and its root`);
    check(!/camera\.capture"\]\n/.test(toml.replace(/^#.*$/gm, "")) && !/REPLACE-WITH/.test(toml), `${c.slug}: nothing of the sample (blocked camera, placeholder key) comes along`);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

console.log(bad ? `\n${bad} failed` : "\nall passed");
process.exit(bad ? 1 : 0);
