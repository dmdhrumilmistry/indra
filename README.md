<p align="center">
  <img src="assets/banner.svg" alt="indra: security agents, graph memory, and an agent console" width="100%">
</p>

<p align="center">
  <a href="https://github.com/dmdhrumilmistry/indra/actions/workflows/indra.yml"><img src="https://github.com/dmdhrumilmistry/indra/actions/workflows/indra.yml/badge.svg" alt="indra CLI build"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-4f46e5" alt="MIT license"></a>
  <img src="https://img.shields.io/badge/Claude%20Code-plugin-f59e0b" alt="Claude Code plugin">
  <img src="https://img.shields.io/badge/Gemini%20CLI-extension-0ea5e9" alt="Gemini CLI extension">
  <img src="https://img.shields.io/badge/CLI-Linux%20%7C%20macOS%20%7C%20Windows-1e1b4b" alt="Linux, macOS and Windows">
</p>

<p align="center">
  <b><a href="https://dmdhrumilmistry.github.io/indra/">Website</a></b> &nbsp;&middot;&nbsp;
  <b><a href="https://dmdhrumilmistry.github.io/indra/docs/">Docs</a></b> &nbsp;&middot;&nbsp;
  <a href="#install">Install</a> &nbsp;&middot;&nbsp;
  <a href="#usage">Usage</a> &nbsp;&middot;&nbsp;
  <a href="#indra-memory-and-agent-console">Memory &amp; console</a> &nbsp;&middot;&nbsp;
  <a href="#pull-request-review">PR review</a> &nbsp;&middot;&nbsp;
  <a href="tools/indra/README.md">CLI docs</a>
</p>

**indra** is a multi-agent **application-security review harness** for Claude Code and other AI agents.
One request runs a team of specialist agents that **map** a codebase, **hunt** vulnerabilities with a
per-class knowledge base, **chain** findings into escalations, **verify** real impact, and **report** to
README, JSON, SARIF, HTML and PDF. Every run **remembers** what it learned, and the `indra` CLI gives you
a web console to launch agents and watch them work.

```
/plugin marketplace add dmdhrumilmistry/indra
/plugin install indra@indra
/indra full security review of ./api
```

<table>
  <tr>
    <td width="33%" valign="top">
      <h3>A review team</h3>
      Recon, one hunter per vulnerability class in parallel, an exploit chainer, a verifier that refutes
      first, and a reporter. Each stage runs on the model its job needs.
    </td>
    <td width="33%" valign="top">
      <h3>Memory that compounds</h3>
      <code>indra mem</code> keeps past findings, known guards and confirmed false positives as a graph of
      Markdown notes, and hands them to the next run's hunters.
    </td>
    <td width="33%" valign="top">
      <h3>Watch it work</h3>
      <code>indra serve</code> shows every subagent, its tool calls, refused permissions and cost, live,
      on this machine or remote ones over SSH, and <code>indra run</code> orchestrates them.
    </td>
  </tr>
</table>

<p align="center">
  <img src="assets/screenshots/live-agents.png" alt="The indra console following a live review: subagents, their tool calls, and a refused permission" width="100%">
</p>

> **Scope:** indra performs **static** analysis (source review, data-flow tracing, PoC and payload
> construction) on code you own or are authorized to test. It does not attack live third-party systems.

## What's inside

```
indra/                           # this repository: a plugin marketplace + the indra CLI
├── plugins/indra/               # the indra plugin
│   ├── skills/
│   │   ├── indra                # /indra - the entry point, routes any appsec request
│   │   ├── sh-security-review   # the pipeline orchestrator (stages 0-6)
│   │   ├── sh-pr-review         # review one pull request, post inline comments
│   │   ├── sh-memory            # long-term graph memory across runs
│   │   ├── sh-publish           # findings to Jira, Confluence, GitHub issues, Notion
│   │   └── sh-kb-*  (15)        # per-vulnerability-class knowledge bases
│   ├── agents/
│   │   ├── sh-recon             # map: code graph, stack, SBOM/CVE, attack surface
│   │   ├── sh-hunter            # find: source-to-sink hunting, one per class, in parallel
│   │   ├── sh-chainer           # escalate: combine findings into attack chains
│   │   ├── sh-verifier          # confirm: refute first, then prove impact with a PoC
│   │   └── sh-reporter          # deliver: README/JSON/SARIF/HTML/PDF/doc
│   └── references/              # shared contracts (finding schema, SARIF map, rubrics)
├── tools/indra/                 # the indra CLI (Rust): graph memory + agent web console
├── site/                        # the project website (GitHub Pages)
└── assets/                      # logo, banner and screenshots
```

