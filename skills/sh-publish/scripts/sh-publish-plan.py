#!/usr/bin/env python3
"""Turn a finished indra review run into a publish plan for Jira, Confluence,
GitHub Issues or Notion.

Why a script and not the model: what leaves the machine must be exact and
safe. This decides deterministically which findings are published, what each
record says, and how it is de-duplicated, so an agent only has to carry the
plan out with the destination's MCP tools.

What a record never contains: `evidence`, `payload`, `poc` or `data_flow`.
Evidence for a secrets finding *is* the secret, and exploit payloads do not
belong in a ticket tracker. Records keep the location, impact, verdict and
fix, which is what the people fixing it need.

De-duplication: every record carries a marker line,
    indra-fingerprint: <12 hex>
derived from the finding's fingerprint (the same one sh-pr-review and
sh-memory use). Search the destination for that marker before creating
anything; if it exists, comment or update instead.

Usage:
  sh-publish-plan.py <run_dir> --to jira|github|confluence|notion
                     [--min-severity info|low|medium|high|critical]
                     [--status verified,needs-runtime] [--max 25]

Prints the plan as JSON on stdout. Exit codes: 0 ok, 1 error, 2 bad usage.
"""

import argparse
import hashlib
import json
import os
import sys

SEVERITIES = ["info", "low", "medium", "high", "critical"]
JIRA_PRIORITY = {"critical": "Highest", "high": "High", "medium": "Medium", "low": "Low", "info": "Lowest"}
NEVER = ("evidence", "payload", "poc", "data_flow")
MAX_TEXT = 700


def clip(s, n=MAX_TEXT):
    s = " ".join(str(s or "").split())
    return s if len(s) <= n else s[: n - 3] + "..."


def fingerprint(f):
    fp = str(f.get("fingerprint") or "").strip().lower()
    if len(fp) >= 8 and all(c in "0123456789abcdef" for c in fp):
        return fp[:12]
    cwe = ",".join(f.get("cwe") or [])
    path = str(f.get("file") or "").replace("\\", "/").lstrip("./").lower()
    key = "|".join([cwe, path, str(f.get("class") or ""), str(f.get("title") or "").lower()])
    return hashlib.sha256(key.encode("utf-8")).hexdigest()[:12]


def load(run_dir):
    path = os.path.join(run_dir, "verified.jsonl")
    if not os.path.isfile(path):
        raise FileNotFoundError("no verified.jsonl in %s (run the verify stage first)" % run_dir)
    out = []
    with open(path, encoding="utf-8") as fh:
        for n, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError as e:
                print("skipping verified.jsonl:%d: %s" % (n, e), file=sys.stderr)
    return out


def body_markdown(f, fp, run_id):
    """The record text: Markdown, which every destination's MCP server accepts."""
    sev = str(f.get("severity") or "unknown")
    ver = f.get("verification") or {}
    lines = [
        "**Severity:** %s  " % sev.capitalize(),
        "**Status:** %s  " % (f.get("status") or "unknown"),
        "**Location:** `%s:%s`  " % (f.get("file") or "?", f.get("line") or "?"),
    ]
    if f.get("cwe"):
        lines.append("**CWE:** %s  " % ", ".join(f["cwe"]))
    if f.get("owasp"):
        lines.append("**OWASP:** %s  " % ", ".join(f["owasp"]))
    if f.get("confidence") is not None:
        lines.append("**Confidence:** %s%%  " % f.get("confidence"))
    lines.append("")
    if f.get("why_it_matters"):
        lines += ["### Impact", clip(f["why_it_matters"]), ""]
    if ver.get("rationale"):
        lines += ["### Why it was confirmed", clip(ver["rationale"]), ""]
    if f.get("mitigation"):
        lines += ["### Fix", clip(f["mitigation"]), ""]
    if f.get("chained_with"):
        lines += ["Combines with: %s" % ", ".join(f["chained_with"]), ""]
    lines += [
        "---",
        "Found by an indra security review (%s, %s). Exploit details stay in the local run directory." % (run_id, f.get("id") or "no id"),
        "",
        "indra-fingerprint: %s" % fp,
    ]
    return "\n".join(lines)


