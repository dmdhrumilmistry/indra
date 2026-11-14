# Reviewing pull requests from the console

The console runs the whole loop of a pull request review: start it, read and triage what it
found, compare it with the last review, remember what was dismissed, and post it.

## 1. Start the review

**Runs > Review a pull request.** Give the PR (a URL, or a number for the checkout's own
repository), the checkout to run in (a clone of the PR's repository, one of the harness's
targets), and a profile: any runner and model, a local one through Ollama included (see
[Local models with Ollama](local-models.md)). Optionally pick the depth (tier 0 to 3, or let the
review choose from the diff), **Deep** for every changed file, and the severity that fails the PR.

This starts `/sh-pr-review <PR> [--tier=N] [--deep] [--fail-on=...] --dry-run` and takes you to
the agent's page, where you can watch it, approve its tool requests and steer it. **Dry run** is
on by default: the review is written to the run's folder, and nothing is posted until you choose
to, below.

## 2. Triage the findings

The run's page lists its findings (`verified.jsonl`), most severe first. Change a finding's
status to verified, needs-runtime or false positive, with a reason (required for a false
positive). indra rewrites that line only, and keeps what it was:

```json
{"id": "F2", "status": "false-positive", "triage": {"from": "verified", "to": "false-positive",
 "reason": "Comments are escaped by the template engine", "at": "...", "by": "console"}}
```

If the file changed under you (the agent is still writing it), the change is refused rather
than applied to the wrong finding.

## 3. Compare with an earlier review

**Compare with** another run of the harness. Findings are matched by fingerprint (the one
`sh-pr-review` uses to dedupe across re-pushes) and sorted into **New**, **Fixed**, **Still
open** and **Changed status**. False positives count as closed, so a finding you dismissed in
the last review does not show up as new.

## 4. Remember, publish, post

**Next steps** on the same page:

- **Record into memory** turns the findings into memory notes and every false positive into a
  learning, with your triage reason in it, so the next review does not raise it again.
- **Publish** builds the tracker records (Jira, GitHub, Confluence, Notion).
- **Post to GitHub** posts the review and sets the commit status (dry run first).

## API

| Call | What it does |
|---|---|
| `POST /api/pr-review` `{pr, target, profile, tier, deep, fail_on, dry_run, node}` | start `/sh-pr-review` (the same checks as Launch) |
| `GET /api/run/findings?key=` | a run's findings with their fingerprints |
| `POST /api/run/triage` `{key, index, fingerprint, status, reason}` | change one finding's status |
| `GET /api/run/compare?key=&with=` | new, fixed, still open and changed findings between two runs |
