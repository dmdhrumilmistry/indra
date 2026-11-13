#!/usr/bin/env python3
"""Local-only run metrics for the security harness.

Records what each run and each agent inside it actually cost: wall time, tokens,
model, outcome. Without this, "the cache is working" and "the review got slower"
are both unfalsifiable claims.

**Nothing here is ever transmitted.** There is no network code in this file and
no reporting endpoint. Records are written under your own cache directory and
stay on the machine that produced them. `sh-metrics.py path` prints exactly
where, and `purge` deletes it all.

Two files, both append-only JSONL:

  runs.jsonl     one line per run: repo, PR, mode, verdict, totals, arguments
  events.jsonl   one line per phase or agent: model, tokens, duration, outcome

JSONL because a crashed run still leaves valid lines above the crash, which is
the failure mode that matters when you are measuring a thing that sometimes
dies.

  path                          print the metrics directory
  start   --run-id X ...        open a run
  event   --run-id X ...        record one phase or agent
  finish  --run-id X ...        close a run with its totals
  report  [--repo R] [--last N] aggregate what was recorded
  purge   [--older-than-days N] delete records
"""

import argparse
import datetime as _dt
import io
import json
import os
import re
import sys

SCHEMA = 1

# Anything that looks like a credential is replaced before it reaches disk.
# Local-only is not a reason to keep a token in a log file: local files get
# pasted into issues.
_REDACT = [
    (re.compile(r'\b(gh[pousr]_[A-Za-z0-9]{16,})\b'), "<redacted-gh-token>"),
    (re.compile(r'\b(sk-[A-Za-z0-9_-]{16,})\b'), "<redacted-api-key>"),
    (re.compile(r'\b(AIza[0-9A-Za-z_-]{20,})\b'), "<redacted-api-key>"),
    (re.compile(r'(?i)\b(authorization|token|secret|password|api[_-]?key)\s*[:=]\s*\S+'),
     r"\1=<redacted>"),
]


def redact(text):
    if not isinstance(text, str):
        return text
    for pat, repl in _REDACT:
        text = pat.sub(repl, text)
    return text


def metrics_dir():
    env = os.environ.get("SH_METRICS_DIR")
    if env:
        return env
    if sys.platform == "win32":
        base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
        return os.path.join(base, "indra", "metrics")
    if sys.platform == "darwin":
        return os.path.join(os.path.expanduser("~"), "Library", "Application Support",
                            "indra", "metrics")
    base = os.environ.get("XDG_DATA_HOME") or os.path.join(os.path.expanduser("~"), ".local", "share")
    return os.path.join(base, "indra", "metrics")


def _append(name, record):
    d = metrics_dir()
    os.makedirs(d, exist_ok=True)
    record = {k: redact(v) for k, v in record.items()}
    record["schema"] = SCHEMA
    record.setdefault("at", _dt.datetime.now(_dt.timezone.utc).isoformat())
    with io.open(os.path.join(d, name), "a", encoding="utf-8", newline="") as fh:
        fh.write(json.dumps(record, sort_keys=True) + "\n")
    return os.path.join(d, name)


