// Organisation billing through Stripe (IC-948, Addendum 3 E; IC-053,
// IC-410, IC-411; shapes from Addendum 4).
//
// Seats are owners, admins, developers and reviewers; recipients are free
// (R8). The price is a setting, STRIPE_PRICE_ORG_SEAT; unset, checkout
// answers `billing-not-open` and pilots are invoiced by hand
// (PUT .../billing/manual, a Krate admin). The webhook (the hub's existing
// one, signature-checked) moves an organisation's entitlement in its store,
// and fails closed: an unknown price or a customer the organisation does
// not have changes nothing and leaves an alert for the admin desk.
// Nothing is suspended until ORG_BILLING_ENFORCED=1.

const STRIPE = "https://api.stripe.com/v1";

/// One Stripe call, with an Idempotency-Key when it creates something.
async function stripeCall(env, path, { form, idempotencyKey } = {}) {
  const res = await fetch(`${STRIPE}/${path}`, {
    method: form ? "POST" : "GET",
    headers: {
      authorization: `Bearer ${env.STRIPE_SECRET_KEY}`,
      ...(form ? { "content-type": "application/x-www-form-urlencoded" } : {}),
      ...(idempotencyKey ? { "idempotency-key": idempotencyKey } : {}),
    },
    body: form ? new URLSearchParams(form) : undefined,
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error((body.error && body.error.message) || `stripe ${path} answered ${res.status}`);
  return body;
}

export const checkoutOpen = (env) => Boolean(env.STRIPE_SECRET_KEY && env.STRIPE_PRICE_ORG_SEAT);

export async function orgBillingRoute(h) {
  const { method, rest } = h;
  if (rest[0] !== "billing") return null;
  if (rest.length === 1 && method === "GET") return billingRead(h);
  if (rest.length === 2 && rest[1] === "checkout" && method === "POST") return checkout(h);
  if (rest.length === 2 && rest[1] === "manual" && method === "PUT") return manual(h);
  return null;
}

/// GET /orgs/<slug>/billing (owner, admin).
async function billingRead(h) {
  const answer = await h.askRaw("billing-read");
  if (answer.status !== 200) return h.orgAnswer(answer);
  const b = answer.body;
  let invoices = [];
  if (b.customer && h.env.STRIPE_SECRET_KEY) {
    try {
      const list = await stripeCall(h.env, `invoices?customer=${encodeURIComponent(b.customer)}&limit=12`);
      invoices = (list.data || []).map((i) => {
        const out = { id: i.id, at: i.created, status: i.status };
        if (i.number) out.number = i.number;
        // Amounts only when Stripe sent them (Addendum 4).
        if (typeof i.amount_due === "number") out.amount = i.amount_paid || i.amount_due;
        if (i.currency) out.currency = i.currency;
        if (i.hosted_invoice_url) out.url = i.hosted_invoice_url;
        return out;
      });
    } catch (_) {
      invoices = [];
    }
  }
  if (b.manual) invoices.push({ id: b.manual.reference, at: b.manual.paid_until, status: "invoiced" });
  return h.json({
    plan: b.plan,
    state: b.state,
    seats_used: b.seats_used,
    seats_paid: b.seats_paid,
    period_end: b.period_end,
    checkout_open: checkoutOpen(h.env),
    manual: b.manual,
    invoices,
  });
}

/// POST /orgs/<slug>/billing/checkout (owner): a Checkout session for the
/// organisation's own Stripe customer, quantity = seats, with the
/// request's Idempotency-Key carried through to Stripe (IC-411).
async function checkout(h) {
  if (!checkoutOpen(h.env)) {
    return h.json(
      { error: "billing-not-open", message: "Krate invoices pilot organisations directly: write to yashraj@krate.tech" },
      503,
    );
  }
  const key = (h.request.headers.get("idempotency-key") || "").trim().slice(0, 120);
  const first = await h.askRaw("billing-checkout");
  if (first.status !== 200) return h.orgAnswer(first);
  let customer = first.body.customer;
  try {
    if (!customer) {
      const made = await stripeCall(h.env, "customers", {
        form: { name: h.slug, "metadata[krate_org]": h.slug },
        idempotencyKey: `krate-org-customer-${h.slug}`,
      });
      customer = made.id;
      await h.askRaw("billing-checkout", { customer });
      await h.env.APPS.put(`orgcust:${customer}`, h.slug).catch(() => {});
    }
    const session = await stripeCall(h.env, "checkout/sessions", {
      form: {
        mode: "subscription",
        customer,
        client_reference_id: `org:${h.slug}`,
        "line_items[0][price]": h.env.STRIPE_PRICE_ORG_SEAT,
        "line_items[0][quantity]": String(first.body.seats),
        "subscription_data[metadata][krate_org]": h.slug,
        success_url: `${h.site}/org/?o=${encodeURIComponent(h.slug)}&billing=done`,
        cancel_url: `${h.site}/org/?o=${encodeURIComponent(h.slug)}&billing=cancelled`,
      },
      // The same request, retried, is the same session; without a key, the
      // same organisation and seat count within the hour.
      idempotencyKey: key
        ? `krate-org-checkout-${h.slug}-${key}`
        : `krate-org-checkout-${h.slug}-${first.body.seats}-${Math.floor(Date.now() / 3_600_000)}`,
    });
    return h.json({ url: session.url });
  } catch (err) {
    return h.json({ error: "billing-not-open", message: `Checkout could not start: ${err.message}` }, 502);
  }
}

/// PUT /orgs/<slug>/billing/manual {reference, paid_until} (Krate admin):
/// how the pilot is billed.
async function manual(h) {
  const admin = await h.isAdmin(h.request, h.env);
  if (!admin) return h.json({ error: "role-cannot", message: "only Krate support records a manual invoice" }, 403);
  const body = await h.readBody();
  if (body.bad || body.tooLarge) return h.json({ error: "bad-request", message: "the body is JSON, under 64 KiB" }, 400);
  return h.orgAnswer(
    await h.orgAsk(h.env, h.slug, {
      op: "billing-manual",
      args: { reference: body.value.reference, paid_until: body.value.paid_until, admin: `github:${String(admin.login).toLowerCase()}` },
    }),
  );
}

/// An admin-desk alert: an organisation billing event that changed nothing.
async function alertDesk(env, slug, eventId, why) {
  console.error(`org billing event refused for ${slug}: ${why}`);
  try {
    await env.APPS.put(
      `org-billing-alert:${Date.now()}:${crypto.randomUUID().slice(0, 8)}`,
      JSON.stringify({ slug, event: eventId, why, at: Math.floor(Date.now() / 1000) }),
      { expirationTtl: 90 * 24 * 3600 },
    );
  } catch (_) {}
}

/// GET /admin/org-billing-alerts (Krate admin): what the webhook refused.
export async function orgBillingAlerts(env) {
  const listed = await env.APPS.list({ prefix: "org-billing-alert:" });
  const out = [];
  for (const { name } of listed.keys || []) {
    try {
      out.push(JSON.parse((await env.APPS.get(name)) || "null"));
    } catch (_) {}
  }
  return out.filter(Boolean);
}

/// The organisation half of the Stripe webhook. Answers true when the event
/// was an organisation's (handled, refused or not), false to let the
/// person-billing code take it. Called only after the signature checked.
export async function orgBillingEvent(env, event, orgAsk) {
  const obj = event.data && event.data.object;
  if (!obj) return false;
  let sub = null;
  let slug = null;
  if (event.type === "checkout.session.completed") {
    const ref = String(obj.client_reference_id || "");
    if (!ref.startsWith("org:")) return false;
    slug = ref.slice(4);
    if (!obj.subscription) return true;
    try {
      sub = await stripeCall(env, `subscriptions/${obj.subscription}`);
    } catch (_) {
      // The subscription events carry it too.
      return true;
    }
  } else if (/^customer\.subscription\.(created|updated|deleted)$/.test(event.type)) {
    sub = obj;
    slug = (obj.metadata && obj.metadata.krate_org) || (await env.APPS.get(`orgcust:${obj.customer}`).catch(() => null));
    if (!slug) return false;
  } else {
    return false;
  }
  if (!env.ORGS) return true;
  const item = sub.items && sub.items.data && sub.items.data[0];
  const answer = await orgAsk(env, slug, {
    op: "billing-stripe",
    args: {
      customer: sub.customer,
      subscription: sub.id,
      status: event.type === "customer.subscription.deleted" ? "canceled" : sub.status,
      price: item && item.price ? item.price.id : null,
      product: item && item.price ? item.price.product : null,
      quantity: item ? item.quantity : 0,
      period_end: sub.current_period_end || 0,
    },
  });
  if (answer.status !== 200) await alertDesk(env, slug, event.id, (answer.body && answer.body.message) || "refused");
  return true;
}
