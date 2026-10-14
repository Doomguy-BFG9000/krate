/* Two fixes to the organisation lifecycle (IC-948 D):
 *
 * 1. The export used to build its zip in memory, and a Worker holds about
 *    128 MB: a few large releases would have run it out. It now streams
 *    into R2 a fixed-size part at a time.
 * 2. A recovery notice only reached email principals, so the objection
 *    window protected no owner who signs in with GitHub. It now reaches a
 *    GitHub owner or admin at their account's address.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/org-lifecycle-fixes.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert";
import { writeFileSync, mkdtempSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { orgEnv, call, acme, fixture, worker } from "./org-tools.mjs";
import { streamExport, exportStats, EXPORT_PART_BYTES } from "../src/org-lifecycle.js";
import { addressFor } from "../src/org-mail.js";

/* Gmail's SMTP server, answering as test/founder-welcome.test.mjs's does. */
function fakeGmail(seen) {
  return () => {
    const enc = new TextEncoder();
    const dec = new TextDecoder();
    let toClient;
    let inData = false;
    let data = "";
    const readable = new ReadableStream({
      start(c) {
        toClient = c;
        c.enqueue(enc.encode("220 smtp.gmail.com ESMTP ready\r\n"));
      },
    });
    const answer = (line) => toClient.enqueue(enc.encode(`${line}\r\n`));
    const writable = new WritableStream({
      write(chunk) {
        const text = dec.decode(chunk);
        if (inData) {
          data += text;
          if (data.endsWith("\r\n.\r\n") || data.endsWith("\r\n.")) {
            inData = false;
            seen.mails.push({ commands: [...seen.commands] });
            seen.commands = [];
            data = "";
            answer("250 2.0.0 OK queued");
          }
          return;
        }
        const line = text.replace(/\r\n$/, "");
        seen.commands.push(line);
        if (line.startsWith("EHLO")) answer("250-smtp.gmail.com at your service\r\n250 AUTH LOGIN PLAIN");
        else if (line.startsWith("AUTH PLAIN")) answer("235 2.7.0 Accepted");
        else if (line.startsWith("MAIL FROM") || line.startsWith("RCPT TO")) answer("250 2.1.0 OK");
        else if (line === "DATA") {
          inData = true;
          answer("354 Go ahead");
        } else if (line === "QUIT") answer("221 2.0.0 closing");
      },
    });
    return { readable, writable, close: async () => {} };
  };
}

test("a recovery notice reaches a GitHub owner at their account's address; only those with none are not mailed", async () => {
  const seen = { commands: [], mails: [] };
  const env = orgEnv({ GMAIL_APP_PASSWORD: "app password", SMTP_CONNECT: fakeGmail(seen), KRATE_ADMINS: "support" });
  await acme(env);
  // alice signed in to krate.tech with GitHub: her account shows the email
  // GitHub showed on her profile, and the sign-in recorded the account.
  env._kv.set("ghlogin:alice", "acct-alice");
  env._kv.set("user:acct-alice", JSON.stringify({ id: "acct-alice", login: "alice", email: "alice@acme.example", providers: ["github"] }));
  // erin, a second owner, linked GitHub to an account that proved an inbox
  // with Google: that proved inbox comes first.
  const inv = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: "erin", role: "owner" } });
  await call(env, "erin", "POST", `/orgs/acme/invites/${inv.body.code}/accept`);
  env._kv.set("ghlogin:erin", "acct-erin");
  env._kv.set("user:acct-erin", JSON.stringify({ id: "acct-erin", login: "erin-b", email: "", providers: ["google", "github"], verified_emails: ["erin@acme.example"] }));
  // frank, an admin, has never signed in to the site: nothing to reach.
  const adm = await call(env, "alice", "POST", "/orgs/acme/invites", { json: { github: "frank", role: "admin" } });
  await call(env, "frank", "POST", `/orgs/acme/invites/${adm.body.code}/accept`);
  seen.mails.length = 0;

  const r = await call(env, "support", "POST", "/orgs/acme/recover", {
    json: { principal: "github:carol", reason: "both owners asked by phone; checked against the contract on file" },
  });
  assert.strictEqual(r.status, 201, JSON.stringify(r.body));
  assert.deepStrictEqual([...r.body.mailed].sort(), ["github:alice", "github:erin"]);
  assert.deepStrictEqual(r.body.not_mailed, ["github:frank"], "only an owner or admin with no address at all");
  const to = seen.mails.map((m) => m.commands.find((c) => c.startsWith("RCPT TO:<") && !c.includes("yashraj@krate.tech"))).sort();
  assert.deepStrictEqual(to, ["RCPT TO:<alice@acme.example>", "RCPT TO:<erin@acme.example>"]);
  // Who could not be told is kept with the recovery, for the owners' banner:
  // shown to owners and admins on GET /orgs/<slug>, and to nobody else.
  assert.deepStrictEqual(r.body.recovery.not_mailed, ["github:frank"]);
  for (const who of ["alice", "erin", "frank"]) {
    const seenBy = (await call(env, who, "GET", "/orgs/acme")).body.org.recovery;
    assert.deepStrictEqual([seenBy.id, seenBy.not_mailed], [r.body.recovery.id, ["github:frank"]], who);
  }
  for (const who of ["bob", "carol", "dan"]) {
    assert.strictEqual((await call(env, who, "GET", "/orgs/acme")).body.org.recovery, null, `${who} sees no recovery at all`);
  }
  // It stays with the recovery after it is cancelled.
  await call(env, "alice", "POST", `/orgs/acme/recover/${r.body.recovery.id}/object`);
  assert.deepStrictEqual((await call(env, "alice", "GET", "/orgs/acme")).body.org.recovery.not_mailed, ["github:frank"]);

  // An account made by email that only later linked GitHub: its plain
  // `email` is not one GitHub showed, so it is not used.
  env._kv.set("user:acct-alice", JSON.stringify({ id: "acct-alice", login: "alice", email: "alice@acme.example", providers: ["email", "github"] }));
  assert.strictEqual(await addressFor(env, "github:alice"), null);
  assert.strictEqual(await addressFor(env, "email:x@acme.example"), "x@acme.example");

  // A GitHub sign-in records which account the login belongs to.
  const fresh = orgEnv();
  await worker.fetch(new Request("https://hub.example/my/apps", { headers: { authorization: "Bearer gho_grace" } }), fresh);
  assert.ok(fresh._kv.get("ghlogin:grace"), "the login points at its account");
});

