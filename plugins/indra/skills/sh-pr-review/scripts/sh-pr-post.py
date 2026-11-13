#!/usr/bin/env python3
"""Post an sh-pr-review result to a GitHub PR, and set its commit status.

Why a script: posting is a multi-step operation with a mandatory tail. The
review goes up, the status must follow, receipts must be written, and a 422 has
to be recovered from by moving a comment rather than nudging a line number. A
model doing this by hand will sometimes do four of those five steps and report
success, and the missing one is usually the status, which is the part a branch
protection rule depends on.

The guarantee this script provides: **it never exits leaving the commit status
at `pending`.** If the review fails to post, the status is still set, to `error`,
with a description saying the tooling failed rather than accusing the PR of a
vulnerability it never found.

Requires `gh`, already authenticated.

  post   --repo R --pr N --payload review-payload.json --verdict verdict.json

Exit codes: 0 posted, 1 failed (status still set), 2 bad usage.
"""

import argparse
import io
import json
import os
import subprocess
import sys
import tempfile
import time

MAX_DESCRIPTION = 140
VALID_STATES = ("success", "failure", "error", "pending")


def run_gh(args, payload_path=None):
    """Run gh, returning (ok, parsed_or_text, raw_stderr)."""
    cmd = ["gh"] + args
    if payload_path:
        cmd += ["--input", payload_path]
    try:
        p = subprocess.run(cmd, capture_output=True, text=True)
    except FileNotFoundError:
        return False, None, "gh not found on PATH"
    body = p.stdout.strip()
    if p.returncode != 0:
        return False, body, (p.stderr or "").strip()
    try:
        return True, json.loads(body) if body else {}, ""
    except ValueError:
        return True, body, ""


def load(path, what):
    try:
        with io.open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except Exception as e:
        sys.stderr.write("error: cannot read %s (%s): %s\n" % (what, path, e))
        sys.exit(2)


def set_status(repo, sha, state, description, context, target_url, dry_run):
    """Set the commit status. Returns True on success."""
    if state not in VALID_STATES:
        sys.stderr.write("error: invalid status state %r\n" % state)
        return False
    # GitHub truncates hard at 140; truncating here keeps the visible text ours.
    if len(description) > MAX_DESCRIPTION:
        description = description[:MAX_DESCRIPTION - 3] + "..."
    args = ["api", "repos/%s/statuses/%s" % (repo, sha), "--method", "POST",
            "-f", "state=%s" % state,
            "-f", "context=%s" % context,
            "-f", "description=%s" % description]
    if target_url:
        args += ["-f", "target_url=%s" % target_url]
    if dry_run:
        print("  [dry-run] status -> %s: %s" % (state, description))
        return True
    ok, _out, err = run_gh(args)
    if ok:
        print("  status -> %s: %s" % (state, description))
    else:
        sys.stderr.write("  FAILED to set status: %s\n" % err)
    return ok


def is_line_error(err_text, out):
    blob = "%s %s" % (err_text or "", json.dumps(out) if not isinstance(out, str) else out)
    blob = blob.lower()
    return "not part of the diff" in blob or "pull_request_review_thread.line" in blob


def demote_comments_to_body(payload):
    """Move every inline comment into the body. Used once, after a 422.

    A 422 means the anchoring map is wrong. The one thing never to do is shift a
    line number until the API accepts it: that posts a real finding against a
    line that has nothing to do with it.
    """
    comments = payload.get("comments") or []
    if not comments:
        return payload, 0
    lines = ["", "### Findings without a diff anchor", "",
             "_These could not be anchored to a line in this diff, so they are "
             "listed here rather than posted inline._", ""]
    for c in comments:
        loc = "`%s`" % c.get("path", "?")
        if c.get("line"):
            loc += " line %s" % c["line"]
        lines.append("- %s" % loc)
        for ln in (c.get("body") or "").splitlines():
            lines.append("  %s" % ln)
        lines.append("")
    payload = dict(payload)
    payload["body"] = (payload.get("body") or "") + "\n".join(lines)
    payload["comments"] = []
    return payload, len(comments)


def write_receipts(path, rows):
    if not path or not rows:
        return
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with io.open(path, "a", encoding="utf-8", newline="") as fh:
        for r in rows:
            fh.write(json.dumps(r, sort_keys=True) + "\n")


