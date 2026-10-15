// An organisation's logo: what its people see at the top of the console
// in place of an initial. Small on purpose -- the page shrinks the picture
// to a square of at most 160 pixels before sending it -- so it travels
// inside GET /orgs/<slug> instead of needing a request of its own, and a
// picture can never carry a script: PNG, JPEG or WebP only, never SVG.
//
//   PUT    /orgs/<slug>/logo {data_url}   owner or admin
//   DELETE /orgs/<slug>/logo              owner or admin
//   GET    /orgs/<slug>                   org.logo (a data: URL or null)
import { Org, MATRIX, extendOrg, allows } from "./org.js";

MATRIX["org.logo"] = { owner: "yes", admin: "yes" };

/// The largest logo kept, as a data: URL.
export const LOGO_MAX = 48 * 1024;
const LOGO = /^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/]+={0,2}$/;

/// Whether `v` is a logo this hub keeps: a small PNG, JPEG or WebP whose
/// bytes start the way that kind of file starts.
export function validLogo(v) {
  if (typeof v !== "string" || v.length > LOGO_MAX || !LOGO.test(v)) return false;
  const kind = v.slice(11, v.indexOf(";"));
  let head;
  try {
    head = atob(v.slice(v.indexOf(",") + 1, v.indexOf(",") + 1 + 24));
  } catch {
    return false;
  }
  const b = [...head].map((c) => c.charCodeAt(0));
  if (kind === "png") return b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47;
  if (kind === "jpeg") return b[0] === 0xff && b[1] === 0xd8;
  return String.fromCharCode(...b.slice(0, 4)) === "RIFF" && String.fromCharCode(...b.slice(8, 12)) === "WEBP";
}

function set(ctx, value) {
  const target = `org:${ctx.slug}`;
  const { me, refusal } = this.standing(ctx, "org.logo", target);
  if (refusal) return refusal;
  if (allows(me.role, "org.logo") !== "yes") {
    return this.deny(ctx, "org.logo", target, "role-cannot", `only ${ctx.slug}'s owners and admins change its logo`);
  }
  this.sql.exec("UPDATE org SET logo = ? WHERE id = 1", value);
  this.audit(me.principal, "settings.logo", target, "allowed", value ? "set" : "removed");
  return this.ok({ logo: value });
}

const ops = {
  "logo-set"(ctx) {
    if (!validLogo(ctx.args.data_url)) {
      return { status: 400, body: { error: "bad-request", message: `a logo is a PNG, JPEG or WebP picture under ${LOGO_MAX / 1024} KB, as a data: URL` } };
    }
    return set.call(this, ctx, ctx.args.data_url);
  },
  "logo-clear"(ctx) {
    return set.call(this, ctx, null);
  },
};

extendOrg({
  columns: [["org", "logo", "TEXT"]],
  ops,
  mutations: ["logo-set", "logo-clear"],
  view(org) {
    const r = this.row("SELECT logo FROM org WHERE id = 1");
    org.logo = r && r.logo ? r.logo : null;
  },
});
void Org;

/// PUT and DELETE /orgs/<slug>/logo.
export async function orgBrandRoute(h) {
  const { rest, method, mutateRaw, orgAnswer } = h;
  if (rest[0] !== "logo" || rest.length !== 1) return null;
  if (method === "PUT") return orgAnswer(await mutateRaw("logo-set", (b) => ({ data_url: b.data_url })));
  if (method === "DELETE") return orgAnswer(await mutateRaw("logo-clear", () => ({})));
  return null;
}