test("an export far larger than a Worker's memory streams into R2 with a bounded buffer", async () => {
  const MiB = 1024 * 1024;
  const SIZE = 100 * MiB;
  // Three releases of 100 MiB: 300 MiB, more than twice what a Worker
  // holds. Their bodies are made a chunk at a time and never stored.
  const digests = ["a", "b", "c"].map((c) => c.repeat(64));
  const body = (seed) => {
    let sent = 0;
    return new ReadableStream({
      pull(controller) {
        if (sent >= SIZE) return controller.close();
        sent += MiB;
        controller.enqueue(new Uint8Array(MiB).fill(seed));
      },
    });
  };
  const parts = [];
  let completed = null;
  const env = {
    APPS: { get: async () => null },
    BUNDLES: {
      head: async (k) => (digests.includes(k) ? { size: SIZE } : null),
      get: async (k) => (digests.includes(k) ? { size: SIZE, body: body(digests.indexOf(k) + 1) } : null),
      createMultipartUpload: async () => ({
        uploadPart: async (n, bytes) => {
          parts.push(bytes.length);
          return { partNumber: n, etag: `e${n}` };
        },
        complete: async (list) => {
          completed = list;
          return {};
        },
        abort: async () => {},
      }),
    },
  };
  const data = {
    org: { slug: "acme", name: "Acme" },
    members: [],
    candidates: digests.map((d, i) => ({ digest: d, name: `Big ${i}`, version: "1.0.0", state: "released" })),
    audit: [],
  };
  const { size } = await streamExport(env, "acme", "ex_test", data);
  assert.ok(size > 3 * SIZE, "every release went into the package");
  assert.ok(exportStats.peakBuffered <= EXPORT_PART_BYTES + MiB, `peak buffered ${exportStats.peakBuffered} bytes`);
  assert.ok(exportStats.peakBuffered * 10 < 128 * MiB, "an order of magnitude under a Worker's memory");
  assert.strictEqual(completed.length, parts.length);
  assert.ok(parts.length > 30);
  assert.ok(parts.slice(0, -1).every((n) => n === EXPORT_PART_BYTES), "every part but the last the same size, as R2 requires");
  assert.strictEqual(parts.reduce((a, b) => a + b, 0), size);
});

test("past the package limit a release is listed as excluded with its digest and size", async () => {
  const env = orgEnv({ ORG_EXPORT_MAX_BYTES: "1000" });
  await acme(env);
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`);
  const ex = await call(env, "alice", "POST", "/orgs/acme/export");
  const got = await call(env, "alice", "GET", `/orgs/acme/exports/${ex.body.export.id}`);
  const text = new TextDecoder("latin1").decode(got.bytes);
  assert.ok(!text.includes(".krate\u0000") && !text.includes(`-${up.body.digest.slice(0, 12)}.krate`), "the release file is not in the package");
  assert.ok(text.includes(`"digest": "${up.body.digest}"`) && text.includes(`"size": ${fixture("candidate-v1.krate").length}`), "it is listed with digest and size");
});

test("the streamed package is a zip other tools read", async () => {
  const env = orgEnv();
  await acme(env);
  const up = await call(env, "carol", "POST", "/orgs/acme/candidates", { bytes: fixture("candidate-v1.krate") });
  await call(env, "bob", "POST", `/orgs/acme/candidates/${up.body.digest}/approve`);
  const ex = await call(env, "alice", "POST", "/orgs/acme/export");
  const got = await call(env, "alice", "GET", `/orgs/acme/exports/${ex.body.export.id}`);
  const path = join(mkdtempSync(join(tmpdir(), "org-export-")), "export.zip");
  writeFileSync(path, got.bytes);
  const said = execFileSync(
    "python3",
    ["-c", "import sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); print(z.testzip() or 'ok', len(z.namelist()))", path],
    { encoding: "utf8" },
  );
  assert.match(said, /^ok 6/, said);
});
