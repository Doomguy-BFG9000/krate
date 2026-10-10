/* A new person gets the founder's plain welcome, once, ten minutes on.
 *
 * Signing up marks the account welcome-due; the ten-minute run sends the
 * note to accounts at least ten minutes old -- from the founder's krate.tech
 * address, replies to his Gmail, plain text, the first name filled in --
 * and records that it went, so a second run sends nothing. An account only
 * minutes old waits. An account that existed before (no due mark) is never
 * mailed.
 *
 * Drives the REAL worker with a Map for KV and a stubbed fetch.
 *
 *   node --experimental-wasm-modules --test cloud/worker/test/founder-welcome.test.mjs
 */
import assert from "node:assert/strict";
import test from "node:test";
import worker from "../src/index.js";

const KV = new Map();
const env = {
  PUBLIC_BASE: "https://hub.test",
  GITHUB_CLIENT_SECRET: "gh-secret",
  RESEND_API_KEY: "resend-key",
  APPS: {
    get: async (k) => KV.get(k) ?? null,
    put: async (k, v) => { KV.set(k, v); },
    delete: async (k) => { KV.delete(k); },
    list: async ({ prefix }) => ({ keys: [...KV.keys()].filter((k) => k.startsWith(prefix)).map((name) => ({ name })) }),
  },
};

const mailed = [];
let githubUser = { id: 41, login: "ada", name: "Ada Lovelace", email: "ada@example.com" };
const realFetch = globalThis.fetch;
globalThis.fetch = async (input, init = {}) => {
  const url = String(input.url || input);
  const reply = (v) => new Response(JSON.stringify(v), { headers: { "content-type": "application/json" } });
  if (url === "https://github.com/login/oauth/access_token") return reply({ access_token: "gho_ada" });
  if (url === "https://api.github.com/user") return reply(githubUser);
  if (url === "https://api.github.com/user/emails") return reply([{ email: githubUser.email, primary: true, verified: true }]);
  if (url === "https://api.resend.com/emails") {
    mailed.push(JSON.parse(init.body));
    return reply({ id: "sent" });
  }
  return realFetch(input, init);
};

async function signUpWithGithub() {
  const ctx = { waitUntil() {} };
  const start = await worker.fetch(new Request("https://hub.test/login/start"), env, ctx);
  assert.equal(start.status, 302);
  const state = new URL(start.headers.get("location")).searchParams.get("state");
  const done = await worker.fetch(new Request(`https://hub.test/login/callback?state=${state}&code=abc`), env, ctx);
  assert.equal(done.status, 302, "the sign-in finishes");
}

const tick = (now) => {
  const real = Date.now;
  Date.now = () => now;
  return worker.scheduled({ cron: "*/10 * * * *" }, env).finally(() => {
    Date.now = real;
  });
};

test("a new account is welcomed once, ten minutes on, in the founder's words", async () => {
  await signUpWithGithub();
  const due = [...KV.keys()].filter((k) => k.startsWith("welcome-due:"));
  assert.equal(due.length, 1, "signing up marks the account welcome-due");
  const created = Number(KV.get(due[0]));

  await tick(created + 3 * 60 * 1000);
  assert.equal(mailed.length, 0, "three minutes in, nothing is sent");

  await tick(created + 11 * 60 * 1000);
  assert.equal(mailed.length, 1, "eleven minutes in, the note goes");
  const mail = mailed[0];
  assert.equal(mail.from, "Yashraj Pardeshi <yashraj@krate.tech>");
  assert.equal(mail.reply_to, "incyashraj@gmail.com");
  assert.deepEqual(mail.to, ["ada@example.com"]);
  assert.equal(mail.subject, "You’re in. One question from me");
  assert.equal(mail.html, undefined, "plain text only");
  assert.ok(mail.text.startsWith("Hi Ada,\n"), mail.text.slice(0, 40));
  assert.ok(mail.text.includes("What’s the first thing you want to make with Krate?"));
  assert.ok(mail.text.includes("https://calendly.com/incyashraj/30min\n"));
  assert.ok(!mail.text.includes("{"), "no template field left unfilled");

  await tick(created + 25 * 60 * 1000);
  assert.equal(mailed.length, 1, "never twice");
  assert.equal([...KV.keys()].filter((k) => k.startsWith("welcome-due:")).length, 0);
});

test("a nameless sign-up is greeted as 'there', and an old account is never mailed", async () => {
  githubUser = { id: 42, login: "nobody", name: "", email: "x@example.com" };
  await signUpWithGithub();
  // An account from before the welcome existed: a user with no due mark.
  KV.set("user:old-person", JSON.stringify({ id: "old-person", created: 1, email: "old@example.com" }));
  await tick(Date.now() + 11 * 60 * 1000);
  const latest = mailed[mailed.length - 1];
  assert.deepEqual(latest.to, ["x@example.com"]);
  assert.ok(latest.text.startsWith("Hi there,\n"), latest.text.slice(0, 30));
  assert.ok(!mailed.some((m) => m.to.includes("old@example.com")), "no mail to a person from before");
});
