// A .krate for the proofs: an app's manifest (its entry pointed at the
// bundle's code.wasm) and its built component, zipped the way the hub's
// door reads an archive. `krate pack` makes the same two entries; this
// avoids building the whole CLI for a proof run.
//
//   node pack.mjs <manifest.toml> <component.wasm> <out.krate> [capability | optional:<capability>]...
import { readFileSync, writeFileSync } from 'node:fs';
import { deflateRawSync, crc32 } from 'node:zlib';

const [manifestPath, componentPath, outPath, ...extra] = process.argv.slice(2);
let manifest = readFileSync(manifestPath, 'utf8').replace(/^entry\s*=.*$/m, 'entry = "code.wasm"');
for (const cap of extra) {
  if (cap.startsWith('optional:')) {
    // A declared capability made optional: the app runs without it.
    const name = cap.slice('optional:'.length);
    manifest = manifest
      .split('[[capabilities]]')
      .map((block) => (block.includes(`cap = "${name}"`) ? block.replace('required = true', 'required = false') : block))
      .join('[[capabilities]]');
    continue;
  }
  manifest += `\n[[capabilities]]\ncap = "${cap}"\nrationale = "Proof: a capability a tab cannot give"\nrequired = true\n`;
}
const entries = [
  { name: 'manifest.toml', data: new TextEncoder().encode(manifest) },
  { name: 'code.wasm', data: new Uint8Array(readFileSync(componentPath)) },
];

const le16 = (n) => [n & 0xff, (n >> 8) & 0xff];
const le32 = (n) => [n & 0xff, (n >> 8) & 0xff, (n >> 16) & 0xff, (n >>> 24) & 0xff];
const parts = [];
const central = [];
let offset = 0;
for (const e of entries) {
  const name = new TextEncoder().encode(e.name);
  const stored = new Uint8Array(deflateRawSync(e.data, { level: 9 }));
  const crc = crc32(e.data) >>> 0;
  const local = new Uint8Array([
    ...le32(0x04034b50), ...le16(20), ...le16(0), ...le16(8), ...le16(0), ...le16(0x21),
    ...le32(crc), ...le32(stored.length), ...le32(e.data.length), ...le16(name.length), ...le16(0), ...name,
  ]);
  central.push(new Uint8Array([
    ...le32(0x02014b50), ...le16(20), ...le16(20), ...le16(0), ...le16(8), ...le16(0), ...le16(0x21),
    ...le32(crc), ...le32(stored.length), ...le32(e.data.length), ...le16(name.length), ...le16(0), ...le16(0),
    ...le16(0), ...le16(0), ...le32(0), ...le32(offset), ...name,
  ]));
  parts.push(local, stored);
  offset += local.length + stored.length;
}
const cdSize = central.reduce((n, c) => n + c.length, 0);
const eocd = new Uint8Array([
  ...le32(0x06054b50), ...le16(0), ...le16(0), ...le16(entries.length), ...le16(entries.length),
  ...le32(cdSize), ...le32(offset), ...le16(0),
]);
const all = Buffer.concat([...parts, ...central, eocd]);
writeFileSync(outPath, all);
console.log(`${outPath} ${all.length} bytes`);
