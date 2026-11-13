# Graft usage guide (for recon & hunters)

[Graft](https://github.com/NanoNets/Graft) builds a queryable structural graph of a codebase with
tree-sitter. It is the fast source->sink navigation layer for this harness. It does **not** do
SBOM/CVE/version detection - recon handles that separately.

Verified against **graft 0.12.0**. Structural commands (`build`, `ask`, `grep`, `callers`, `map`,
`skeleton`, `blast`) cost `$0` and need **no API key**. Only `build --deep` calls an LLM.

## Install & set up (done by the pipeline's Stage 0 - see sh-security-review)
```
npm install -g @nanonets/graft        # global install (the flow auto-runs this if graft is missing)
graft version                          # presence/version check (Stage 0 capability probe)
graft init <target> --no-agents --no-global   # wire Graft into Claude Code for <target>, scoped to the repo
```
`graft init` (Claude Code wiring) writes into the **target repo**: `.mcp.json` (registers the `graft` MCP
server), `.claude/settings.json` + `.claude/helpers/*.cjs` (statusline + freshness hooks), and
`.claude/skills/graft/SKILL.md`. `--no-global` avoids any writes outside the repo; `--dry-run` previews
every file first. This is optional convenience - the pipeline itself calls the `graft` **CLI** directly and
works without MCP wiring. `graft/` (the graph + cache) is added to `.gitignore` automatically on first build.

### MCP mode (optional)
`graft mcp <dir>` serves the graph over stdio MCP, exposing tools `graft_find_code`, `graft_trace_calls`,
`graft_find_all`, `graft_file_api`, `graft_repo_map`, `graft_check_freshness`. When `graft init` has
registered it in `.mcp.json`, these tools become available in Claude Code sessions on that repo and can be
used interchangeably with the CLI commands below.

## Build the graph once (recon, Stage 1)
```
graft build <target>                   # structural graph, $0, no key  (DEFAULT)
graft build <target> --deep            # + LLM concept map & per-symbol summaries/crux (needs a key)
graft build <target> -e .py .ts        # restrict to extensions;  --lsp adds compiler-grade call edges
```
`--deep` requires LLM credentials via env: `GRAFT_PROVIDER` (openai|anthropic), `GRAFT_MODEL`,
`GRAFT_API_KEY` (or `--provider/--model/--api-key`). If no key is configured, use the structural build
(still fully sufficient for source->sink tracing). Output lands in `<target>/graft/`:
`.graph/wiring.json` (the graph), `INDEX.md`, and one `*.md` card per file, plus a `.cache/`.

## Query commands hunters use most  (add `--json` for machine-readable output)
```
graft grep "<regex>" <dir> --json                 # every occurrence, grouped by enclosing symbol, ranked by coupling
graft grep "<literal>" <dir> --fixed -i --json     # literal + case-insensitive
graft ask "<question>" <dir> --source -n 8         # ranked nodes with source inlined (--full for whole spans)
graft callers <symbol> <dir> --json                # who calls/references it (default direction: in)
graft callers <symbol> <dir> --direction out --json         # what it calls (callees)
graft callers <symbol> <dir> -d all --json                  # transitive closure = full blast radius
graft skeleton <file> <dir>                        # signatures-only API surface of one file
graft map <dir> --json                             # directory clusters, hubs, hotspots (orientation)
graft blast <dir> --format markdown                # blast radius of a diff (CI/PR comment; Mermaid diagram)
```
Useful flags on the query commands: `--in <path>` narrows to a subtree; `--no-refresh` answers from the
graph as-is (skip the freshness re-check). `graft ask` returns at most `-n/--limit` hits (default 8) - if it
returns few, switch tool (`grep`/`skeleton`/`callers`) rather than re-asking with new wording.

## Source->sink tracing recipe (the core hunter loop)
1. From the class KB, get the list of **sink symbols** (e.g. `execute`, `child_process.exec`).
2. `graft grep "<sink-regex>" <dir> --json` to locate every sink; note the enclosing `symbol.id` and line.
3. `graft callers <enclosing-symbol> <dir> --json` (optionally `-d all`) to walk **up** toward an entry
   point listed in `codebase-map.json` (an untrusted source).
4. If an unbroken path exists from an entry point to the sink with no sanitizer in between, it is a
   candidate. Use `graft ask "how does <param> flow into <sink>" <dir> --source` to confirm the hops.
5. Record the path in the finding's `data_flow[]` (each hop: file, line, what happens to the value).

Example (verified): `grep "execute"` finds the sink in `get_user`; `callers get_user` shows `handler`
calls it; `handler` reads `request.args.get("id")` -> tainted value reaches `cur.execute("... '%s'" % uid)`.

## Graceful degradation (Graft absent - check capabilities.json)
Replace the commands above with native tools:
- `graft grep "<re>"` -> Grep tool (ripgrep) with the same regex.
- `graft callers <sym>` -> Grep for call sites of `<sym>`, then read enclosing functions.
- `graft ask` / `graft map` -> Explore subagent over the relevant directories.
Note in `recon.md` and each finding that tracing used the native fallback (slightly lower confidence).
