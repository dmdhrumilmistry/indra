# Changelog

What each release of the indra CLI adds, in short. Each release page on GitHub starts with its
section from this file.

## v0.19.0

- **Codex and Gemini CLI in engagements.** Run an agent in an engagement as Claude Code, Codex or
  Gemini CLI. indra installs its guard as each one's hooks: `--settings` for Claude Code, `-c
  hooks...` for Codex (with `--dangerously-bypass-hook-trust`), a private system settings file for
  Gemini CLI. Scope, window, pacing, the audit log and the recording proxy hold for all three, and
  each one's own model API passes through the proxy untouched.
- **Steer and approve Codex and Gemini.** **Steer now** reaches Codex and Gemini agents mid-turn too
  (their only way in while they run). Approval rules hold for every agent: deny rules refuse in a
  pre-tool hook (Claude Code included, even in a mode that would not ask), allow rules answer
  Codex's PermissionRequest and go into a Gemini policy file.
- **Codex and Gemini output, read.** The built-in `codex` and `gemini` runners use their JSON output
  (`codex exec --json`, `gemini --output-format stream-json`): the console shows their text, each
  command or tool call, refusals, sessions, tokens and spend.
- **Spend for every agent and login.** Codex and Gemini CLI report tokens, and a subscription login
  reports no cost: indra estimates the spend from a price table (correct it with `[[price]]`). Spending
  limits now apply to every agent and are kept by indra itself, so they hold for OAuth logins too;
  engagement budgets and account limits count the same spend.
- **OAuth tokens for every agent.** Accounts take `kind = "oauth_token"` for Codex (an OpenAI agent
  identity token, logged in to a private Codex home) and Gemini CLI (a Google access token: Login
  with Google), as well as Claude Code (`claude setup-token`).
- **Fallbacks through accounts.** A model fallback can name an `[[account]]`: its API key, or its
  Claude subscription OAuth token, sent as a bearer token with Anthropic's OAuth beta flag.

## v0.18.0

Ideas from [ARTEX](https://github.com/BaaRai/ARTEX) (see `docs/artex-comparison.md`), written fresh for
indra.

- **Recording proxy for engagements.** With **Record traffic** on (the default), each agent's web
  requests go through its own proxy, which checks the scope on every connection (the name and every
  address it resolves to, connecting only to an address it checked), paces each host, and records
  each request and response by SHA-256. HTTPS is read with a certificate authority made for that one
  agent, whose key never touches the disk; the agent's own model API is tunnelled untouched.
- **Findings backed by traffic.** A finding cites the exchanges that prove it (`evidence_traffic`, with
  a role and a quote). `indra traffic list|show|verify|replay`, the engagement's **Traffic** view, and
  **Check evidence** on a run say whether each citation still holds.
- **Retest a finding.** **Retest** on a finding (or the new `sh-retest` skill) re-checks it against the
  code as it is now, or replays its recorded traffic inside an engagement, and records reproducible,
  fixed or cannot confirm in the run's `retests.jsonl`. `indra review retest` records one by hand.
- **Lessons with evidence.** Triage and retests turn into lessons for the codebase, each tied to the
  findings it came from: reversing a triage withdraws that evidence. The next review hands its hunters
  the lessons for their class (`indra lessons`). Knowledge > Lessons lists them; adopt or retire one.
- **Coverage.** An engagement shows which scope entries its agents reached and which not yet; a review
  run shows which entry points and classes its hunters probed.
- **Steer now.** A message the agent reads after its next tool call, mid-turn, without stopping it
  (Claude Code agents started from the console, here or on machines that dial in).
- **Approval rules by command.** `[approvals]` (and a profile's `approvals`) allow or deny Bash requests
  by their first words. An allow covers one plain command only; a deny is found anywhere, behind
  `sudo`, `sh -c` or in a pipeline. Each decision says who made it: a rule or a person.
- **Noise and budgets.** Pace presets (quiet, careful, balanced, aggressive); a host that answers 429
  or 503 is backed off by its `Retry-After`. An engagement's **budget** caps what all its agents spend:
  told to wrap up at 90%, stopped at 100%.
- **Model fallbacks.** A profile's `fallback` list takes Claude Code's model calls when Anthropic fails
  (429, 5xx, overloaded): a gateway with its own key, or a local model on Ollama. The agent's login
  never goes to a fallback.
- **Untrusted output, framed.** SIEM search results, workflow files read from repositories and recorded
  responses reach agents between nonce-tagged markers, as data, not instructions.

## v0.17.0

- **Own branch per agent.** **Work on its own branch** under Run an agent gives the agent a git
  worktree on a new branch, so agents working on one repository never edit the same files. When
  it stops, its session offers **Merge into `<base>`**, **Keep branch** (to push it or open a pull
  request) or **Discard**. A merge that conflicts is undone and leaves your branch as it was.
  Inspired by Claude Squad, Vibe Kanban and ClickHouse's multiagent-terminal.
- **Spending limit.** A Claude Code agent stops once its API calls cost a set amount, per launch
  (Run an agent > More options) or per profile (`max_budget_usd`), through Claude Code's own
  `--max-budget-usd`. Inspired by OpenHands' per-run budget.

## v0.16.0

- **Status** (`#/pad` in the console): every session in one list, the ones waiting for you
  first, then the ones at work, then what just finished. Each row has a small animated symbol
  that moves while the agent works (a spinner while it thinks, a cursor while it writes, its own
  motion for reading, editing, running a command, the web and subagents) and holds still while it
  waits for you (`!` for an approval, `?` for a question) or has finished (a tick or a cross). The
  state is always written beside it, so neither the symbol nor its colour is the only signal.