def plan(run_dir, to, min_sev, statuses, limit):
    run_id = os.path.basename(os.path.normpath(run_dir))
    floor = SEVERITIES.index(min_sev)
    picked, held_back = [], 0
    for f in load(run_dir):
        if f.get("status") not in statuses:
            continue
        sev = str(f.get("severity") or "info").lower()
        if sev not in SEVERITIES or SEVERITIES.index(sev) < floor:
            continue
        picked.append(f)
    picked.sort(key=lambda f: (-SEVERITIES.index(str(f.get("severity") or "info").lower()), str(f.get("id"))))
    if len(picked) > limit:
        held_back = len(picked) - limit
        picked = picked[:limit]

    records = []
    for f in picked:
        fp = fingerprint(f)
        sev = str(f.get("severity") or "info").lower()
        cls = str(f.get("class") or "security")
        title = "[%s] %s: %s" % (sev.upper(), cls, clip(f.get("title") or f.get("file") or "finding", 120))
        rec = {
            "fingerprint": fp,
            "dedupe_query": "indra-fingerprint: %s" % fp,
            "finding_id": f.get("id"),
            "severity": sev,
            "title": title,
            "body_markdown": body_markdown(f, fp, run_id),
            "labels": ["security", "indra", cls, "severity:" + sev],
        }
        if to == "jira":
            rec["jira"] = {"issue_type": "Bug", "priority": JIRA_PRIORITY.get(sev, "Medium")}
        records.append(rec)
        # A defensive check: nothing sensitive may leave through the plan.
        blob = json.dumps(rec)
        for k in NEVER:
            v = f.get(k)
            if isinstance(v, str) and len(v) >= 8 and v in blob:
                raise RuntimeError("refusing to publish: %s of %s leaked into the record" % (k, f.get("id")))

    out = {
        "destination": to,
        "run": run_id,
        "records": records,
        "held_back_over_limit": held_back,
        "filters": {"min_severity": min_sev, "statuses": sorted(statuses), "max": limit},
    }
    if to in ("confluence", "notion"):
        # One page per run, with a section per finding, is easier to read there.
        sections = ["## %s\n\n%s" % (r["title"], r["body_markdown"]) for r in records]
        counts = {}
        for r in records:
            counts[r["severity"]] = counts.get(r["severity"], 0) + 1
        summary = ", ".join("%d %s" % (counts[s], s) for s in reversed(SEVERITIES) if s in counts) or "no findings at this threshold"
        out["page"] = {
            "title": "Security review %s" % run_id,
            "dedupe_query": "indra-run: %s" % run_id,
            "body_markdown": "Summary: %s.\n\n%s\n\nindra-run: %s\n" % (summary, "\n\n".join(sections), run_id),
        }
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("run_dir")
    ap.add_argument("--to", required=True, choices=["jira", "github", "confluence", "notion"])
    ap.add_argument("--min-severity", default="medium", choices=SEVERITIES)
    ap.add_argument("--status", default="verified", help="comma-separated: verified,needs-runtime")
    ap.add_argument("--max", type=int, default=25)
    a = ap.parse_args()
    statuses = {s.strip() for s in a.status.split(",") if s.strip()}
    bad = statuses - {"verified", "needs-runtime"}
    if bad:
        print("unsupported status: %s (false positives are never published)" % ", ".join(sorted(bad)), file=sys.stderr)
        return 2
    try:
        print(json.dumps(plan(os.path.abspath(a.run_dir), a.to, a.min_severity, statuses, max(1, a.max)), indent=2))
    except (OSError, RuntimeError) as e:
        print("sh-publish-plan: %s" % e, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
