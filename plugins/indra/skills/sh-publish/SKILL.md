---
name: sh-publish
description: "Publish an indra review's findings to the team's tools: Jira issues, Confluence pages, GitHub issues or Notion pages, through their official MCP servers. Use when the user asks to 'file these in Jira', 'open GitHub issues for the findings', 'put the report in Confluence/Notion', 'track these findings', or 'sync findings to <tool>'. Builds a de-duplicated, secret-free plan with a bundled script and creates nothing without the user's confirmation. For bringing Obsidian, Notion or GitHub-wiki knowledge INTO indra memory, use `indra mem import` instead (see the sh-memory skill)."
argument-hint: "[jira|github|confluence|notion] [run dir, default: the latest run] [min severity, default: medium]"
allowed-tools: Read, Bash
---

# indra - Publish findings

Findings that only live in `.security-harness/` get forgotten. This skill puts them where the
team works, without duplicating tickets on every re-run and without leaking anything that
should stay on the machine.

## Rules (read first)

- **Ask before creating anything.** Show the destination (project, repo, space or page) and
  the list of titles, and wait for an explicit yes. Creating tickets is visible to other
  people and hard to undo.
- **Publish only what the plan contains.** Never add `evidence`, `payload`, `poc` or
  `data_flow` from the run, never paste code from the target, and never include secret
  values, tokens, internal hostnames or customer data. The plan script already leaves these
  out and refuses to run if one leaked into a finding's text: fix that finding's text in
  `verified.jsonl` (or skip it), never work around the refusal.
- **De-duplicate by fingerprint.** Every record ends with `indra-fingerprint: <id>`. Search the
  destination for that exact line first. If it exists, add a comment that the finding was seen
  again (with the run id) instead of creating a duplicate.
- **Use the user's own access.** Authentication belongs to the MCP server the user connected;
  never ask for, store or print tokens.
- **Stay in scope.** Don't close, reassign or edit unrelated issues or pages.

## Step 1 - Check the destination is connected

The tools come from the destination's official MCP server. If its tools are not available in
this session, tell the user how to add it, then stop:

| Destination | Add it to Claude Code | Then |
|---|---|---|
| Jira, Confluence | `claude mcp add --transport http atlassian https://mcp.atlassian.com/v2/mcp` (see [Atlassian's setup guide](https://support.atlassian.com/atlassian-rovo-mcp-server/docs/getting-started-with-the-atlassian-remote-mcp-server/)) | run `/mcp` and sign in |
| Notion | `claude mcp add --transport http notion https://mcp.notion.com/mcp` | run `/mcp` and sign in |
| GitHub | `claude mcp add --transport http github https://api.githubcopilot.com/mcp/ -H "Authorization: Bearer <a fine-grained token with Issues: write>"` | `claude mcp list` shows it connected |

Add `--scope user` to make a server available in every project. Endpoints change; if one
fails, check the provider's current documentation.

## Step 2 - Build the plan

```bash
PY="$(command -v python3 || command -v python)"
RUN="<TARGET>/.security-harness/$(cat <TARGET>/.security-harness/latest)"
"$PY" "${CLAUDE_PLUGIN_ROOT}/skills/sh-publish/scripts/sh-publish-plan.py" "$RUN" --to jira
#   --to jira|github|confluence|notion
#   --min-severity medium     (default; info|low|medium|high|critical)
#   --status verified         (default; add needs-runtime to include those)
#   --max 25                  (default; the plan reports how many were held back)
```

The plan is JSON: `records[]` (one per finding: `title`, `body_markdown`, `labels`,
`severity`, `fingerprint`, `dedupe_query`, and for Jira a `priority`), and for Confluence and
Notion also a `page` (one page per run, with its own `indra-run:` marker).

## Step 3 - Confirm, then create

Show the user: the destination, the number of records (and any held back), and the titles.
After an explicit yes:

- **Jira:** for each record, search the project for `dedupe_query` (JQL `text ~ "<fingerprint>"`).
  Found: comment "Seen again in <run>". Not found: create an issue with the record's `title`,
  `body_markdown` as the description, `jira.issue_type`, `jira.priority` and `labels`.
- **GitHub:** search the repo's issues for `dedupe_query`. Found: comment. Not found: create an
  issue with `title`, `body_markdown` and `labels` (create missing labels only if the user
  agreed to new labels).
- **Confluence:** search the space for the page's `dedupe_query`. Found: update that page.
  Not found: create the page under the parent the user named.
- **Notion:** search for the page's `dedupe_query`. Found: update it. Not found: create the
  page under the parent page or database the user named.

## Step 4 - Remember where it went

For each created or updated item, record the link in indra memory so the next run knows the
finding is tracked:

```bash
indra mem --vault "<TARGET>/.security-harness/memory" append "fp-<fingerprint>" -- "- tracked in: <url>"
```

(`fp-<fingerprint>` is the alias the sh-memory recorder gives each finding; skip this step if
the run was not recorded in memory.)

Finish with a short list: created, commented or updated, and skipped, each with its link.

## Obsidian and other Markdown tools

No publishing step is needed: an indra memory vault *is* an Obsidian vault. Open
`<TARGET>/.security-harness/memory` (or the global vault, `indra mem where`) as a vault in
Obsidian. To bring an existing Obsidian vault, Notion export or GitHub wiki into indra memory,
use `indra mem import <folder>` (see the sh-memory skill).
