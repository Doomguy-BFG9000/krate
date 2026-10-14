// A limit on refused attempts per person (IC-948, Addendum 3 B).
//
// Every refusal by a signed-in principal is audited, so one account could
// grow an organisation's audit a refusal at a time. A principal gets at
// most 30 refused attempts a minute against organisations, counted in the
// anonymous wall's RateGate under the bucket `org/refused` and the
// principal's own name (not the address it calls from); past that, every
// organisation request answers 429 `rate-limited` without reaching a store.

export const REFUSED_PER_MINUTE = 30;
const BUCKET = "org/refused";
const WINDOW = 60;

const local = new WeakMap();
function localCount(env, key, peek) {
  let counts = local.get(env);
  if (!counts) local.set(env, (counts = new Map()));
  const slot = `${key}:${Math.floor(Date.now() / 1000 / WINDOW)}`;
  const used = counts.get(slot) || 0;
  if (!peek) counts.set(slot, used + 1);
  return used;
}

async function ask(env, principal, peek) {
  try {
    if (env.RATE_GATE) {
      const gate = env.RATE_GATE.get(env.RATE_GATE.idFromName(`refused:${principal}`));
      const res = await gate.fetch("https://rate-gate/count", {
        method: "POST",
        body: JSON.stringify({ bucket: BUCKET, limit: REFUSED_PER_MINUTE, peek }),
      });
      return (await res.json()).allowed !== false;
    }
    return localCount(env, principal, peek) < REFUSED_PER_MINUTE;
  } catch (_) {
    // A counter must not take organisations down; the store still decides.
    return true;
  }
}

/// Has this principal used up its refusals for the minute?
export async function refusalsExhausted(env, principals) {
  for (const p of principals) if (!(await ask(env, p, true))) return true;
  return false;
}

/// Count one refusal against each principal the request carried.
export async function noteRefusal(env, principals) {
  for (const p of principals) await ask(env, p, false);
}

/// Whether an answer is a refusal that counts: the store said no.
export function isRefusal(status) {
  return status === 403 || status === 404 || status === 409 || status === 410 || status === 422 || status === 402;
}

export function rateLimitedAnswer() {
  return new Response(
    JSON.stringify({ error: "rate-limited", message: "too many refused attempts -- wait a minute and try again" }),
    {
      status: 429,
      headers: {
        "content-type": "application/json",
        "retry-after": String(WINDOW - (Math.floor(Date.now() / 1000) % WINDOW)),
      },
    },
  );
}