### Vulnerability classes covered (the `sh-kb-*` skills)

access-control (IDOR/BOLA/priv-esc) &middot; sqli &middot; xss &middot; ssrf &middot; injection (cmd/code/SSTI/LDAP) &middot;
auth (session/JWT) &middot; deserialization &middot; path-traversal (LFI/RFI) &middot; secrets &middot; csrf &middot; xxe &middot;
open-redirect &middot; crypto &middot; race-conditions &middot; file-upload. Dependency CVEs and the SBOM are handled
by the recon stage.

## Install

The harness ships for several agents. Full packaging details and the release
checklist are in [`docs/DISTRIBUTION.md`](docs/DISTRIBUTION.md).

**Claude Code** - add this repo as a plugin marketplace and install the plugin:

```
/plugin marketplace add dmdhrumilmistry/indra
/plugin install indra@indra
```

`/plugin marketplace add` accepts any of: a GitHub `owner/repo` (as above), a full git URL
(`https://github.com/dmdhrumilmistry/indra.git`), or a local path to a clone
(e.g. `/plugin marketplace add ./indra` from the directory containing your checkout).
Then run `/plugin install indra@indra` and reload when prompted.

**Gemini CLI** - a native extension, manifest at the repo root:

```bash
gemini extensions install https://github.com/dmdhrumilmistry/indra
```

