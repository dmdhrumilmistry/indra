# Reviewing pull requests from the console

The console runs the whole loop of a pull request review: start it, read and triage what it
found, compare it with the last review, remember what was dismissed, and post it.

## 1. Start the review

**Runs > Run a workflow**, then **pr-review** (a built-in workflow; **security-review**, **threat-model** and the
workflows you configure are in the same list). Give the PR (a URL, or a number for the
checkout's own repository), the checkout to run in (a clone of the PR's repository, one of the
harness's targets), and a profile: any runner and model, a local one through Ollama included
(see [Local models with Ollama](local-models.md)). Optionally pick the depth (tier 0 to 3, or let
the review choose from the diff), **Deep** for every changed file, and the severity that fails
the PR. From a terminal: `indra run pr-review --input pr=<PR> --input target=<checkout>`.

This runs `/sh-pr-review <PR> [--tier=N] [--deep] [--fail-on=...] --dry-run` as the workflow's
one task; open its agent from the workflow run to watch it, approve its tool requests and steer
it. **Dry run** is
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

Each triage also becomes a **lesson** for the codebase (Knowledge > Lessons): a verified finding
teaches "confirmed", a false positive "do not re-report without new mechanics", with your reason.
A lesson keeps the findings it came from; changing your mind withdraws that evidence, and a lesson
with none left is retired. The next review hands its hunters the lessons for their class (`indra
lessons --target <repo> --class <class>`), labelled as prior outcomes, not instructions. You can
adopt or retire a lesson by hand.

**Coverage**, above the findings, shows which entry points the hunters probed (from the run's
attempts ledger and its findings) and which they did not, unauthenticated ones first, and how many
probes each class logged.

## Retest a finding

**Retest** on a finding starts an agent with the `sh-retest` skill. It walks the finding's data flow
again in the code as it is now and records one verdict with why: **reproducible**, **fixed** (it
names the guard that now breaks the path) or **cannot confirm**. Inside an engagement, it can
instead replay the recorded traffic the finding cites (`indra traffic replay`) and compare the new
responses with the cited quotes. The verdict goes to the run's `retests.jsonl` (the findings and
reports are never changed), shows on the finding, and becomes a lesson: a verified finding found
fixed teaches "recheck before reporting". From the command line:

```bash
indra review retest --run .security-harness/run-1 --finding SH-SQLI-001 \
  --verdict fixed --why "search() now binds its parameters (app/db.py:12)"
```

**Check evidence** (on runs whose findings cite recorded traffic) checks every citation on the machine
that recorded it: see [traffic as evidence](engagements.md#traffic-as-evidence).

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
| `GET /api/run/coverage?key=` | entry points and classes the hunters probed, and those they did not |
| `POST /api/run/retest` `{key, fingerprint, engagement?, profile?}` | start an agent that retests one finding |
| `GET /api/run/evidence?key=` | check the recorded traffic the findings cite |
| `GET /api/lessons[?target=&class=]`, `POST /api/lessons/{id}` `{status}` | lessons with their evidence; adopt, retire or reopen one |
