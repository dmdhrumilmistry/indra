#!/usr/bin/env python3
"""Record a finished security-harness run into an indra memory vault.

Why a script and not the model: turning verified.jsonl into notes is exact
bookkeeping. Doing it deterministically means a finding is never "remembered"
with a different title, a false positive is never silently dropped, and a
re-run of the same issue lands on the same note instead of a near-duplicate.

What it writes (all through `indra mem`, never by editing files itself):
  - one hub note per vulnerability class      (type: vuln-class)
  - one hub note per affected file            (type: file)
  - verified / needs-runtime findings         (type: finding)
        instance-of -> class hub, located-in -> file hub
  - false positives                           (type: learning, tag false-positive)
        refines -> class hub, located-in -> file hub
    These are the most valuable notes: the next run's hunters read them and
    stop re-reporting the same safe pattern.
  - chains-with relations between findings of the same run

A finding seen again in a later run is matched by its fingerprint (stored as
an alias) and gets a "seen again" line instead of a new note.

What it deliberately leaves out: `evidence`, `payload`, `poc` and
`data_flow`. Evidence for a secrets finding *is* the secret, and payloads do
not belong in a long-lived store. Notes keep the location, verdict, rationale
and mitigation, which is what a future reviewer needs.

Usage:
  sh-memory-record.py <run_dir> [--vault DIR] [--indra PATH] [--dry-run]

  <run_dir>   <TARGET>/.security-harness/<run-id>
  --vault     default: <TARGET>/.security-harness/memory (the target vault)
  --indra     indra binary (default: `indra` on PATH)
  --dry-run   print the indra commands instead of running them

Exit codes: 0 success, 1 error, 2 bad usage.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys

CLASS_NAMES = {
    "access-control": "Broken access control",
    "sqli": "SQL injection",
    "xss": "Cross-site scripting",
    "ssrf": "Server-side request forgery",
    "injection": "Command and code injection",
    "auth": "Authentication and session flaws",
    "deserialization": "Insecure deserialization",
    "path-traversal": "Path traversal",
    "secrets": "Hardcoded secrets",
    "csrf": "Cross-site request forgery",
    "xxe": "XML external entities",
    "open-redirect": "Open redirect",
    "crypto": "Cryptographic misuse",
    "race-conditions": "Race conditions",
    "file-upload": "Unrestricted file upload",
    "dependency": "Vulnerable dependencies",
}

MAX_TEXT = 500


def clip(s, n=MAX_TEXT):
    s = " ".join(str(s or "").split())
    return s if len(s) <= n else s[: n - 3] + "..."


def fingerprint(f):
    fp = str(f.get("fingerprint") or "").strip().lower()
    if len(fp) >= 8 and all(c in "0123456789abcdef" for c in fp):
        return fp[:12]
    # Same recipe shape as sh-pr-review, minus the snippet we do not store.
    cwe = ",".join(f.get("cwe") or [])
    path = str(f.get("file") or "").replace("\\", "/").lstrip("./").lower()
    key = "|".join([cwe, path, str(f.get("class") or ""), str(f.get("title") or "").lower()])
    return hashlib.sha256(key.encode("utf-8")).hexdigest()[:12]


class Indra:
    def __init__(self, binary, vault, dry_run):
        self.base = [binary, "mem", "--vault", vault]
        self.dry = dry_run

    def run(self, *args, check=True):
        cmd = self.base + list(args)
        if self.dry:
            print("indra " + " ".join(_shell_quote(a) for a in cmd[1:]))
            return 0, "{}"
        p = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8")
        if check and p.returncode != 0:
            raise RuntimeError("indra mem %s failed: %s" % (args[0], p.stderr.strip()))
        return p.returncode, p.stdout

    def exists(self, name):
        if self.dry:
            return False
        code, _ = self.run("--json", "show", name, check=False)
        return code == 0


def _shell_quote(s):
    if s and all(c.isalnum() or c in "-_./=:" for c in s):
        return s
    return "'" + s.replace("'", "'\\''") + "'"


def load_findings(run_dir):
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


def class_hub(m, cls, made):
    title = CLASS_NAMES.get(cls, cls)
    if title not in made:
        m.run("add", title, "-t", "vuln-class", "--tag", cls, "--alias", cls, "--upsert")
        made.add(title)
    return title


def file_hub(m, path, made):
    path = str(path or "").replace("\\", "/").strip()
    if not path:
        return None
    title = "file: " + path
    if title not in made:
        m.run("add", title, "-t", "file", "--alias", path, "--folder", "files", "--upsert",
              "--body=Code location seen in security reviews. Backlinks list what was found here.")
        made.add(title)
    return title


def record(run_dir, m):
    run_id = os.path.basename(os.path.normpath(run_dir))
    findings = load_findings(run_dir)
    made = set()
    by_id = {}
    counts = {"new": 0, "seen-again": 0, "false-positive": 0, "skipped": 0}

    for f in findings:
        status = f.get("status")
        if status not in ("verified", "needs-runtime", "false-positive"):
            counts["skipped"] += 1
            continue
        cls = str(f.get("class") or "unknown")
        fp = fingerprint(f)
        alias = "fp-" + fp
        loc = "%s:%s" % (f.get("file") or "?", f.get("line") or "?")
        ver = f.get("verification") or {}

        if m.exists(alias):
            m.run("append", alias, "--", "- Seen again in %s (status: %s, %s)." % (run_id, status, loc))
            by_id[f.get("id")] = alias
            counts["seen-again"] += 1
            continue

        hub = class_hub(m, cls, made)
        fh = file_hub(m, f.get("file"), made)
        lines = [
            "- status: %s" % status,
            "- location: %s" % loc,
            "- first seen: %s (%s)" % (run_id, f.get("id") or "no id"),
        ]
        if f.get("cwe"):
            lines.append("- cwe: %s" % ", ".join(f["cwe"]))
        if ver.get("rationale"):
            lines.append("- verifier: %s" % clip(ver["rationale"]))

        if status == "false-positive":
            title = "False positive: %s" % clip(f.get("title") or loc, 90)
            body = "\n".join(lines + [
                "",
                "Hunters: this pattern was reviewed and is not exploitable. Do not report it again "
                "unless the guard named above changes.",
            ])
            args = ["add", title, "-t", "learning", "--tag", cls, "--tag", "false-positive",
                    "--alias", alias, "--rel", "refines=" + hub, "--source", run_id, "--folder", "learnings", "--body=" + body]
            counts["false-positive"] += 1
        else:
            sev = str(f.get("severity") or "unknown")
            if f.get("why_it_matters"):
                lines.append("- impact: %s" % clip(f["why_it_matters"]))
            if f.get("mitigation"):
                lines.append("- mitigation: %s" % clip(f["mitigation"]))
            title = "%s: %s" % (f.get("id") or cls.upper(), clip(f.get("title") or loc, 90))
            args = ["add", title, "-t", "finding", "--tag", cls, "--tag", sev, "--tag", status,
                    "--alias", alias, "--rel", "instance-of=" + hub, "--source", run_id, "--folder", "findings",
                    "--body=" + "\n".join(lines)]
            counts["new"] += 1
        if fh:
            args += ["--rel", "located-in=" + fh]
        # If an earlier note in this run already took the title, merge into it.
        args.append("--upsert")
        m.run(*args)
        by_id[f.get("id")] = alias

    for f in findings:
        a = by_id.get(f.get("id"))
        for other in f.get("chained_with") or []:
            b = by_id.get(other)
            # chained_with is symmetric; one relation per pair is enough.
            if a and b and a < b:
                m.run("link", a, b, "--rel", "chains-with")

    return counts


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("run_dir")
    ap.add_argument("--vault")
    ap.add_argument("--indra", default="indra")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()

    run_dir = os.path.abspath(a.run_dir)
    vault = a.vault or os.path.join(os.path.dirname(run_dir), "memory")
    binary = a.indra if ("/" in a.indra or os.sep in a.indra) else shutil.which(a.indra)
    if not binary and not a.dry_run:
        print("indra not found on PATH; install it (see the sh-memory skill) or pass --indra", file=sys.stderr)
        return 1
    try:
        counts = record(run_dir, Indra(binary or a.indra, vault, a.dry_run))
    except (OSError, RuntimeError) as e:
        print("sh-memory-record: %s" % e, file=sys.stderr)
        return 1
    print(json.dumps({"vault": vault, **counts}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