def cmd_post(a):
    payload = load(a.payload, "payload")
    verdict = load(a.verdict, "verdict") if a.verdict else {}

    sha = payload.get("commit_id") or verdict.get("head_sha")
    if not sha:
        sys.stderr.write("error: no commit_id in payload and no head_sha in verdict.\n"
                         "The review must be pinned to the SHA that was analysed.\n")
        return 2

    # Never let a bot block or approve a merge. The commit status is the gate,
    # and it is the one branch protection can actually see.
    event = payload.get("event", "COMMENT")
    if event != "COMMENT":
        sys.stderr.write("error: review event is %r; only COMMENT is allowed.\n" % event)
        return 2

    state = verdict.get("state", "error")
    description = verdict.get("description") or "Security review completed."
    context = a.status_context
    at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    receipts = []

    n_inline = len(payload.get("comments") or [])
    print("Posting review to %s#%s (%d inline + 1 summary), pinned to %s"
          % (a.repo, a.pr, n_inline, sha[:8]))

    if a.dry_run:
        print("  [dry-run] nothing sent")
        set_status(a.repo, sha, state, description, context, a.target_url, True)
        return 0

    endpoint = "repos/%s/pulls/%s/reviews" % (a.repo, a.pr)

    def send(pl):
        fd, tmp = tempfile.mkstemp(suffix=".json")
        os.close(fd)
        with io.open(tmp, "w", encoding="utf-8", newline="") as fh:
            fh.write(json.dumps(pl))
        try:
            return run_gh(["api", endpoint, "--method", "POST"], payload_path=tmp)
        finally:
            os.unlink(tmp)

    ok, out, err = send(payload)

    # One recovery attempt, and only for the anchoring case.
    if not ok and is_line_error(err, out) and n_inline:
        print("  422: a comment is outside the diff. Moving inline comments to the "
              "body and retrying once (never adjusting a line number).")
        payload, moved = demote_comments_to_body(payload)
        receipts.append({"action": "body_only", "reason": "not_anchorable_422",
                         "count": moved, "at": at})
        ok, out, err = send(payload)

    if not ok:
        low = (err or "").lower()
        if "403" in low or "forbidden" in low:
            hint = "the token cannot write to this PR (needs pull-requests: write)"
        elif "404" in low:
            hint = "repo or PR not found, or no access"
        else:
            hint = "see the error above"
        sys.stderr.write("  FAILED to post review: %s\n  (%s)\n" % (err, hint))
        receipts.append({"action": "failed", "error": err[:500], "at": at})
        write_receipts(a.receipts, receipts)
        # The mandatory tail: a computed verdict that never reaches the status
        # is worse than no check at all.
        set_status(a.repo, sha, "error",
                   "Review completed but could not be posted. See the run log.",
                   context, a.target_url, False)
        return 1

    url = out.get("html_url") if isinstance(out, dict) else None
    print("  posted: %s" % (url or "(no url returned)"))
    receipts.append({"action": "posted_review", "review_url": url,
                     "inline": len(payload.get("comments") or []), "at": at})

    if a.no_status:
        print("  --no-status: commit status not set")
    else:
        # target_url points at the review when we have one, so a red check leads
        # straight to the reason for it.
        set_status(a.repo, sha, state, description, context,
                   a.target_url or url, False)

    write_receipts(a.receipts, receipts)
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd")

    p = sub.add_parser("post", help="post the review and set the commit status")
    p.add_argument("--repo", required=True, help="owner/repo")
    p.add_argument("--pr", required=True)
    p.add_argument("--payload", required=True, help="review-payload.json")
    p.add_argument("--verdict", help="verdict.json (state + description)")
    p.add_argument("--status-context", default="security/pr-review")
    p.add_argument("--target-url", default=None)
    p.add_argument("--receipts", default=None, help="append receipts.jsonl here")
    p.add_argument("--no-status", action="store_true")
    p.add_argument("--dry-run", action="store_true")
    p.set_defaults(fn=cmd_post)

    a = ap.parse_args(argv)
    if not getattr(a, "fn", None):
        ap.print_help()
        return 2
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