def _read(name):
    p = os.path.join(metrics_dir(), name)
    if not os.path.isfile(p):
        return []
    rows = []
    with io.open(p, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except ValueError:
                continue  # a torn final line from a killed run is not an error
    return rows


def out(o):
    print(json.dumps(o, indent=2, sort_keys=True))


def cmd_path(a):
    d = metrics_dir()
    out({"dir": d, "runs": os.path.join(d, "runs.jsonl"),
         "events": os.path.join(d, "events.jsonl"),
         "exists": os.path.isdir(d), "local_only": True})
    return 0


def cmd_start(a):
    p = _append("runs.jsonl", {
        "event": "start", "run_id": a.run_id, "repo": a.repo, "pr": a.pr,
        "mode": a.mode, "tier": a.tier, "arguments": a.arguments,
        "cache": a.cache, "skill_version": a.skill_version,
    })
    out({"recorded": "start", "run_id": a.run_id, "file": p})
    return 0


def cmd_event(a):
    p = _append("events.jsonl", {
        "run_id": a.run_id, "phase": a.phase, "agent": a.agent,
        "vuln_class": a.vuln_class, "model": a.model,
        "tokens_in": a.tokens_in, "tokens_out": a.tokens_out,
        "duration_ms": a.duration_ms, "status": a.status,
        "reused": a.reused, "note": a.note,
    })
    out({"recorded": "event", "run_id": a.run_id, "phase": a.phase, "file": p})
    return 0


def cmd_finish(a):
    p = _append("runs.jsonl", {
        "event": "finish", "run_id": a.run_id, "verdict": a.verdict,
        "status_state": a.status_state, "findings": a.findings,
        "posted": a.posted, "deduped": a.deduped, "reused_files": a.reused_files,
        "reused_verdicts": a.reused_verdicts, "duration_ms": a.duration_ms,
        "tokens_in": a.tokens_in, "tokens_out": a.tokens_out,
        "cost_usd": a.cost_usd, "agents_launched": a.agents_launched,
    })
    out({"recorded": "finish", "run_id": a.run_id, "file": p})
    return 0


def cmd_report(a):
    runs, events = _read("runs.jsonl"), _read("events.jsonl")
    starts = {r["run_id"]: r for r in runs if r.get("event") == "start"}
    fins = {r["run_id"]: r for r in runs if r.get("event") == "finish"}

    ids = [i for i in starts if not a.repo or starts[i].get("repo") == a.repo]
    ids.sort(key=lambda i: starts[i].get("at", ""))
    if a.last:
        ids = ids[-a.last:]

    by_model, rows = {}, []
    for i in ids:
        s, f = starts[i], fins.get(i, {})
        evs = [e for e in events if e.get("run_id") == i]
        for e in evs:
            m = e.get("model") or "unknown"
            b = by_model.setdefault(m, {"events": 0, "tokens_in": 0, "tokens_out": 0, "ms": 0})
            b["events"] += 1
            for k, src in (("tokens_in", "tokens_in"), ("tokens_out", "tokens_out"), ("ms", "duration_ms")):
                b[k] += (e.get(src) or 0)
        rows.append({
            "run_id": i, "at": s.get("at"), "repo": s.get("repo"), "pr": s.get("pr"),
            "tier": s.get("tier"), "cache": s.get("cache"),
            "verdict": f.get("verdict"), "status": f.get("status_state"),
            "agents": f.get("agents_launched"), "findings": f.get("findings"),
            "reused_files": f.get("reused_files"), "reused_verdicts": f.get("reused_verdicts"),
            "duration_ms": f.get("duration_ms"), "cost_usd": f.get("cost_usd"),
            "complete": i in fins,
        })

    totals = {
        "runs": len(rows),
        "incomplete": sum(1 for r in rows if not r["complete"]),
        "cost_usd": round(sum(r.get("cost_usd") or 0 for r in rows), 4),
        "duration_ms": sum(r.get("duration_ms") or 0 for r in rows),
        "agents": sum(r.get("agents") or 0 for r in rows),
    }
    out({"dir": metrics_dir(), "totals": totals, "by_model": by_model, "runs": rows})
    return 0


def cmd_purge(a):
    d = metrics_dir()
    if a.older_than_days is None:
        removed = []
        for n in ("runs.jsonl", "events.jsonl"):
            p = os.path.join(d, n)
            if os.path.isfile(p):
                os.remove(p)
                removed.append(p)
        out({"purged": removed})
        return 0
    cutoff = _dt.datetime.now(_dt.timezone.utc) - _dt.timedelta(days=a.older_than_days)
    kept_total = dropped_total = 0
    for n in ("runs.jsonl", "events.jsonl"):
        rows = _read(n)
        kept = []
        for r in rows:
            try:
                if _dt.datetime.fromisoformat(r.get("at", "")) >= cutoff:
                    kept.append(r)
            except Exception:
                kept.append(r)  # unparseable timestamps are kept, not silently dropped
        dropped_total += len(rows) - len(kept)
        kept_total += len(kept)
        p = os.path.join(d, n)
        if os.path.isfile(p):
            with io.open(p, "w", encoding="utf-8", newline="") as fh:
                for r in kept:
                    fh.write(json.dumps(r, sort_keys=True) + "\n")
    out({"kept": kept_total, "dropped": dropped_total})
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd")

    sub.add_parser("path").set_defaults(fn=cmd_path)

    p = sub.add_parser("start")
    p.add_argument("--run-id", required=True)
    p.add_argument("--repo"); p.add_argument("--pr"); p.add_argument("--mode", default="pr-review")
    p.add_argument("--tier", type=int); p.add_argument("--arguments",
                                                       help="the flags the user passed, verbatim")
    p.add_argument("--cache", help="hit or the miss reason")
    p.add_argument("--skill-version")
    p.set_defaults(fn=cmd_start)

    p = sub.add_parser("event")
    p.add_argument("--run-id", required=True)
    p.add_argument("--phase", required=True)
    p.add_argument("--agent"); p.add_argument("--vuln-class"); p.add_argument("--model")
    p.add_argument("--tokens-in", type=int); p.add_argument("--tokens-out", type=int)
    p.add_argument("--duration-ms", type=int)
    p.add_argument("--status", default="ok")
    p.add_argument("--reused", action="store_true")
    p.add_argument("--note")
    p.set_defaults(fn=cmd_event)

    p = sub.add_parser("finish")
    p.add_argument("--run-id", required=True)
    p.add_argument("--verdict"); p.add_argument("--status-state")
    p.add_argument("--findings", type=int); p.add_argument("--posted", type=int)
    p.add_argument("--deduped", type=int); p.add_argument("--reused-files", type=int)
    p.add_argument("--reused-verdicts", type=int)
    p.add_argument("--duration-ms", type=int)
    p.add_argument("--tokens-in", type=int); p.add_argument("--tokens-out", type=int)
    p.add_argument("--cost-usd", type=float); p.add_argument("--agents-launched", type=int)
    p.set_defaults(fn=cmd_finish)

    p = sub.add_parser("report")
    p.add_argument("--repo"); p.add_argument("--last", type=int)
    p.set_defaults(fn=cmd_report)

    p = sub.add_parser("purge")
    p.add_argument("--older-than-days", type=int, default=None)
    p.set_defaults(fn=cmd_purge)

    a = ap.parse_args(argv)
    if not getattr(a, "fn", None):
        ap.print_help()
        return 2
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