- A notice at the top says how many requests wait and opens them one after another, the
  longest-waiting first. Keys `1`-`9` open a row and Enter opens the next request.
- **Hold to allow.** Allowing a tool request now takes a 0.6 s press and hold (pointer, Enter or
  Space), so a stray tap or click never approves anything. Deny is still one tap, with an optional
  reason. This applies on Status, in Sessions and on a process page.
- **Answer an agent's questions from the console.** Claude Code's `AskUserQuestion` shows as
  numbered options (single or multiple choice, or your own text). Pick one, then hold Send. The API
  takes `answers` on `POST /api/procs/{id}/permission` and accepts only the questions that were
  asked.
- **Alerts**: a desktop notification (or a toast while you look at the console) and an optional
  chime when an agent asks for you or finishes. They are off by default and switched on from
  Status. They work on every page.
- **A cleaner console**: a header bar with the main sections (the rest under More) in place of
  the sidebar, content centred at a readable width, a quiet neutral palette where colour marks
  only what needs attention, and the state symbols in Sessions and on Home. It works at phone
  width.
- `GET /api/procs` reports each agent's `phase` and current `activity` (the tool it uses or asks
  for).

## v0.15.0

- **Attack surface: what actually impacts you.** A review run's page:
  - ranks its vulnerabilities by who can reach them: from the internet, after login, internal only, or
    not at all;
  - shows the path an attacker takes to each (Internet, then `POST /api/login`, then the finding);
  - draws the whole surface as a graph: entry points, components, findings, sinks, dependencies and
    their CVEs.
- **CVE reachability:** recon now records whether each CVE's vulnerable code is called, with the call
  sites, and the verifier records each finding's entry point and who can reach it. The new
  `sh-attack-surface` skill fills these in for older runs.

## v0.14.0

- **MCP & tools: any third-party tool for agents, managed from the dashboard.** Settings > MCP & tools
  (or `[[tool]]`) holds:
  - any MCP server, started from a command or reached at a URL: EDR, threat intel, ticketing,
    scanners, a vendor's own SIEM server;
  - read-only data sources: Wazuh, Elastic, OpenSearch, Splunk and log files.

  Give each tool to profiles, or pick tools per launch. Test checks a tool works. Secrets go into a
  private config file, never onto a command line, and the file is deleted when the agent exits.
- **Search data sources** with `indra siem search`.
- **Check detections:** on an engagement, see which of its steps your SIEM alerted on, which it
  missed, and the ATT&CK techniques its rules named. One click drafts detection rules for what was missed.
- **Blue-team skills:** `sh-detection-rules` (Sigma rules with noise measured on your logs and a test for
  each), `sh-log-triage` (incidents, timelines, indicators and verdicts) and `sh-hardening` (an ordered
  fix plan, each step with how to verify it).

## v0.13.0

- **Engagements: test live systems inside a written scope.** Each engagement records:
  - who authorized it;
  - what may be tested (hosts, `*.domain`, IPs and CIDRs, minus exclusions) and when;
  - the one machine its agents run on;
  - how many calls per second each host takes.
- **Every tool call is checked.** Agents in an engagement run Claude Code with `indra guard` as a hook.
  Each call that names a host outside the scope, or runs outside the window, is refused with the
  reason. The rest are paced per host and logged.
- **A tamper-evident audit log**: launches, approvals, every tool call and its result, chained by
  SHA-256, merged across the console and the machine, and exported as CSV or JSONL.

## v0.12.1

- **Ollama models that cannot load fail before the agent starts**, with what to do (update Ollama,
  or pull the model again), instead of as an API error inside the agent.
- **A run that ended in an error is not asked for learnings**, and an error in answer to the
  learnings question is not proposed as one.
- **Learnings are global.** Every harness has the global memory (shared by every project and agent),
  and accepted learnings go there unless you pick another vault.
- **Replies and README files render as Markdown** in Sessions and on run pages: headings,
  lists, tables, quotes, links and code. HTML in them is shown as text, never run, and
  images are never loaded.
- **Why the context is big, and a smaller one.** The Context panel says how much the agent started
  with before doing anything (Claude Code's instructions, the tools of each MCP server, skills,
  CLAUDE.md). **MCP servers: none** (Run an agent > More options, or `mcp = "none"` in a profile)
  starts it without MCP servers.

