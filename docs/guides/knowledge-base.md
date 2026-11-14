# Knowledge base and integrations

indra keeps what reviews learn in **memory vaults**: folders of Markdown notes linked with
`[[wikilinks]]`, searchable and graph-aware (`indra mem`). This guide shows how to bring
existing knowledge in, how to use a vault with Obsidian, and how to push findings out to Jira,
Confluence, GitHub and Notion.

```text
 Obsidian vault --+                          +--> Jira issues
 Notion export  --+--> indra mem import -->  |    Confluence pages
 GitHub wiki    --+      [ memory vault ] ---+--> GitHub issues      (sh-publish skill,
 Markdown docs  --+       (Obsidian-ready)   +--> Notion pages        via official MCP servers)
```

## The vaults

- **Per project:** `<target>/.security-harness/memory`, written by reviews (findings, the
  files they touched, vulnerability classes, fixes).
- **Global:** `indra mem where` prints it; use it for knowledge that applies everywhere.

```bash
indra mem --vault "$V" recall "payment webhooks"   # graph-aware context for an agent
indra mem --vault "$V" search "csrf"               # ranked full-text search
indra mem --vault "$V" tree                        # the graph in the terminal
indra mem --vault "$V" export -o graph.html        # an interactive graph, offline
```

Reviews read memory before hunting and record what they found afterwards (the `sh-memory`
skill); pass `memory:off` to skip that for one review.

Any agent the console starts can also be given the relevant notes up front, and propose what it
learned for you to accept into a vault; the console's **Knowledge** page edits vaults in place.
See [Context, memory and learnings](context-and-learnings.md).

## Import

`indra mem import <folder>` copies notes into a vault and rewrites their links as wikilinks. It
never modifies the source, skips notes that already exist (add `--overwrite` to replace them),
and `--dry-run` prints what it would do without writing. Each note gets a
`source: <kind>:<path>` field so you can trace it back.

| Option | Effect |
|---|---|
| `--from obsidian\|notion\|github-wiki\|markdown` | force the source kind (default: detected) |
| `--folder F` | put the notes in subfolder `F` of the vault |
| `--tag T` | tag every imported note (repeatable) |
| `--overwrite` | replace notes that already exist |
| `--dry-run` | report only |
| `--json` | machine-readable report |

### Obsidian

```bash
indra mem --vault "$V" import ~/Documents/MyVault --folder obsidian
```

Detected by its `.obsidian/` folder. Notes keep their names and their links as they are; the
`.obsidian/` settings and attachments are not copied.

### Notion

In Notion: **... > Export > Markdown & CSV** (include subpages), then unzip it:

```bash
unzip Export-*.zip -d notion-export
indra mem --vault "$V" import notion-export --folder notion --tag notion
```

Notion appends a 32-character id to every file name (`Threat model 1a2b...e9.md`). indra
strips it, slugifies the name, and turns links between pages
(`[how we sign in](Auth%20flows%201a2b....md)`) into `[[auth-flows|how we sign in]]`
(just `[[auth-flows]]` when the link text is the page title).

### GitHub wiki

Every GitHub wiki is a git repository:

```bash
git clone https://github.com/<owner>/<repo>.wiki.git
indra mem --vault "$V" import <repo>.wiki --folder wiki
```

Detected by `Home.md` or `_Sidebar.md`. Page names like `Deploy-Process` become the title
"Deploy Process", and both wiki links (`[[Text|Deploy-Process]]`) and Markdown links are
converted.

### Any Markdown

```bash
indra mem --vault "$V" import ./docs --from markdown --tag docs
```

Relative links to other `.md` files in the folder become wikilinks. Links that point outside
the folder are left as they are and counted as *unresolved* in the report; code blocks are
never touched.

After any import, `indra mem --vault "$V" check` lists links that still do not resolve.

## Obsidian, both ways

A vault *is* an Obsidian vault: open `<target>/.security-harness/memory` (or the global vault)
in Obsidian with **Open folder as vault**. Graph view, backlinks and search work on indra's
notes, and typed relations (`mitigated-by:: [[Parameterized queries]]`) are Dataview inline
fields. Notes you write in Obsidian are read by `indra mem` and by the next review. Avoid
editing a note in Obsidian while a review is writing to the same vault.

## Publish findings: Jira, Confluence, GitHub, Notion

The `sh-publish` skill files a review's findings where your team tracks work. Ask your agent,
for example "file the high findings from the last review in Jira project SEC" or "put the
report in Confluence under Security".

It works through each tool's **official MCP server**, signed in as you, so indra stores no
tokens. Add the one you need to Claude Code once:

```bash
# Jira and Confluence (Atlassian Rovo MCP server), then run /mcp and sign in
claude mcp add --transport http atlassian https://mcp.atlassian.com/v2/mcp
# Notion, then run /mcp and sign in
claude mcp add --transport http notion https://mcp.notion.com/mcp
# GitHub, with a fine-grained token that can write issues on the repository
claude mcp add --transport http github https://api.githubcopilot.com/mcp/ \
  -H "Authorization: Bearer $GITHUB_PAT"
```

Add `--scope user` to make a server available in every project. These endpoints are the
providers' own; if one changes, follow the provider's documentation.

What the skill does:

1. **Builds a plan** with a bundled script: findings at or above a severity (default medium),
   verified only by default, at most 25. Jira gets one issue per finding with a priority;
   GitHub one issue per finding; Confluence and Notion one page per review.
2. **Asks you first.** It shows the destination and every title and creates nothing until you
   say yes.
3. **Never duplicates.** Each record ends with `indra-fingerprint: <id>`, stable across runs.
   It searches for that line first and comments ("seen again in run X") instead of opening a
   second ticket.
4. **Never leaks.** Records hold location, severity, CWE, impact, why it was confirmed, and the
   fix. Evidence, payloads, proofs of concept and data-flow traces stay in the local run
   directory, and the planner refuses to run if one of them shows up in a finding's text.
5. **Remembers** each ticket's link on the finding's memory note, so the next review knows it is
   tracked.

To preview a plan yourself:

```bash
indra publish plan <target>/.security-harness/<run> --to jira --min-severity high
```

or open the run in the console (`indra serve`, Runs, then the run) and use **Next steps**:
**Publish** builds the same plan, and **Record into memory** does what `indra mem record-run` does.