**opencode, Codex, or any [agentskills.io](https://agentskills.io) agent** - copy the
skills into a discovery directory. Codex additionally picks up `AGENTS.md` on its own:

```bash
git clone https://github.com/dmdhrumilmistry/indra
cd indra
indra skills install --to agents       # ~/.agents/skills
indra skills install --to opencode     # ~/.config/opencode/skills
# without indra: mkdir -p ~/.agents/skills && cp -R skills/* ~/.agents/skills/
```

**Graft is installed and set up automatically by the pipeline.** Stage 0 runs `npm install -g
@nanonets/graft` if it's missing (requires Node/npm), then `graft init <target> --no-agents --no-global`
to register the Graft MCP server and freshness hooks for the target repo. The graph itself (`<target>/graft/`,
auto-gitignored) is built during recon. To pre-install manually: `npm install -g @nanonets/graft`. Graft's
structural build is free and needs no API key; the optional `--deep` LLM pass uses `GRAFT_API_KEY` /
`GRAFT_PROVIDER` / `GRAFT_MODEL` when set.

**The other tools are also auto-installed by Stage 0 when missing** (via whatever package manager is on the
machine - winget/choco/scoop, brew, apt, npm/pip/go - see `references/tooling-setup.md`). Installs are
announced, prefer no-elevation methods, and never block the run: anything that can't be installed is simply
marked unavailable and the pipeline falls back. Stage 0 installs only what fills a missing capability group:

- SBOM: [`syft`](https://github.com/anchore/syft) · CVEs: one of [`grype`](https://github.com/anchore/grype)
  (preferred), [`trivy`](https://github.com/aquasecurity/trivy), or [`osv-scanner`](https://github.com/google/osv-scanner)
- Reports: `wkhtmltopdf` or `pandoc` (for PDF/DOCX); otherwise you get `report.html` (or a headless-Chrome PDF).

All of them are optional - the pipeline degrades gracefully to native search + manifest parsing if none install.

## Usage

Ask `/indra` in plain language:

```
/indra full security review of ./api
/indra find SQLi and IDOR in src/
/indra just map this codebase        # recon only
```

Or call the pipeline directly:

```
/sh-security-review . classes:sqli,access-control,ssrf depth:deep
/sh-security-review . stage:report       # regenerate reports for the latest run
```

### Model & cost control

Each stage runs on a model matched to its cognitive load, so tokens are spent where discovery quality
actually depends on them and saved on mechanical work. **This is the default - no arguments needed.**

| Stage | Default model |
|---|---|
| recon | `sonnet` |
| hunt (per class) | `haiku` for pattern classes (secrets, crypto, open-redirect, csrf) · `sonnet` for source→sink tracing (sqli, xss, ssrf, injection, path-traversal, xxe, file-upload, auth) · `opus` for deep-logic classes (access-control, race-conditions, deserialization) |
| chain | `opus` |
| verify | `opus` (the precision gate - kept strong) |
| report | `haiku` |

Override with the `models:` argument (passed through the router too):

```
/sh-security-review .                         # default tiered map above
/sh-security-review . models:max              # every stage + hunter on opus (max quality, max cost)
/sh-security-review . models:cheap            # aggressive downshift (trades some verify precision)
/sh-security-review . models:verify=opus,hunt=sonnet          # per-stage overrides
/sh-security-review . models:report=sonnet,hunt.pattern=sonnet  # per-hunter-tier override
```

Stages: `setup, recon, hunt, chain, verify, report`. Models: `opus, sonnet, haiku, inherit`. For `hunt`,
a bare model flattens all hunters to it; `hunt.pattern` / `hunt.trace` / `hunt.logic` target one tier.
Other token savers are built in: recon only spawns hunters for classes with real attack surface, hunters
query the Graft graph instead of reading whole files, and `findings.json`/SARIF are generated by a
deterministic script rather than the model.

### Output

Everything lands under `<target>/.security-harness/<run-id>/`:

- `recon.md`, `codebase-map.json` - the map (stack, SBOM, CVEs, attack surface).
- `findings.jsonl` → `chains.md` → `verified.jsonl` - the working state (see `references/state-files.md`).
- `reports/` - `README.md`, `findings.json`, `results.sarif`, `report.html`, `report.pdf` (+ `report.docx`).

Each published finding carries a payload, a PoC, the verification verdict, CWE/OWASP ids, CVSS, and a
code-level mitigation.

## indra: memory and agent console

[`indra`](tools/indra/README.md) is one small Rust CLI (~2.5 MB, Linux / macOS / Windows) that
ships in this repo. It is named for Indra's net: an endless net with a jewel at every knot, each
reflecting all the others.

```bash
cargo install --locked --git https://github.com/dmdhrumilmistry/indra indra
```

or download a binary from an `indra-v*` [release](https://github.com/dmdhrumilmistry/indra/releases).

### Long-term memory (`indra mem`)

Without memory, every review starts from zero: it re-flags the sanitizer that was cleared last
week and re-learns the same framework quirk on every codebase. The `sh-memory` skill gives the
harness a memory that persists across runs, stored as a graph of small Markdown notes.

- **After recon**, the pipeline recalls what earlier runs learned about each class it is about to
  hunt (past findings, known guards, confirmed false positives) and hands it to that hunter.
- **After the report**, it records verified findings and false positives (never evidence,
  payloads or secrets) and up to five judged learnings.
- **Two vaults:** `<target>/.security-harness/memory/` for codebase-specific knowledge (gitignored
  with the rest), and a per-user global vault for portable techniques only.

Memory is optional. Without `indra` installed, or with `memory:off`, the pipeline runs exactly as
before. You can also use it directly:

```
/sh-memory what do we know about SSRF in this codebase
indra mem --vault .security-harness/memory tree          # the memory graph, in the terminal
indra mem recall "django raw sql"                        # what do we know about this?
```

A vault is an ordinary Obsidian-compatible folder, so you can also open it in Obsidian.

**Bring knowledge in, send findings out.** `indra mem import` copies an Obsidian vault, a Notion
export, a GitHub wiki or any Markdown folder into a vault, turning its links into wikilinks. The
`sh-publish` skill files a review's findings in Jira, Confluence, GitHub issues or Notion
through their official MCP servers: it asks before creating anything, never opens a duplicate,
and never sends evidence, payloads or secrets. See
[Knowledge base and integrations](docs/guides/knowledge-base.md).

### Agent console (`indra serve`)

A local web console for the harness, and for any other agent harness:

```bash
cd indra && indra serve --target ~/code/my-api     # prints a URL with an access token
```

- **Agents, skills, prompts:** read and edit every definition the harness is made of, and see
  what each one uses and is used by.
- **Graph map:** stages run agents, agents spawn agents and load the `sh-kb-*` skills,
  everything reads its references.
- **Memory:** the memory vaults drawn as graphs.
- **Runs:** each `.security-harness/run-*` and `pr-*` with stage progress, findings per class,
  severity and status counts, and every file it wrote, live. From a finished run, record it into
  memory, build the publish plan for Jira, GitHub, Confluence or Notion, or post a PR review.
- **Tools:** the PR review cache, local run metrics, the skills mirror and install, and the
  site build: everything `indra review`, `indra skills` and `indra dev` do, from the browser.
- **Launch:** start a review through Claude Code (or Codex, Gemini CLI, your own runner) and
  watch it work: every subagent it spawns, each one's tool calls, refused permissions, and the
  final cost. Stop stops the whole process tree.
- **Agent control:** approve or deny each tool request, switch an agent between permission modes
  (manual, acceptEdits, auto, plan, ...) and models while it runs, interrupt it, or send it a
  follow-up. **Profiles** bundle runner, model, mode, tools and environment, so a strong model can
  plan and the model you pick (or a local one through Ollama) does the work. A profile can also
  keep Claude on Anthropic and hand just its subagents to a local model, through indra's model
  router, which keeps your credentials off the model server.
- **Marketplace:** browse Claude Code plugin marketplaces (Anthropic's skills and official
  directory built in, add any other) and install their skills, agents and commands for Claude
  Code, Codex or a harness, or as native Claude Code plugins; update and remove them later.
- **Plan and delegate:** a strong model (Opus, Fable, a big Codex model) plans; you review the
  steps; cheaper or self-hosted models (Haiku, Ollama) carry each one out; a reviewer checks the
  result. Each role is a profile, so any runner fits any role, and the cost per role is shown.
- **Knowledge:** agents can start with the memory notes relevant to their task and finish by
  proposing what they learned; you review proposals in an inbox and accept them into memory.
  Vaults are searched, edited and linked in the console, and each agent shows how full its
  context window is.
- **Terminals:** `indra attach` follows and steers an agent from your own terminal, and
  `indra serve --shell` adds a real terminal in the browser: a shell or Claude Code in a target,
  or an agent's own session.
- **Secrets:** tokens agents need live in an encrypted store, are referenced from profiles
  (`JIRA_TOKEN = "secret:jira"`), are scoped to profiles and machines, reach the agent only in
  memory at launch, and are redacted from everything it prints.
- **Board:** work items in To do, Running, Review, Done and Failed. Run one with a profile; it
  moves to Review when the agent finishes. Connect Jira, GitHub Issues or ClickUp to import
  tickets, run them (automatically if you like) and report results and state back. See
  [the board guide](docs/guides/board.md).
- **Workflow editor:** build workflows as a graph by dragging tasks, reusable components and
  whole workflows (as one step) onto a canvas and connecting them; it saves plain TOML.
- **Settings:** configure everything from the browser: runners, remote nodes, workflows,
  presets, harnesses, capacity, or the raw `indra.toml`. Each save is checked and applies at
  once, and your comments in the file are kept.
- **Pivot:** redirect an agent mid-run. *Continue* resumes its own session with your new
  instruction, so it keeps everything it learned; *Fresh* starts a new agent briefed with what
  the first one was asked and found. Keep the original running to try two directions at once.

<table>
  <tr>
    <td width="33%"><img src="assets/screenshots/graph-map.png" alt="Graph map of the harness"><br><sub><b>Graph map:</b> stages, agents, skills and prompts</sub></td>
    <td width="33%"><img src="assets/screenshots/run.png" alt="A run being monitored"><br><sub><b>Runs:</b> stage progress and findings per class</sub></td>
    <td width="33%"><img src="assets/screenshots/memory.png" alt="The memory graph"><br><sub><b>Memory:</b> findings, guards and false positives</sub></td>
  </tr>
</table>

### Agents on many machines (`indra nodes`, `indra run`)

Run agents on this machine and on any machine you can SSH into, and orchestrate them:

- **Nodes:** a machine dials in with `indra node connect` (no open port, no SSH; a direct
  WebRTC channel when possible, relayed otherwise; it reconnects on its own and loses no output),
  or is reached over SSH as a fallback. Either way it runs only its own configured runners.
- **Workflows:** a graph of agent tasks across nodes, run in dependency order within each
  node's capacity, with retries, skip-on-failure and cancel. Run one with `indra run nightly`
  (cron and CI friendly) or from the console.
- **Fan-out:** one prompt across any set of repositories on any machines, from the console.

<table>
  <tr>
    <td width="50%"><img src="assets/screenshots/nodes.png" alt="The Nodes page: this machine, a reachable build box, and an unreachable node"><br><sub><b>Nodes:</b> health, runners and capacity of every machine</sub></td>
    <td width="50%"><img src="assets/screenshots/workflow.png" alt="A workflow running across two machines"><br><sub><b>Workflows:</b> tasks across machines, in dependency order</sub></td>
  </tr>
</table>

See [Nodes and orchestration](tools/indra/README.md#nodes-and-orchestration) for the setup and
how remote execution stays safe.

**Design:** [architecture](docs/architecture.md) (agent control, profiles, secrets, transport,
the board, graph workflows). **Guides:** [handling agent processes](docs/guides/processes.md) (watching, stopping, logs,
capacity, troubleshooting), [pivoting agents](docs/guides/pivoting.md),
[local models with Ollama](docs/guides/local-models.md), and
[knowledge base and integrations](docs/guides/knowledge-base.md).

<p align="center"><img src="assets/screenshots/pivot.png" alt="Pivoting an agent: continue its session with a new instruction" width="720"></p>

<p align="center"><img src="assets/screenshots/settings.png" alt="The Settings page: editing a workflow and its tasks in the browser" width="720"></p>

The security harness is the default profile. A plain Claude Code project (`.claude/agents`,
`.claude/skills`) is picked up automatically, and a custom harness is a few lines of TOML.
It is locked down like a remote shell: loopback only, a token on every call, DNS-rebinding and
CSRF checks, no shell, and file access confined to the harness. `indra ls` and `indra check`
give the same picture in the terminal.

## Pull request review

`sh-pr-review` reviews a single pull request rather than a whole codebase, posts the
result as inline comments **on the PR itself**, and sets a `security/pr-review` commit
status that branch protection can enforce.

**Run it from your own machine, on any PR you can read.** Install the plugin and ask:

```
review https://github.com/acme/api/pull/128
review PR 42
security review this PR
```

**Paste a PR link** and it reviews that PR in that repository, cloning it to a temporary
directory first, because the hunters read files and not just the patch. Nothing is
written into the repo you are working in.

**Pass a bare number** and it resolves against **the repo you are currently in**, the one
`git remote` points at. Pass nothing and it takes the open PR for your current branch.

Phase 7 prints the findings and the verdict and **asks before posting anything** - a
decline is a normal outcome, and the payload stays on disk for you to post later.

Before spending any analysis it checks whether you can actually write to the target repo,
so reviewing someone else's project tells you up front that posting will 403 rather than
discovering it ten minutes in.

Three properties make it usable as a merge gate rather than noise:

- **Only what the PR is responsible for fails it.** Every finding carries a `pr_impact`
  of `introduced`, `aggravated`, or `pre_existing`. The first two block; `pre_existing`
  is reported and never blocks. Blocking a merge over code the author never wrote is how
  a required check gets deleted, so when a hunter is unsure between `aggravated` and
  `pre_existing`, it must pick `pre_existing`.
- **Depth follows risk.** Triage runs first, in the orchestrator, with no subagents. It
  maps changed paths and added-line sink tokens onto the same class slugs the `sh-kb-*`
  bases use, then picks a tier. Tier 0 (no security-relevant change) launches nothing at
  all and still sets the status. Tier 3 runs the full pipeline.
- **Re-pushes do not spam.** Each comment carries a hidden fingerprint computed without
  line numbers, so a re-review adds only what is new and lists what was fixed as
  "Resolved since the last review".

| Verdict | Status | When |
|---|---|---|
| fail | `failure` | introduced or aggravated finding at or above `--fail-on` (default `medium`), confidence >= 80 |
| warn | `success` | nothing introduced or aggravated; pre-existing findings reported |
| pass | `success` | no findings, or triage stopped at Tier 0 |
| error | `error` | the review could not complete |

`warn` reports `success` on purpose: a warning that blocks a merge is a failure with
extra steps, and teams respond by removing the check. `error` is kept distinct from
`failure` so a broken run never looks like a vulnerability it did not find.

The review event is always `COMMENT`, never `REQUEST_CHANGES` or `APPROVE`. The commit
status is the enforcement mechanism, and it is the one branch protection reads.

**Scope:** the skill writes to the pull request and the commit status, and nowhere else.
It opens no issues and creates nothing in any external tracker.

### Re-reviews are incremental

A PR gets reviewed once per push, so the second review has to be cheaper than the first or
the tool becomes something people turn off.

**Dedup happens before the spending, not before the posting.** The fingerprints already on
the PR are read in Phase 1 and handed to the hunters and the verifier. Finding a duplicate
at the end would mean the most expensive model in the pipeline had already re-confirmed a
conclusion that was written on the PR the whole time. This needs no cache: the state lives
in the PR, so it works on a cold machine and in CI.

**A local cache makes the rest incremental.** `indra review cache` stores each run's file
hashes, findings and verdicts under your OS cache directory (never in the repo, since a
cross-repo review runs in a temp clone that gets deleted). The next review re-hunts only
files whose content actually changed, reuses verdicts for findings that are unchanged, and
reuses the recon map if nothing it covers moved.

**A base-branch merge costs nothing.** Merging `main` into a PR branch changes the head
SHA and nothing the author wrote, but a commit status is pinned to a SHA, so the required
check silently disappears from the new head. When the PR's own files are byte-identical
*and* the base delta touches nothing the findings depend on, the previous verdict is
re-stamped onto the new SHA with no agents launched at all. That last condition is what
makes it safe: a base merge that deletes a sanitizer leaves every PR file unchanged while
turning a safe line into an exploitable one.

Invalidation is deliberately conservative, because a stale entry in a security tool does
not make it slow, it makes it **wrong**. The cache key hashes every `sh-kb-*` knowledge
base, so a KB update invalidates every cached finding - a cached "clean" must never
suppress the finding that update was written to catch. Model identity, skill version, file
content and a 7-day TTL all invalidate too, and an unspecified model is treated as a miss.

`--no-cache` disables it, `--refresh-cache` re-baselines, and `run.md` records per phase
what was launched, reused and skipped, so a cache that quietly stops hitting is visible
rather than assumed.

### Posting, and where things are written

**The review and the commit status are posted by default.** A review that was computed and
never delivered helped nobody. `--confirm` restores a prompt before posting, `--dry-run`
sends nothing, `--no-status` posts the review but leaves the commit status alone.

Posting goes through `indra review post` rather than hand-built API calls, because it
is a multi-step operation with a mandatory tail: review, then status, then receipts, with
a 422 recovered by moving the comment rather than shifting a line number. It
**never exits leaving the status at `pending`** - if the review cannot be posted it still
sets `error`, saying the tooling failed rather than accusing the PR.

Inline comments are reserved for findings at **medium or above** with confidence >= 80.
Low-severity findings go in the collapsed body section, so a low-priority finding showing
up with zero inline comments is the policy working, not a failure.

### Local run metrics

Every run records what it cost, so "the cache is working" and "reviews got slower" stop
being matters of opinion.

```bash
indra review metrics path      # where records live
indra review metrics report    # aggregate, by model
indra review metrics purge --older-than-days 30
```

Two append-only JSONL files - `runs.jsonl` (repo, PR, tier, verdict, totals, the flags you
passed) and `events.jsonl` (one line per phase or agent: model, tokens, duration, outcome,
whether it was reused from cache). JSONL so a crashed run still leaves valid lines above
the crash.

| Platform | Metrics | Cache |
|---|---|---|
| **Linux / BSD** | `$XDG_DATA_HOME/indra/metrics`<br>default `~/.local/share/indra/metrics` | `$XDG_CACHE_HOME/indra`<br>default `~/.cache/indra` |
| macOS | `~/Library/Application Support/indra/metrics` | `~/Library/Caches/indra` |
| Windows | `%LOCALAPPDATA%\indra\metrics` | `%LOCALAPPDATA%\indra\cache` |

Linux follows the XDG Base Directory spec, so both honour `XDG_DATA_HOME` and
`XDG_CACHE_HOME` when set and fall back to `~/.local/share` and `~/.cache` when they are
not. Override either directly with `SH_METRICS_DIR` and `SH_REVIEW_CACHE_DIR`.

**One binary, no interpreter.** The cache, the metrics, posting, the memory recorder and
the publish planner are all `indra` subcommands, so a review needs no Python on any
platform; install `indra` once (see [the indra CLI](tools/indra/README.md#install)).

**Strictly local.** Neither the cache nor the metrics contain any network code or reporting endpoint.
Anything token-shaped is redacted before it is written, because local files get pasted
into issues.

### Running it unattended

Optional, and a separate decision from using the skill. Run it by hand on your own PRs
for a while first, so you know what it says about your codebase before it says it in
front of your team.

When you are ready, "Enforcing the check on a repository" in
[`references/pr-review-mapping.md`](plugins/indra/references/pr-review-mapping.md)
has a copy-paste workflow for **your** repo, plus the fail-safe that stops a dead job
leaving a required check stuck on `pending`.

The threshold stays at the default `medium`. Pre-existing findings never block a merge,
so an unscanned codebase does not produce a wall of red on day one - only what a PR
actually introduces or aggravates can fail it.

## How it works

1. **Setup** - probe available tools, define scope, create the run directory.
2. **Recon** (`sh-recon`) - build the Graft graph; detect stack/versions; SBOM + CVEs; enumerate entry
   points, trust boundaries, and dangerous sinks.
3. **Hunt** (`sh-hunter` ×N, parallel) - one hunter per relevant class loads its `sh-kb-*` knowledge base,
   traces attacker input from source to sink, and records candidates. A shared **attempts ledger** stops
   agents from repeating each other's probes.
   With `indra` available, the orchestrator first recalls each class's past findings, guards and
   false positives from long-term memory and hands them to that hunter.
4. **Chain** (`sh-chainer`) - compose findings into higher-severity attack paths.
5. **Verify** (`sh-verifier`) - refute first, then confirm exploitability from evidence, build PoCs, assign
   CVSS, and cut false positives.
6. **Report** (`sh-reporter`) - produce the deliverables.
7. **Remember** (optional, `sh-memory`) - record verified findings, false positives and a few
   learnings into the long-term memory graph for the next run.

Subagents share nothing but files; the contract is in `plugins/indra/references/state-files.md`.

## Extending

Add a new vulnerability class by creating `skills/sh-kb-<class>/SKILL.md` following the shared template
(When to hunt · Sources & sinks · Detection recipe · Payloads/PoC · False-positive filters · CWE/OWASP ·
Chaining hints · Mitigation), then add its slug to the `class` enum in `references/finding-schema.json`
and the routing table in `skills/indra/SKILL.md`.

## Automated knowledge-base updates

A scheduled GitHub Action (`.github/workflows/update-knowledge-base.yml`) keeps the `sh-kb-*` knowledge
bases fresh. **Every alternate day** (and on manual `workflow_dispatch`), it runs an agent to distill new,
reputable public security research - OWASP, PortSwigger Research, CWE/CAPEC, NIST, MDN, curated GitHub
repos, and public HackerOne disclosures - into small, well-sourced improvements. A **second, adversarial
reviewer agent** then scans the resulting diff for malicious/injected content, and the PR is
**auto-merged only if that reviewer approves**.

### Two workflows, three jobs

PR creation is deliberately separated from review and merge, so the thing that writes
the diff is never the thing that decides to ship it.

**Stage 1 - [`update-knowledge-base.yml`](.github/workflows/update-knowledge-base.yml)**
(scheduled or manual). One job, `create-pr`:

1. **Generate** - the agent edits the KB from allowlisted sources. No commit, no push.
2. **Open PR** - a deterministic step opens (or updates) a PR on the `automated/kb-update`
   branch, labeled `awaiting-review`.
3. **Hand off** - on a successful PR creation it dispatches stage 2 with the PR number.

**Stage 2 - [`kb-review-and-merge.yml`](.github/workflows/kb-review-and-merge.yml)**
(dispatched by stage 1, or run by hand against any automated PR). Two jobs:

- **`review`** - a *separate* agent run inspects the diff **adversarially** for
  prompt-injection artifacts, out-of-scope edits, secrets/exfil, PII, weaponized
  exploits, off-allowlist sourcing, or house-style violations. It has **no web and no
  shell**, and **fails closed**: anything suspicious, any uncertainty, or a missing
  verdict file → REJECT. The verdict is posted as a PR comment and drives the label.
- **`merge`** - runs **only** on `APPROVE`, and merges the PR. A `REJECT` skips it and
  the `blocked` job reports why.

> **Why a dispatch rather than a `pull_request` trigger:** a PR opened by `GITHUB_TOKEN`
> does not trigger `pull_request` workflows. `workflow_dispatch` is one of the two events
> exempt from that recursion guard, so stage 1 can hand off reliably.

**Auto-merge means an approving agent lands code in `main`.** The controls on that:

- The merge job refuses any PR that is closed, from a fork, or whose head branch is
  outside `automated/*` (`ALLOWED_HEAD_PREFIX` in the workflow).
- It prefers GitHub's own auto-merge, so **branch protection still applies**. With a rule
  on `main` requiring an approving review, the PR queues and waits for a human instead of
  merging. It falls back to an immediate merge only on repos where auto-merge is off.
- Set the `auto_merge` input to `false` on a manual run to review without merging.
- The reviewer prompt tells the agent its verdict is binding, not advisory.

> Requires the repo setting **"Allow GitHub Actions to create and approve pull requests"** (Settings →
> Actions → General → Workflow permissions) so the workflow can open the PR. If you want a human in the
> loop despite auto-merge, protect `main` with a branch-protection rule requiring a pull request and at
> least one approving review - the auto-merge path honours it.

### Pluggable agents

Both stages run through [`.github/actions/ai-agent`](.github/actions/ai-agent/action.yml),
a composite action that dispatches to whichever agent you configure. Claude Code, OpenAI
Codex, Gemini CLI, and an escape hatch for anything else:

| `agent` | Runs | Credential |
|---|---|---|
| `claude` (default) | `anthropics/claude-code-action@v1` | `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` |
| `codex` | `codex exec --full-auto` | `OPENAI_API_KEY` |
| `gemini` | `gemini --yolo --prompt` | `GEMINI_API_KEY` |
| `custom` | your `KB_AGENT_INSTALL` / `KB_AGENT_COMMAND` | whatever it needs |

Pick per run from the `workflow_dispatch` inputs, or set repo variables to change the
default: `KB_AGENT` and `KB_MODEL` for the generator, `KB_REVIEW_AGENT` and
`KB_REVIEW_MODEL` for the reviewer. Running the generator and the reviewer on **different
agents** is a meaningful hardening step: an injection tuned for one model is less likely
to land on a second, independent one.

For `agent: custom`, set `KB_AGENT_COMMAND` to a shell command. The prompt is written to
the file named by `$AGENT_PROMPT_FILE`, and `$AGENT_MODEL` carries the model input.

Prompt-injection defenses, since the generator reads the open web:

- **Domain allowlist.** `WebFetch` is restricted to the trusted domains in
  `.github/kb-update/trusted-sources.md` (mirrored in the workflow's `--allowedTools`). `WebSearch` can
  discover URLs, but only allowlisted domains can actually be fetched.
- **Content is data, not commands.** The task prompt (`.github/kb-update/prompt.md`) instructs Claude to
  treat every fetched byte as untrusted reference material and to ignore any instructions embedded in a
  page - HackerOne report bodies (user-generated) are flagged as the highest-risk tier.
- **No shell, no push on the generator; the reviewer is the gate.** The generator can only edit files.
  The independent reviewer (`.github/kb-update/review-prompt.md`) is what stands between fetched content
  and `main` - nothing merges without its explicit approval.
- **Different agents for generator and reviewer.** Optional, and the strongest version of the gate: set
  `KB_AGENT` and `KB_REVIEW_AGENT` to two different engines.

**Setup:**

- Add the credential for whichever agent you use (Settings → Secrets and variables → Actions):
  **`CLAUDE_CODE_OAUTH_TOKEN`** (default), `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or `GEMINI_API_KEY`.
  The OAuth token authenticates against **your Claude subscription's usage limits** rather than a metered
  API key - generate it locally with `claude setup-token` (requires an active Claude Pro/Max subscription)
  and paste the result.
- Enable **"Allow GitHub Actions to create and approve pull requests"** (Settings → Actions → General →
  Workflow permissions) so the workflow can open its PR. Recommended: add a branch-protection rule on `main`
  requiring a PR and an approving review, so no automated change can land without a human even with
  auto-merge on.
- To change which sources are allowed, edit the allowlist in `trusted-sources.md` **and** the matching
  `WebFetch(domain:...)` entries in the workflow - keep the two in sync.

Each run records what it did in `.github/kb-update/last-run-summary.md`.

## Roadmap

- ~~Codex / Cursor mirror wiring.~~
  ✅ Shipped: `AGENTS.md`, a Gemini CLI extension, and `indra skills install`
  for `.agents/skills` and opencode. See [`docs/DISTRIBUTION.md`](docs/DISTRIBUTION.md).
- ~~Optional live-fetch augmentation of knowledge bases (PortSwigger/OWASP/CWE) on top of curated references.~~
  ✅ Shipped as the scheduled knowledge-base updater above.
- Optional DAST bridge for runtime confirmation of `needs-runtime` findings.

## License

MIT
