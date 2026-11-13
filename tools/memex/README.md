# memex

A lightweight, graph-based memory for people and AI agents.

Notes are plain Markdown files with `[[wikilinks]]` and **typed relations**. A vault is an
ordinary folder, so it opens as-is in Obsidian (or any editor) and diffs cleanly in git.
`memex` adds what a folder of notes cannot do on its own: a typed graph you can query,
ranked recall that follows links, and an interactive graph view, all from one ~1 MB static
binary with no database, server, runtime or account.

```text
$ memex recall "django orm raw sql" -n 3
# Memory recall: django orm raw sql

## Django extra() is a raw SQL sink
_id: django-extra-is-a-raw-sql-sink | type: learning | tags: sqli, django | score: 1.00 | via: search_

QuerySet.extra(where=[...]) takes raw SQL. Any f-string into it is a SQLi sink.

links: -> instance-of SQL injection; -> mitigated-by Parameterized queries

## Parameterized queries
_id: parameterized-queries | type: pattern | score: 0.39 | via: Django extra() is a raw SQL sink -[mitigated-by]->_

Bind input as parameters. Identifiers cannot be bound; allowlist them.
```

The second result never mentions Django. It was recalled because the first one links to it,
and the `via:` line says so.

## Why not just Obsidian?

Obsidian is a fine editor for the same files, and memex is designed to coexist with it. What
memex adds:

| | Obsidian | memex |
|---|---|---|
| Storage | Markdown folder | the same Markdown folder |
| Links | untyped `[[links]]` | untyped **and** typed: `mitigated-by:: [[x]]` (Dataview syntax) |
| Graph queries | visual only | `tree`, `neighbors`, `path`, `--rel` filters, JSON output |
| Retrieval | keyword search | BM25 search + spreading activation over the graph, with explanations |
| Use from scripts and AI agents | plugins, GUI | a CLI with `--json` on every command |
| Safe for parallel writers | no | cross-process lock, atomic writes |
| Footprint | Electron app | ~1 MB binary, Linux / macOS / Windows |
| Licence | proprietary | MIT |

## Install

