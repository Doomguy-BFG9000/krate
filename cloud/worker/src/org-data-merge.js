// The merge rule for an organisation's app data (store.org, IC-951).
//
// Pure: no clock, no storage. The device implements exactly the same rule
// in crates/runtime/src/org_merge.rs, and both are tested against one set
// of vectors (test/fixtures/org-data-vectors.json), so a device and the hub
// can never disagree about which write won.
//
// - One row per field. A record is the set of its field rows; a delete is
//   one more row, the record's tombstone, under the reserved field name "".
//   Two people editing different fields of one record both keep theirs.
// - The greater stamp wins, per row. A stamp is a hybrid logical clock --
//   wall milliseconds, a counter, the writing device -- printed at fixed
//   widths, so stamps compare as text exactly as they compare as clocks
//   (and SQLite can compare them).
// - A wrong clock does not win: a push carries the device's own "now", and
//   its stamps are moved by the difference between that and the hub's
//   clock (rebase). What counts is how long ago the device made each edit,
//   on its own clock, which a wrong setting does not change. No stamp is
//   ever later than the hub receiving it, plus DRIFT_MS.
// - A field is live while its stamp is above the tombstone and its value is
//   not null.
//
// Since F-316 the hub merges sealed rows: it sees the opaque record and
// field names, the stamp and the kind (value, removed field, tombstone), and
// applies exactly this rule to them; the names and values are the devices'.
// The value and record-size functions are the devices' rule, kept here so
// both implementations pass the same vectors.

export const DRIFT_MS = 2000;
export const MAX_RECORD_BYTES = 64 * 1024;
export const TOMBSTONE = "";
export const MAX_WALL = Number.MAX_SAFE_INTEGER;
const MAX_COUNTER = 99999999;

const STAMP = /^(\d{16})-(\d{8})-([0-9a-f]{16})$/;
const utf8 = (text) => new TextEncoder().encode(String(text)).length;

/// `{ wall, counter, device }`, or null.
export function parseStamp(text) {
  const m = STAMP.exec(typeof text === "string" ? text : "");
  if (!m) return null;
  const wall = Number(m[1]);
  if (!Number.isSafeInteger(wall) || wall > MAX_WALL) return null;
  return { wall, counter: Number(m[2]), device: m[3] };
}

export function formatStamp({ wall, counter, device }) {
  return `${String(wall).padStart(16, "0")}-${String(counter).padStart(8, "0")}-${device}`;
}

/// -1, 0 or 1. Text order is clock order, by construction.
export function compareStamps(a, b) {
  return a < b ? -1 : a > b ? 1 : 0;
}

/// The next stamp for a write made now on `device`, above `last`.
export function nextStamp(last, phys, device) {
  const p = Math.min(phys, MAX_WALL);
  const l = last ? parseStamp(last) : null;
  if (l && l.wall >= p) {
    if (l.counter >= MAX_COUNTER) return formatStamp({ wall: Math.min(l.wall + 1, MAX_WALL), counter: 0, device });
    return formatStamp({ wall: l.wall, counter: l.counter + 1, device });
  }
  return formatStamp({ wall: p, counter: 0, device });
}

/// Move a pushed stamp onto the hub's clock (see the top of this file).
export function rebase(stamp, deviceNow, hubNow) {
  const s = parseStamp(stamp);
  if (!s) return null;
  const delta = hubNow - deviceNow;
  const shifted = Math.abs(delta) <= DRIFT_MS ? s.wall : s.wall + delta;
  const ceiling = Math.min(hubNow + DRIFT_MS, MAX_WALL);
  const wall = Math.max(0, Math.min(shifted, ceiling));
  return formatStamp({ wall, counter: s.counter, device: s.device });
}

const B64 = /^[A-Za-z0-9+/]*={0,2}$/;

/// Is `v` a value on the wire: null, {t}, {n}, {b} or {x}?
export function validValue(v) {
  if (v === null) return true;
  if (!v || typeof v !== "object" || Array.isArray(v)) return false;
  const keys = Object.keys(v);
  if (keys.length !== 1) return false;
  const [k] = keys;
  if (k === "t") return typeof v.t === "string";
  if (k === "n") return typeof v.n === "number" && Number.isFinite(v.n);
  if (k === "b") return typeof v.b === "boolean";
  if (k === "x") return typeof v.x === "string" && v.x.length % 4 === 0 && B64.test(v.x);
  return false;
}

/// What a value costs against a record's size.
export function valueSize(v) {
  if (v === null) return 0;
  if ("t" in v) return utf8(v.t);
  if ("n" in v) return 8;
  if ("b" in v) return 1;
  if ("x" in v) {
    const pad = v.x.endsWith("==") ? 2 : v.x.endsWith("=") ? 1 : 0;
    return (v.x.length / 4) * 3 - pad;
  }
  return 0;
}

/// Does a row stamped `incoming` replace one stamped `held`?
export function wins(incoming, held) {
  return held == null || incoming > held;
}

/// A record's live fields, `{ name: { v, s } }` -> `[[name, cell]]` sorted.
export function liveFields(cells) {
  const tomb = cells[TOMBSTONE] ? cells[TOMBSTONE].s : null;
  return Object.entries(cells)
    .filter(([name, cell]) => name !== TOMBSTONE && cell.v !== null && (tomb === null || cell.s > tomb))
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
}

export function recordSize(cells) {
  return liveFields(cells).reduce((n, [name, cell]) => n + utf8(name) + valueSize(cell.v), 0);
}
