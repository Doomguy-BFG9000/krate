#!/usr/bin/env python3
"""krate.tech/.well-known/security.txt is there, valid, and not expired.

RFC 9116 is short, and the ways to get it wrong are all silent: the file is
written once, the deploy drops it, or its Expires date passes and every
scanner that reads it calls it invalid. The deploy used to drop it: GitHub's
upload-pages-artifact excludes every path that starts with a dot, so
.well-known never reached the site. pages.yml now archives the site itself and
runs this check on the assembled site, so a security.txt that is missing,
malformed or expired fails the deploy instead of shipping.

    python3 scripts/check-security-txt.py              check docs/landing (the source)
    python3 scripts/check-security-txt.py --site _site check an assembled site
    python3 scripts/check-security-txt.py --self-test

Run against an assembled site it also checks that every Policy link on
krate.tech names a page that exists in that site.
"""

import argparse
import datetime as dt
import pathlib
import re
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
ORIGIN = "https://krate.tech"
PATH = ".well-known/security.txt"
CANONICAL = f"{ORIGIN}/{PATH}"
# The fields RFC 9116 section 2.5 defines (CSAF is registered since).
KNOWN = {"acknowledgments", "canonical", "contact", "encryption", "expires",
         "hiring", "policy", "preferred-languages", "csaf"}
# A year out at most (the RFC recommends less than a year), and a warning
# when the date is close enough that the next deploy might be after it.
MAX_DAYS = 366
WARN_DAYS = 45


def parse(text):
    """Field lines as (name, value) pairs, comments and blanks skipped."""
    fields, problems = [], []
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        m = re.match(r"^([A-Za-z0-9-]+):[ \t]*(\S.*)$", line)
        if not m:
            problems.append(f"line {n} is not 'Field: value': {line!r}")
            continue
        fields.append((m.group(1).lower(), m.group(2).strip()))
    return fields, problems


def check_text(text, now, site=None):
    """Errors and warnings for one security.txt."""
    errors, warnings = [], []
    fields, problems = parse(text)
    errors += problems
    names = [n for n, _ in fields]
    for name in sorted(set(names) - KNOWN):
        errors.append(f"unknown field {name!r}")
    contacts = [v for n, v in fields if n == "contact"]
    if not contacts:
        errors.append("no Contact field (RFC 9116 requires at least one)")
    for value in contacts:
        if not re.match(r"^(mailto:[^@\s]+@[^@\s]+|https://\S+|tel:\S+)$", value):
            errors.append(f"Contact {value!r} is not a mailto:, https: or tel: URI")
    expires = [v for n, v in fields if n == "expires"]
    if len(expires) != 1:
        errors.append(f"wants exactly one Expires field, has {len(expires)}")
    else:
        try:
            when = dt.datetime.strptime(expires[0], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
        except ValueError:
            errors.append(f"Expires {expires[0]!r} is not YYYY-MM-DDTHH:MM:SSZ")
        else:
            left = (when - now).days
            if when <= now:
                errors.append(f"Expires {expires[0]} has passed: move it up to a year ahead")
            elif left > MAX_DAYS:
                errors.append(f"Expires {expires[0]} is more than a year away (RFC 9116 2.5.5)")
            elif left < WARN_DAYS:
                warnings.append(f"Expires {expires[0]} is {left} days away: move it up to a year ahead")
    canon = [v for n, v in fields if n == "canonical"]
    if canon != [CANONICAL]:
        errors.append(f"wants one Canonical: {CANONICAL}, has {canon}")
    for name in ("preferred-languages",):
        if names.count(name) > 1:
            errors.append(f"{name} appears more than once")
    policies = [v for n, v in fields if n == "policy"]
    if not policies:
        errors.append("no Policy field: point at the security page")
    if site is not None:
        for value in policies:
            if value.startswith(ORIGIN + "/"):
                rel = value[len(ORIGIN) + 1:]
                page = site / rel / "index.html" if rel.endswith("/") or not rel else site / rel
                if not page.is_file():
                    errors.append(f"Policy {value} names a page this site does not have ({page})")
    for value in [v for n, v in fields if n in ("policy", "canonical", "encryption", "acknowledgments", "hiring")]:
        if not value.startswith("https://"):
            errors.append(f"{value!r} is not an https:// URI")
    return errors, warnings


def run(root, site_mode, now):
    f = root / PATH
    if not f.is_file():
        print(f"check-security-txt: {f} is missing", file=sys.stderr)
        return 1
    errors, warnings = check_text(f.read_text(encoding="utf-8"), now, root if site_mode else None)
    for w in warnings:
        print(f"::warning::security.txt: {w}")
    for e in errors:
        print(f"security.txt: {e}", file=sys.stderr)
    if errors:
        return 1
    print(f"ok  {f.relative_to(root.parent) if root.parent in f.parents else f}: valid, {len(warnings)} warning(s)")
    return 0


def self_test():
    now = dt.datetime(2026, 10, 14, tzinfo=dt.timezone.utc)
    good = (
        "# comment\nContact: mailto:yashraj@krate.tech\nExpires: 2027-10-01T00:00:00Z\n"
        f"Policy: {ORIGIN}/security/\nPreferred-Languages: en\nCanonical: {CANONICAL}\n"
    )
    assert check_text(good, now) == ([], []), check_text(good, now)
    # The real file passes too, on the day it was written.
    real = (ROOT / "docs/landing" / PATH).read_text(encoding="utf-8")
    assert check_text(real, now)[0] == [], check_text(real, now)
    # Each way to break it is reported.
    assert any("has passed" in e for e in check_text(good, dt.datetime(2027, 10, 2, tzinfo=dt.timezone.utc))[0])
    assert any("more than a year" in e for e in check_text(good.replace("2027-10-01", "2028-12-01"), now)[0])
    assert check_text(good, dt.datetime(2027, 9, 1, tzinfo=dt.timezone.utc))[1], "a near date warns"
    assert any("Contact" in e for e in check_text(good.replace("Contact: mailto:yashraj@krate.tech\n", ""), now)[0])
    assert any("Canonical" in e for e in check_text(good.replace(CANONICAL, ORIGIN + "/security.txt"), now)[0])
    assert any("unknown field" in e for e in check_text(good + "Contacts: x\n", now)[0])
    assert any("not 'Field: value'" in e for e in check_text(good + "just words\n", now)[0])
    assert any("Expires" in e for e in check_text(good.replace("2027-10-01T00:00:00Z", "next year"), now)[0])
    with tempfile.TemporaryDirectory() as tmp:
        site = pathlib.Path(tmp)
        assert any("does not have" in e for e in check_text(good, now, site)[0]), "a dead Policy link is caught"
        (site / "security").mkdir()
        (site / "security/index.html").write_text("<html></html>")
        assert check_text(good, now, site)[0] == []
        # A site without the file at all is a failure, which is the bug this exists for.
        assert run(site, True, now) == 1
    print("ok  check-security-txt self-test")
    return 0


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--site", type=pathlib.Path, help="an assembled site directory (default: docs/landing)")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args(argv)
    if args.self_test:
        return self_test()
    now = dt.datetime.now(dt.timezone.utc)
    if args.site:
        return run(args.site.resolve(), True, now)
    return run(ROOT / "docs/landing", False, now)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