- **`indra node restart`** restarts a machine's link to the hub, so it runs the version `indra
  update` just installed. `indra update` says so on a joined machine. `indra node stop` stops it.
- **`indra node connect` runs in the background** (and again at each sign-in) and gives the
  terminal back; `--foreground` keeps it in the terminal as before.
- A machine on an older indra than the hub gets a note on its card with what to run, and requests
  it does not know yet say the same instead of "unknown request".

## v0.12.0

- **Threat models.** `threat model this repo` (or the threat-model workflow) maps the data flows, draws
  the trust boundaries with the controls found at each, and ranks STRIDE threats by risk, with a data
  flow diagram. It also works on a design document (`design:<file>`) or a change (`since:<ref>`). The
  run's page shows the boundaries and the open threats.
- **Reviews hunt where it matters.** A security review now threat-models first and hunts only the
  classes with an open threat, each hunter starting from a short brief; findings link to their threats.
- **Fewer tokens.** An unchanged codebase is not mapped twice, `since:<ref>` reviews only what changed,
  and Sessions shows each session's tokens, cache share and cost, with a hint when a long context makes
  every turn expensive.

## v0.11.0

- **Sessions, like Claude Code online.** Your agents as conversations: a list on the left (search, New,
  live status), the selected one in the middle with its tool steps folded to one line
  each, Allow and Deny for what it asks, and a box to reply (queued while it works) or to
  continue it once it ended.
- **What it worked on, at a glance:** the machine, folder and git branch, the agent and model,
  how full its context is, its task list, the files it changed (click one for the diff), and
  the pull requests it linked.
- **Browse the folder it works in.** Files on the right lists the folder the agent works in, on
  any machine; click a file to read it. Code (in replies, files and diffs) shows with line
  numbers and a Wrap lines switch. Read-only, never outside that folder, operators and admins.
- **Sessions are kept.** Finished sessions come back after the console restarts.

## v0.10.0

- **Workflows run on a schedule.** Click Start in the workflow editor: every hour, every day or
  weekday at a time, every week, or any cron schedule (`schedule = "0 9 * * 1-5"`).
- **If blocks.** An If checks something (a step failed, its output contains a text, an input
  is a value) and has a Yes and a No arrow; the side not taken is skipped.
- **Loops in the graph.** Drop For each or Repeat until on a step; it shows on the step, with
  an arrow back for Repeat. Click an arrow to run the next step on success, on failure or
  either way. The old Conditions and loops form is gone.
- Create with Claude knows schedules, If and branches; the run page shows what each If answered.

## v0.9.0

- **Roll out with Jamf, Intune or any MDM.** Machines > Add many machines makes one enrollment
  key for a whole fleet (expiry, machine limit, revoke), with ready-made scripts.
- **Organization settings.** `defaults.toml` (starting values) and `managed.toml` (enforced) in
  the system folder; Settings shows what your organization sets. `indra join` can take the hub
  and key from them.
- **A friendlier console.** Board edits and the Run an agent prompt survive refreshes, leaving
  unsaved changes asks first, admin-only buttons show to admins only, passwords are typed in
  masked dialogs, errors stay until closed, and Sign out works on phones.
- **Safer by default.** Only admins can start an agent that never asks for permission, and it
  shows a warning; outside links are web addresses only.
- **Fixes:** machines joining with one key at the same moment are never refused; memory notes
  on Windows and terminals on macOS no longer lose a race.

## v0.8.1

- **Choose where agents work** on any machine from the Machines page, with a folder browser.
- **Machines reconnect** by themselves after the hub or the machine was down.
- **Fixes:** dropdowns that showed nothing (Claude's "default" mode), workflow Output closing
  every few seconds, MCP servers that failed silently (the process page now says how to fix
  them). The default Ollama server can be saved.
- Releases stay hidden until every file is uploaded, so `indra update` never gets half of one.

## v0.8.0

- **Add a machine with one command.** Machines > Add a machine gives a line to paste on the
  other computer: it installs indra, joins, names itself, finds its agents and keeps running in
  the background. No SSH, no open ports.
- **A simpler console:** Home with a Get started checklist, a shorter menu (the rest under
  More), plain page names, and Run an agent with four fields.
- `indra node status`, `folders` and `leave` on a machine; Remove in the console.

## v0.7.0

- **Accounts.** Give agents credentials from the hub on any machine, track what each account
  used, set daily and monthly limits, and route runs to the account with the most left.

## v0.6.1

- Releases are named `v<version>`, published to crates.io (`cargo install indra`), and the
  README describes indra as it is now.

## v0.6.0

- **Workflows grew up:** shell command steps, conditions, loops (for each, repeat until),
  failure handling, stop steps, and Start and End blocks in the editor.
- **Create with Claude:** describe a workflow and Claude Code builds it, through indra's MCP
  server (`indra mcp`).