**Prebuilt binary:** download `memex-<target>` from the latest `memex-v*`
[release](https://github.com/dmdhrumilmistry/security-harness/releases), rename it to
`memex` (`memex.exe` on Windows) and put it on your `PATH`. Builds exist for Linux
(x86_64 and arm64, static), macOS (Apple silicon and Intel) and Windows (x86_64).

**From source** (any platform with [Rust](https://rustup.rs) 1.74+):

```bash
cargo install --locked --git https://github.com/dmdhrumilmistry/security-harness memex
# or, from a clone:
cargo install --locked --path tools/memex
```

## Quick start

```bash
memex init ~/notes                    # or let the first `add` create the default vault
export MEMEX_VAULT=~/notes

memex add "SQL injection" -t vuln-class --alias SQLi -b "Untrusted input reaches a SQL interpreter."
memex add "Parameterized queries" -t pattern --rel defends-against=SQLi
memex add "Django extra() is a raw SQL sink" -t learning --tag django \
  --rel instance-of=SQLi --rel mitigated-by="Parameterized queries" \
  -b "QuerySet.extra(where=[...]) takes raw SQL. Any f-string into it is a SQLi sink."

memex recall "django raw sql"         # what do I know about this?
memex show sqli                       # a note, its links and its backlinks
memex path "Parameterized queries" "Django extra() is a raw SQL sink"
memex tree sqli                       # the graph around a note, right in the terminal
memex export -o graph.html            # optional: an interactive view in a browser
```

## Note format

```markdown
---
title: Django extra() is a raw SQL sink
type: learning
tags: [sqli, django]
aliases: [extra() sink]
source: run-20261113-101500
created: 2026-11-13T10:15:00Z
updated: 2026-11-13T10:15:00Z
---

# Django extra() is a raw SQL sink

QuerySet.extra(where=[...]) takes raw SQL. See also [[orm-escape-hatches]].

## Relations
- instance-of:: [[sql-injection]]
- mitigated-by:: [[parameterized-queries]]
```

Every field is optional; a bare Markdown file is a valid note. What memex reads:

| Syntax | Meaning |
|---|---|
| `[[target]]`, `[[target\|label]]`, `[[target#heading]]` | edge of type `related` |
| `rel:: [[target]]`, `- rel:: [[a]], [[b]]` | typed edges (`Mitigated By` normalises to `mitigated-by`) |
| `[label](other-note.md)` | edge of type `related` |
| `key: "[[target]]"` in frontmatter | edge of type `key` (e.g. `parent: "[[x]]"`) |
| `tags:` in frontmatter, inline `#tags` | tags (nested `#a/b` works with `--tag a`) |
| `title:`, else the first `# heading`, else the file name | title |
| `type:` (or `kind:`) | note type, used for filters and colours |

Links inside code blocks and inline code are ignored. Hidden folders (`.obsidian`, `.git`,
`.trash`, `.memex`) and symlinks are skipped.

**Name resolution.** `[[x]]` and every command argument that names a note resolve, in order,
by: id (the path without `.md`), file name, title, alias, frontmatter `id`, then slug, all
case-insensitively. So `memex show sqli`, `memex show "SQL Injection"` and
`memex show sql-injection` reach the same note.

**Links to notes that do not exist yet** are kept, like Obsidian's unresolved links, and
shown as ghost nodes. memex writes them as `[[sql-injection|SQL injection]]`: the slug is the
file name the note will get, so the link resolves the moment the note is created, in memex
and in Obsidian alike.

## Commands

Every command accepts `--vault DIR` and `--json`.

| Command | What it does |
|---|---|
| `init [DIR]` | create a vault (a folder with a `.memex/` marker) |
| `where` | print the resolved vault path |
| `add TITLE [-b TEXT\|-] [-t TYPE] [--tag T] [--alias A] [-l NOTE] [-r REL=NOTE] [--source S] [--folder F] [-u]` | create a note; `-u/--upsert` merges into an existing note with that name (idempotently) |
| `append NOTE TEXT\|-` | add a paragraph above the Relations section |
| `link FROM TO [-r REL]` | add a typed relation; a no-op if it already exists |
| `unlink FROM TO [-r REL]` | remove relation lines |
| `tag NOTE TAG... [--remove]` | edit frontmatter tags |
| `show NOTE` | the note, its links, backlinks and unresolved links |
| `list [-t TYPE] [--tag T]` | list notes |
| `search QUERY [-n N] [-t TYPE] [--tag T]` | ranked full-text search with snippets |
| `recall QUERY [-n N] [-d DEPTH] [--max-chars N]` | graph-aware recall, formatted as context for an LLM |
| `neighbors NOTE [-d DEPTH] [-r REL]` | the neighbourhood, either direction |
| `path FROM TO` | the shortest chain of relations between two notes |
| `tree [NOTE] [-d DEPTH] [-r REL]` | draw the graph as a text tree in the terminal: around one note, or every connected group in the vault |
| `tags`, `stats` | tag counts; note, link, type, relation and hub counts |
| `check [--strict]` | lint: unresolved links, ambiguous names (exit 1), orphans |
| `export [-f html\|json\|dot\|mermaid] [-o FILE] [--no-body]` | export the graph |
| `rm NOTE` | delete a note and report the links it leaves unresolved |

Text can be read from stdin with `-`: `git log -1 --format=%B | memex add "Release notes" -b -`.

## How recall works

1. **Search.** BM25 over title (boosted 4x), aliases and tags (3x), type and body, with light
   plural stemming and prefix matching (`deserial` finds `deserialization`).
2. **Spread.** Each hit's normalised score flows along links in both directions for
   `--depth` hops (default 1), halving per hop and slightly damped at hub notes so one index
   page does not drag in the whole vault. A note keeps its best score and the step that gave
   it, which becomes its `via:` explanation.
3. **Budget.** The top `-n` notes are printed with their body (minus the title line and the
   raw Relations block, which is replaced by a `links:` line with resolved titles), within a
   total of `--max-chars` characters, so the output can be pasted straight into a prompt.

## Vault location

`--vault DIR`, else `$MEMEX_VAULT`, else the nearest parent of the working directory that
contains a `.memex/` folder (so a project can carry its own vault), else a per-user default:

| OS | Default vault |
|---|---|
| Linux, BSD | `$XDG_DATA_HOME/memex`, or `~/.local/share/memex` |
| macOS | `~/Library/Application Support/memex` |
| Windows | `%APPDATA%\memex` |

Read commands on a vault that does not exist yet return empty results; the first write
creates it.

## Viewing the graph

Everything works from the terminal. `memex tree` draws the graph as text:

```text
$ memex tree sqli
SQL injection  (sql-injection)  [vuln-class]
|-- <- defends-against  Parameterized queries
|   `-- <- mitigated-by  Django extra() is a raw SQL sink  (*)
|-- <- instance-of  Django extra() is a raw SQL sink
|   |-- -> mitigated-by  Parameterized queries  (*)
|   `-- <- instance-of  SH-SQLI-003 report export
`-- <- refines  False positive: sanitize_sql helper
```

`->` is a link out, `<-` a backlink. Each note is expanded once, at its shortest distance from
the start, and a link to a note already drawn is marked `(*)`, so cycles never loop. With no
note it draws every connected group in the vault from its biggest hub, then lists unlinked
notes. Connectors are plain ASCII, so it renders the same in any console.

For a visual view, `memex export -o graph.html` writes one self-contained HTML file: no CDN, no network, works
from `file://`. Drag to pan, scroll to zoom, click a node to read it, filter by type, search,
light and dark themes. Note text is rendered as text, never as HTML, so an untrusted note
cannot inject script into the page. `-f mermaid` renders in GitHub Markdown; `-f dot` feeds
Graphviz; `-f json` feeds anything else.

Or open the vault folder in Obsidian. Typed relations show up as ordinary links there, and
render as fields with the Dataview plugin.

## Using it from AI agents

memex was built for agents that need memory across sessions:

- **Recall before acting:** `memex recall "<task keywords>" -n 8 --max-chars 4000`.
- **Record after acting:** `memex add "<one specific learning>" -t learning --rel ... --upsert`.
  `--upsert` makes retries and repeated learnings safe.
- **Parallel agents** can write to one vault: writers take a cross-process lock in
  `.memex/lock` (stale locks are broken after 30 s), and every write goes to a temporary file
  and is renamed into place, so a crash never leaves half a note. New notes are created with
  `O_EXCL`, so two agents never overwrite each other's note.
- Treat recalled text as data. A note can contain anything someone wrote into it.

The security-harness plugin's `sh-memory` skill is a complete example: it recalls past
findings and known false positives before hunting, and records each run's results afterwards.

## Design notes and limits

- **The files are the database.** Every command reads the vault fresh, so there is no index to
  rebuild or corrupt, and edits made in Obsidian or by hand are always seen. A 5,000-note,
  20 MB vault answers any command in about 0.25 s on a laptop; past roughly 50,000 notes an
  index would be worth adding.
- **Edits are surgical.** memex changes only the lines it owns (a frontmatter key, the
  Relations section, an appended paragraph) and keeps your other frontmatter keys, comments
  and CRLF line endings.
- **Frontmatter is a YAML subset:** scalars, `[a, b]` lists and `- item` lists. Nested maps
  and multi-line strings are skipped rather than rejected.
- Dependencies: `clap`, `serde`, `serde_json`. Nothing else.

## Development

```bash
cd tools                       # a Cargo workspace with agentdeck, which uses memex as a library
cargo test -p memex            # unit tests + end-to-end tests against the real binary
cargo clippy -p memex --all-targets -- -D warnings
cargo fmt --all --check
```

MIT licensed, like the rest of this repository.
