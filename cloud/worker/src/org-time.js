// An organisation's own day (F-319). Every count a team reads by the day
// -- who opened an app, team data written, connection uses, refusals --
// starts at midnight where the team is, not in London: a team in Singapore
// that opens its apps at nine in the morning sees them on that morning's
// day, not on the evening before.
//
// The organisation has one time zone, an IANA name ("Asia/Singapore").
// The owner's browser names it when the organisation is made; an owner or
// admin changes it in Settings. One made before this, or by the CLI, reads
// UTC until then, and says so. A change applies from the moment it is
// made: days already counted keep the day they were counted on.
//
//   PUT /orgs/<slug>/time-zone {time_zone}   owner or admin
//   GET /orgs/<slug>                         org.time_zone, org.time_zone_chosen
import { Org, MATRIX, extendOrg, allows } from "./org.js";

MATRIX["org.time-zone"] = { owner: "yes", admin: "yes" };

const formatters = new Map();
function formatter(zone) {
  let f = formatters.get(zone);
  if (!f) {
    f = new Intl.DateTimeFormat("en-CA", { timeZone: zone, year: "numeric", month: "2-digit", day: "2-digit" });
    formatters.set(zone, f);
  }
  return f;
}

/// Whether `zone` is a time zone this runtime knows, by IANA name.
export function validZone(zone) {
  if (typeof zone !== "string" || !zone || zone.length > 64 || !/^[A-Za-z0-9_+\-/]+$/.test(zone)) return false;
  try {
    formatter(zone);
    return true;
  } catch {
    return false;
  }
}

/// The day (YYYY-MM-DD) that `seconds` falls on in `zone`.
export function dayIn(zone, seconds) {
  const parts = formatter(zone || "UTC").formatToParts(new Date(seconds * 1000));
  const get = (type) => parts.find((p) => p.type === type).value;
  return `${get("year")}-${get("month")}-${get("day")}`;
}

/// The day `k` days before `day`, by the calendar: a day that is 23 or 25
/// hours long (a clock change) is still one day.
export function dayBefore(day, k) {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(Date.UTC(y, m - 1, d - k)).toISOString().slice(0, 10);
}

const ops = {
  "time-zone-set"(ctx) {
    const target = `org:${ctx.slug}`;
    const { me, refusal } = this.standing(ctx, "org.time-zone", target);
    if (refusal) return refusal;
    if (allows(me.role, "org.time-zone") !== "yes") {
      return this.deny(ctx, "org.time-zone", target, "role-cannot", `only ${ctx.slug}'s owners and admins change its time zone`);
    }
    const zone = ctx.args.time_zone;
    if (!validZone(zone)) return { status: 400, body: { error: "bad-request", message: "time_zone is a time zone's name, such as Europe/Berlin or Asia/Singapore" } };
    const before = this.timeZoneRow();
    this.sql.exec("UPDATE org SET time_zone = ? WHERE id = 1", zone);
    this.zoneCache = undefined;
    if (before !== zone) this.audit(me.principal, "settings.time-zone", target, "allowed", `${before || "UTC (not chosen)"} -> ${zone}`);
    return this.ok({ time_zone: zone, time_zone_chosen: true });
  },
};

function install(OrgClass) {
  extendOrg({
    columns: [["org", "time_zone", "TEXT"]],
    ops,
    mutations: ["time-zone-set"],
    view(org) {
      org.time_zone = this.timeZone();
      org.time_zone_chosen = Boolean(this.timeZoneRow());
    },
  });
  const P = OrgClass.prototype;
  P.timeZoneRow = function () {
    const r = this.row("SELECT time_zone FROM org WHERE id = 1");
    return r && r.time_zone ? r.time_zone : null;
  };
  /// The organisation's time zone; UTC until one is chosen. Kept on the
  /// store object: one per organisation, and the setter clears it.
  P.timeZone = function () {
    if (this.zoneCache === undefined) {
      const z = this.timeZoneRow();
      this.zoneCache = z && validZone(z) ? z : "UTC";
    }
    return this.zoneCache;
  };
  /// The organisation's day that `seconds` falls on.
  P.orgDay = function (seconds) {
    return dayIn(this.timeZone(), seconds);
  };
  // The owner's browser names the zone as it makes the organisation.
  const create = P.op_create;
  P.op_create = function (ctx) {
    const answer = create.call(this, ctx);
    if (answer.status !== 201) return answer;
    if (validZone(ctx.args.time_zone)) this.sql.exec("UPDATE org SET time_zone = ? WHERE id = 1", ctx.args.time_zone);
    this.zoneCache = undefined;
    if (answer.body && answer.body.org) {
      answer.body.org.time_zone = this.timeZone();
      answer.body.org.time_zone_chosen = Boolean(this.timeZoneRow());
    }
    return answer;
  };
}

install(Org);

/// PUT /orgs/<slug>/time-zone.
export async function orgTimeRoute(h) {
  const { rest, method, mutateRaw, orgAnswer } = h;
  if (rest[0] !== "time-zone" || rest.length !== 1 || method !== "PUT") return null;
  return orgAnswer(await mutateRaw("time-zone-set", (b) => ({ time_zone: b.time_zone })));
}
