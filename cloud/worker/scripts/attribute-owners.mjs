#!/usr/bin/env node
// Give each legacy listing, ownership record and channel the owner it
// really has (K-1085, step 4 of Plan/org-pilot/K-1085-ownership-plan.md).
//
// The hub now owns things by principal: `github:<login>` for a login
// GitHub vouched for, `account:<user-id>` for an email or Google account.
// A record written before that has only a login and reads as
// `github:<login>`, so a row an email account published is, until this
// runs, controllable by no one but a GitHub user of that name. This reads
// every account and every record, decides each row's owner, and REPORTS.
// It writes only with --apply, and then only the rows whose owner differs
// from what the legacy reading already gives: an `account:` owner. A row
// it cannot decide is never guessed.
//
//   node cloud/worker/scripts/attribute-owners.mjs --remote            report, production (read-only)
//   node cloud/worker/scripts/attribute-owners.mjs --remote --json     the same, as JSON
//   node cloud/worker/scripts/attribute-owners.mjs --local --persist-to DIR [--apply]
//   node cloud/worker/scripts/attribute-owners.mjs --remote --apply    writes; the founder's call
//
// It reads with `wrangler kv key list` and `wrangler kv key get` on the
// APPS binding, from cloud/worker, and never runs without --remote or
// --local, so it cannot be pointed anywhere by accident.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

/// The owner a login gets, from the accounts that hold it.
///
/// A GitHub account is one created by a GitHub sign-in (`providers[0]` is
/// "github"): its `login` is GitHub's. An account created by email or
/// Google that later linked GitHub keeps its first login, which is an
/// address's local part, so it counts as not-GitHub for that login.
export function ownerForLogin(login, users) {
  const l = String(login || "").toLowerCase();
  const holders = users.filter((u) => String(u.login || "").toLowerCase() === l);
  const github = holders.filter((u) => (u.providers || [])[0] === "github");
  const other = holders.filter((u) => (u.providers || [])[0] !== "github");
  if (github.length && !other.length) return { case: "github", owner: `github:${l}` };
  if (!github.length && other.length === 1) return { case: "account", owner: `account:${other[0].id}` };
  if (!github.length && !other.length) {
    // Nobody holds it now: the legacy default stands, harmless.
    return { case: "no-account", owner: `github:${l}` };
  }
  return {
    case: "ambiguous",
    owner: null,
    held_by: holders.map((u) => ({ id: u.id, providers: u.providers || [], created_by: (u.providers || [])[0] || null })),
  };
}

/// Decide every record. `records` maps a KV key to its parsed value (or
/// the raw string for keys whose value is not JSON). Pure: the tests drive
/// it with a fixture namespace.
export function planAttribution({ users, records }) {
  const rows = [];
  for (const [key, value] of Object.entries(records)) {
    let kind;
    let login;
    if (key.startsWith("app:")) {
      kind = "listing";
      login = value && value.author_login;
    } else if (key.startsWith("owner:")) {
      kind = "ownership";
      login = value && value.login;
    } else if (key.startsWith("channel:")) {
      kind = "channel";
      login = value && value.login;
    } else if (key.startsWith("bigversions:")) {
      kind = "big-versions";
      login = key.slice("bigversions:".length).split(":")[0];
    } else {
      continue;
    }
    const row = { key, kind, login: login || null };
    if (value && typeof value === "object" && value.owner) {
      rows.push({ ...row, case: "already", owner: value.owner, write: false });
      continue;
    }
    if (!login) {
      rows.push({ ...row, case: "no-login", owner: null, write: false });
      continue;
    }
    const decided = ownerForLogin(login, users);
    const legacy = `github:${String(login).toLowerCase()}`;
    const out = { ...row, ...decided, legacy };
    if (decided.case === "account" && kind === "channel") {
      // A moving name is a developer namespace, which only a GitHub owner
      // has. Attributing it to an account would make it stop resolving;
      // that is for the founder to decide, not this script.
      out.case = "review";
      out.note = "a channel under a login only an email or Google account holds";
      out.write = false;
    } else if (decided.case === "account" && kind === "big-versions") {
      out.note = "big versions are keyed by login and open only to GitHub BIG_PUBLISHERS; nothing to write";
      out.write = false;
    } else {
      // Only an owner different from the legacy reading is worth a write.
      out.write = decided.case === "account" && decided.owner !== legacy;
    }
    rows.push(out);
  }
  rows.sort((a, b) => a.key.localeCompare(b.key));
  const count = (c) => rows.filter((r) => r.case === c).length;
  return {
    rows,
    summary: {
      accounts: users.length,
      github_accounts: users.filter((u) => (u.providers || [])[0] === "github").length,
      records: rows.length,
      github: count("github"),
      account: count("account"),
      no_account: count("no-account"),
      already: count("already"),
      ambiguous: count("ambiguous"),
      review: count("review"),
      no_login: count("no-login"),
      writes: rows.filter((r) => r.write).length,
    },
  };
}

