# Context, memory and learnings

Agents forget everything between runs. indra closes that loop: an agent can **start** with the
notes most relevant to its task, **finish** by proposing what it learned, and **you** decide what
becomes memory for the next one. Everything happens in the console's **Knowledge** page, on
the Launch page and on each process's page.

```text
             +-------------------- memory vaults --------------------+
             |  notes with typed links (indra mem), Obsidian-ready    |
             +---^---------------------------------------------+-----+
                 | accept (with its source)                    | recall (graph-aware)
     +-----------+-----------+                     +-----------v-----------+
     |   learnings inbox     |  <-- proposals --   |  agent: prompt +      |
     |   review, edit, link  |      at the end     |  relevant memory      |
     +-----------------------+                     +-----------------------+
```

## Manage memory in the console

**Knowledge** lists the memory vaults of the selected harness (the harness's `memory` globs, for
example `{target}/.security-harness/memory` or `~/notes`), with how many notes and links each has
and how many links point nowhere. A vault the harness names but that does not exist yet can be
created from there.

| Tab | What you can do |
|---|---|
| **Notes** | Search (ranked, with snippets), filter by type, read a note with its links and backlinks, edit its Markdown (a save is refused if the file changed since you opened it), link it to another note with a typed relation, delete it, or write a new one. |
| **Learnings** | The inbox (below). |
| **Graph** | The vault as a graph; a node opens its note. |
| **Health** | Dangling links (to notes that do not exist) and orphans (notes nothing links to, which recall reaches only by search). |

The notes are the same files `indra mem` and Obsidian use, so the CLI, the console and your
editor all see the same memory.

**Import and export.** On the Notes tab:

- **Import** takes Markdown files or a whole folder: an Obsidian vault, an unzipped Notion
  export, a cloned GitHub wiki, any folder of Markdown, or an indra bundle exported from another
  vault. Pick the source (or let indra detect it), optionally a folder to put the notes in and
  tags to add, then **Preview** to see what would be imported, skipped (notes that exist already,
  unless you tick **Replace**) and left unresolved, and **Import**. Links are converted to
  `[[wikilinks]]`, as `indra mem import` does.
- **Export** downloads the vault as an **indra bundle** (every note's file, to import into another
  vault or console), an offline **HTML** graph, the graph as **JSON**, **Mermaid** or **DOT**.
- A note's **Download .md** saves that one file.

The learnings inbox exports to JSON and imports from it too, and **Clear accepted** tidies the
list (the notes stay in memory).

## Start an agent with memory

On **Launch**, tick **Give it relevant memory**. indra recalls the notes that best match the
prompt from every vault of the harness (search, then a step through the graph, so a fix linked
to a finding comes along) and appends them after the prompt, within a budget of six notes and
about 4,000 characters. The prompt stays first, so a slash command such as `/sh-security-review`
still works. **Preview what it would get** shows the notes before you launch.

The agent's page lists the notes it was given (each opens in Knowledge). To make it the default,
set it on a profile; workflow tasks and board items that use the profile get it too:

```toml
[[profile]]
name    = "worker"
runner  = "claude"
model   = "sonnet"
memory  = true      # start with the relevant notes
reflect = true      # propose learnings before finishing
```

Recall runs on the hub, so agents on remote nodes get the same memory.

## Collect learnings

Tick **Ask for learnings at the end** (or set `reflect = true` on a profile). When a Claude Code
agent has finished its work, and before indra closes its input, it is asked one more question:
what it learned that a future agent should know, as a short JSON list (title, text with the
evidence, tags), or nothing. Its answer goes to **Knowledge > Learnings**, each item tagged with
where it came from: the process, its session, runner, profile and directory. The badge on
Knowledge counts what is waiting.

You can add learnings yourself too: **Save a learning from this agent** on a process page,
**Add a learning** in the inbox, or `/learn <text>` in `indra attach`.

Nothing reaches memory on its own. For each proposal:

- **Accept into memory** writes it to the vault you pick as a `learning` note, with its source,
  tags and any links you add. A learning whose title matches an existing note is merged into it
  rather than duplicated.
- **Save edits** keeps your changes and leaves it in the inbox.
- **Discard** drops it.

Reflection costs one short extra turn, and it applies to agents driven over Claude Code's
control channel (`format = "claude-stream-json"` runners). An agent that is stopped or fails is
not asked.

## Watch an agent's context

A process page shows a **context** meter: the tokens in the agent's context at its last turn
against its model's window (as Claude Code reports it; 200k when it does not). When it runs
high, the agent may compact soon, so this is a good moment to pivot it to a fresh session with a
brief ([Pivoting agents](pivoting.md)). `indra attach` shows the same line under the agent's
header.

## API

| Call | What it does |
|---|---|
| `GET /api/knowledge?h=` | the harness's vaults, with counts and the number of learnings waiting |
| `POST /api/knowledge/init` `{h, i}` | create vault `i` |
| `GET /api/knowledge/notes?h=&i=&q=&type=&tag=` | notes, ranked when `q` is given |
| `GET /api/knowledge/note?h=&i=&id=` | one note, its relations, its file text and hash |
| `PUT /api/knowledge/note` `{h, i, id, text, hash}` | replace the note (409 if it changed) |
| `POST /api/knowledge/notes` `{h, i, title, body, type, tags, links, source}` | create a note |
| `DELETE /api/knowledge/note?h=&i=&id=` | delete a note |
| `POST /api/knowledge/link` `{h, i, from, to, rel}` | add a typed relation |
| `GET /api/knowledge/recall?h=&q=` | what an agent launched with prompt `q` would be given |
| `GET /api/knowledge/health?h=&i=` | dangling links and orphans |
| `GET /api/knowledge/export?h=&i=&format=` | download: `bundle`, `html`, `json`, `mermaid` or `dot` |
| `POST /api/knowledge/import` `{h, i, files: [{path, text}], from, folder, tags, overwrite, dry_run}` | import uploaded files (`dry_run` previews) |
| `POST /api/learnings/import` `{items: [{title, body, tags}]}` | add learnings to the inbox |
| `GET /api/learnings`, `POST /api/learnings` `{title, body, tags, proc}` | the inbox; propose one |
| `PATCH /api/learnings/{id}`, `DELETE /api/learnings/{id}` | edit or discard |
| `POST /api/learnings/{id}/accept` `{h, i, links}` | write it into vault `i` |
| `POST /api/procs` `{..., memory: true, reflect: true}` | launch with memory, and reflection |

Writes are refused when the console is read-only. The inbox lives in `learnings.json` in the
data directory.
