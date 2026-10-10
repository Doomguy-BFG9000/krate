// Send the founder's welcome -- the exact note the hub sends a new person --
// to one address, to see it land. Uses a Resend API key you pass in; the
// hub's own key is a Cloudflare secret nobody can read back.
//
//   RESEND_API_KEY=re_... node --experimental-wasm-modules cloud/worker/scripts/send-welcome-test.mjs you@example.com "Your Name"
import { welcomeText, WELCOME_FROM, WELCOME_REPLY_TO, WELCOME_SUBJECT } from "../src/index.js";

const [to, name = ""] = process.argv.slice(2);
const key = process.env.RESEND_API_KEY;
if (!to || !key) {
  console.error('usage: RESEND_API_KEY=re_... node --experimental-wasm-modules cloud/worker/scripts/send-welcome-test.mjs you@example.com "Your Name"');
  process.exit(2);
}
const res = await fetch("https://api.resend.com/emails", {
  method: "POST",
  headers: { authorization: `Bearer ${key}`, "content-type": "application/json" },
  body: JSON.stringify({
    from: WELCOME_FROM,
    to: [to],
    reply_to: WELCOME_REPLY_TO,
    subject: WELCOME_SUBJECT,
    text: welcomeText(name),
  }),
});
console.log(res.ok ? `sent to ${to}` : `Resend refused it: ${res.status} ${await res.text()}`);
process.exit(res.ok ? 0 : 1);