/// Write the decided owners: each record read again just before its write,
/// so nothing changed since the report is overwritten. `kv` is
/// `{ get(key) -> string|null, put(key, value) }`.
export async function applyPlan(kv, plan) {
  const done = [];
  for (const row of plan.rows.filter((r) => r.write)) {
    const raw = await kv.get(row.key);
    if (!raw) {
      done.push({ key: row.key, result: "gone" });
      continue;
    }
    let value;
    try {
      value = JSON.parse(raw);
    } catch (_) {
      done.push({ key: row.key, result: "not-json" });
      continue;
    }
    if (value.owner) {
      done.push({ key: row.key, result: "already" });
      continue;
    }
    const login = row.kind === "listing" ? value.author_login : value.login;
    if (String(login || "").toLowerCase() !== String(row.login || "").toLowerCase()) {
      done.push({ key: row.key, result: "changed-since-report" });
      continue;
    }
    await kv.put(row.key, JSON.stringify({ ...value, owner: row.owner }));
    done.push({ key: row.key, result: "written", owner: row.owner });
  }
  return done;
}

/// Read the namespace: every account and every record this decides.
export async function readNamespace(kv) {
  const users = [];
  for (const key of await kv.list("user:")) {
    try {
      const u = JSON.parse((await kv.get(key)) || "null");
      if (u) users.push({ id: u.id || key.slice("user:".length), login: u.login || "", providers: u.providers || [] });
    } catch (_) {}
  }
  const records = {};
  for (const prefix of ["app:", "owner:", "channel:", "bigversions:"]) {
    for (const key of await kv.list(prefix)) {
      const raw = await kv.get(key);
      try {
        records[key] = JSON.parse(raw);
      } catch (_) {
        records[key] = raw;
      }
    }
  }
  return { users, records };
}

/// KV through wrangler, from cloud/worker: `--remote` (production) or
/// `--local --persist-to DIR`.
function wranglerKv(target) {
  const worker = join(dirname(fileURLToPath(import.meta.url)), "..");
  const bin = join(worker, "node_modules", ".bin", "wrangler");
  const where = target.remote ? ["--remote"] : ["--local", ...(target.persist ? ["--persist-to", target.persist] : [])];
  const run = (args) =>
    execFileSync(bin, ["kv", "key", ...args, "--binding", "APPS", ...where], {
      cwd: worker,
      encoding: "utf8",
      env: { ...process.env, WRANGLER_SEND_METRICS: "false" },
      stdio: ["ignore", "pipe", "pipe"],
    });
  return {
    async list(prefix) {
      return JSON.parse(run(["list", "--prefix", prefix])).map((k) => k.name);
    },
    async get(key) {
      try {
        return run(["get", key, "--text"]);
      } catch (_) {
        return null;
      }
    },
    async put(key, value) {
      run(["put", key, value]);
    },
  };
}

function printReport(plan) {
  const s = plan.summary;
  console.log(`accounts: ${s.accounts} (${s.github_accounts} created by a GitHub sign-in)`);
  console.log(
    `records: ${s.records} -- github ${s.github}, account ${s.account}, no current account ${s.no_account}, ` +
      `already owned ${s.already}, ambiguous ${s.ambiguous}, for review ${s.review}, no login ${s.no_login}`,
  );
  console.log(`would write: ${s.writes}`);
  for (const r of plan.rows) {
    const to = r.owner ? ` -> ${r.owner}` : "";
    const why = r.note ? `  (${r.note})` : r.held_by ? `  (held by ${r.held_by.map((h) => `${h.id} via ${h.created_by}`).join(", ")})` : "";
    console.log(`  ${r.write ? "WRITE " : "      "}${r.case.padEnd(10)} ${r.kind.padEnd(12)} ${r.key}  [${r.login}]${to}${why}`);
  }
}

async function main(argv) {
  const remote = argv.includes("--remote");
  const local = argv.includes("--local");
  const at = argv.indexOf("--persist-to");
  const persist = at >= 0 ? argv[at + 1] : null;
  if (remote === local) {
    console.error("say where: --remote (production) or --local [--persist-to DIR]");
    return 2;
  }
  const kv = wranglerKv({ remote, persist });
  const plan = planAttribution(await readNamespace(kv));
  if (argv.includes("--json")) console.log(JSON.stringify(plan, null, 2));
  else printReport(plan);
  if (!argv.includes("--apply")) {
    if (!argv.includes("--json")) console.log("\nreport only: nothing was written (add --apply to write)");
    return 0;
  }
  const done = await applyPlan(kv, plan);
  for (const d of done) console.log(`${d.result.padEnd(22)} ${d.key}${d.owner ? ` -> ${d.owner}` : ""}`);
  return 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main(process.argv.slice(2)).then((code) => process.exit(code), (err) => {
    console.error(String(err && err.stack ? err.stack : err));
    process.exit(1);
  });
}
