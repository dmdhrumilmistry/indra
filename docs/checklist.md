# Checklist

What is being finished, what is done, and what is still open. New items go to the end of
their section; nothing is deleted, items are checked off with the change that did them.

## Claude Code on Ollama

- [x] `provider = "ollama"` for a whole agent (Claude Code, Codex) and `subagent_provider` for
      Claude's subagents only (#32)
- [x] The model router: routes by model, streams, keeps every credential off Ollama (#32)
- [x] Workflow tasks override `model` and `provider`; Launch has **Runs on** (#32)
- [x] One shared setup for Claude Code on Ollama (console launches, terminal, CLI)
- [x] `indra claude -m MODEL`: Claude Code in your own terminal on an Ollama model, with checks,
      `--pull`, `--subagents`, `--profile`, `--dry-run`
- [x] Preflight checks: server up, model pulled, tool calling, context window, Ollama version
- [x] Tell Claude Code the real context window (`CLAUDE_CODE_MAX_CONTEXT_TOKENS`); it assumes
      1M for models it does not know and would never compact
- [x] Browser terminal: Claude Code with a profile (an Ollama one included), and resuming an
      Ollama agent's session on Ollama, not Anthropic
- [x] Console: Ollama card on Tools (server, models, tool support, context, pull with progress)
- [x] Launch warns before starting an agent on a missing or unsuitable Ollama model
- [x] Docs: `indra claude`, the context window, the checks
- [x] Browser terminal: the profile was dropped by term.js before it reached the server (found
      by running the real Claude Code TUI through the terminal; fixed)
- [ ] Test against a real Ollama server (needs permission to run the Ollama binary in the
      test container; done so far against a stand-in that speaks Ollama's API)

## Review loop

- [x] Start a PR review from the console, triage findings, compare runs (#33)
- [x] Claude Code on Ollama end to end: `indra claude`, terminal profiles, checks, pulls (#34)

## Partly implemented (from the audit)

- [x] Settings > General save drops `[server] webrtc` and `ice_servers`; no UI for them
- [x] Runner form drops `model_args` (Customize or Edit on codex or gemini breaks `model`)
- [x] Workflow "Edit as form" drops `component`, `workflow` and `pos` on tasks
- [x] Settings forms in general: keep keys a form does not show instead of dropping them
- [x] Nodes that dial in (no `ssh`): the form requires ssh, the summary prints "undefined",
      `/api/nodes/runs` and `indra nodes` only work over SSH (runs now come over the link)
- [x] TURN servers cannot carry a username and credential
- [x] Linear is named in the architecture docs but there is no Linear connector
- [x] A fresh pivot cannot switch runner, profile or model (the guide says it can)
- [x] Tracker import stops at 50 items (no paging)
- [x] Leftover "bundled script" wording and a stale `scripts/` folder after the Python removal
- [x] Dead `/api/memory` endpoint
- [x] Board: the Run form cannot pick a runner
- [x] Runs on remote nodes are listed but cannot be opened, triaged or posted: the Nodes page
      now copies a run to the hub (over SSH or the link) and opens the copy
- [x] tools/indra/README.md Settings table lists 7 of the 13 tabs

## Install and release

- [x] One-line installers: `install.sh` (Linux, macOS) and `install.ps1` (Windows), checking SHA-256
- [x] `indra update` (and `--check`): the binary updates itself, checking SHA-256
- [x] Release workflow: tag must match the crate version; `SHA256SUMS` and the install scripts on
      each release; install instructions in the release notes
- [x] Docs lead with the one-liners; the console's Add a machine shows install, then connect
- [x] Cut the first binary release (`indra-v0.2.0`) and try the one-liners against it (the
      install.sh one-liner installed 0.2.0, a 14.5 MB static Linux build; `update --check` agreed)
- [x] Release from Actions (Run workflow with a version), for when tags cannot be pushed (#37)

## Requested next

- [x] Terminal: prompt icons render (Nerd Font symbols bundled, UTF-8 locale) (#38)
- [x] Workflows take inputs; **pr-review** and **security-review** are built-in workflows;
      Runs has **Run a workflow** instead of a PR-only form; `indra run NAME --input k=v`
- [x] Users and roles (RBAC) for the console instead of an access token in the URL: sign-in,
      viewer/operator/admin checked on every call, Settings > Users, `indra user`, audit log
- [x] Manage terminals: tabs in one terminals page (+ New, a close button per tab, Start again,
      keyboard shortcuts), console buttons add tabs to an open page, and **Running terminals** on
      the Terminal page with Close (ends background jobs too)
- [x] Cut `indra-v0.3.0` so `indra update` brings the icon fix, workflow inputs, users and
      terminal tabs to installs of 0.2.0
- [x] Automatic updates: a daily background check tells you about new releases, and
      `indra update --auto on` installs them by itself (SHA-256 checked; the console shows it too)
- [x] Tab completion: `indra completions` for bash, zsh, fish and PowerShell, set up by the
      installers, refreshed by `indra update`
- [x] Workflow editor: delete a step (button at the top of the panel, x on the step, Delete key,
      Undo); steps added from the palette no longer land on top of each other
- [x] Workflows as code: `<repo>/.indra/workflows/*.toml`, picked up from every harness root and
      target, saved there from the editor (Saved in), seen after a `git pull`, bad files listed
- [x] Built-in profiles (planner, worker, builder, reviewer) and team (plan-and-build), so the
      Team form and shared workflows work with no configuration (the empty Planner/Worker bug)
- [x] Skills as workflow steps (`skill = "<name>"`, Skills in the palette); "components" are now
      called saved tasks in the console
- [x] Terminal shell is configurable (`[terminal]`, Settings > General) and pickable per terminal
- [x] Several boards: create, rename, delete (move or drop items), trackers per board, move items
- [x] Release: each indra release is marked Latest once complete; `latest` input marks one (#44)
- [x] Shell steps (`run`), on any node, with values passed as environment variables only
- [x] Conditions (`when`), `on = "failure" | "always"`, `allow_failure`, `{steps.X.output}`
- [x] Loops: `foreach` (copies per item) and `until` with `max_runs`
- [x] `indra mcp` (MCP server: guide, list, get, validate, save) and **Create with Claude**
- [x] Stop steps (`stop = "success" | "failure"`) and Start/End blocks in the editor
- [x] A local step without a target runs in the harness's first target (saving a workflow with
      steps dragged from the palette failed with "has no target")
- [x] Releases are `v<version>` (no `indra-` prefix); installers and `indra update` read both
- [x] Each release is published to crates.io (`cargo install indra`) with `CARGO_REGISTRY_TOKEN`
- [x] Accounts: credentials from the hub for agents on any node, usage per account and per
      machine login, limits, cooldown after usage limits, routing (most left by default), the
      Accounts page and `indra accounts`
- [x] Adding a machine is one command: **Machines > Add a machine** gives a code (once, an hour)
      and a line to paste there (`INDRA_JOIN` in the installers, or `indra join <hub> <code>`);
      the machine names itself, reports its system and agents, and keeps its link running in the
      background (systemd, launchd, Windows sign-in); `indra node status`, `folders`, `leave`;
      **Remove** in the console
- [x] Joining works on a console that listens on the network (its settings are locked there:
      "Add a machine" used to fail without a visible error)
- [x] Folders agents work in are changed from the console for every machine (this one too):
      a folder browser over the machine's link (names only), checked and kept by the machine
- [x] Machines reconnect after the hub or the machine was down; a machine the hub does not know
      keeps retrying (the hub may run with another config), and stops only when removed; a
      supervisor restarts a background link that failed (Windows, Linux without systemd)
- [x] Blank selects everywhere: a select set to a value it has no option for (Claude's `default`
      permission mode) now shows that value
- [x] Workflow step Output stays open while the page refreshes
- [x] The process page lists the agent's MCP servers and says how to fix one that failed or
      needs a login
- [x] The default Ollama server can be saved (Tools > Local models > Save as default)
- [x] MDM roll-outs (Jamf, Intune, any script): reusable enrollment keys (expiry, machine limit,
      revoke; stored hashed) with ready-made scripts; organization settings in `defaults.toml`
      (under the user's) and `managed.toml` (enforced, shown in Settings); `[join]` lets
      `indra join` run with no arguments; joining again is a no-op
- [x] Console review fixes: Board forms survive the refresh and cards move without dragging
      (Move to); the Run an agent prompt is kept as a draft; leaving a page with unsaved changes
      asks first; admin-only buttons show to admins only; passwords in masked dialogs; errors stay
      until closed; Sign out and Theme on phones; waiting approvals in the tab title; confirms for
      Stop, Cancel and Discard; a focus ring; a warning (and admins only) for bypassPermissions;
      outside links http(s) only; plain, consistent names; Run again for workflows
- [x] Workflows, simpler: schedules (cron, local time; the Start block and `schedule`), If steps
      with Yes and No edges (`if` and `branch`; a step both sides reach runs after the side that
      ran), For each and Repeat until as bands on a step with a loop-back arrow, link conditions
      on the arrow; conditions picked from lists; the old Conditions and loops form is gone
- [x] A simpler console: Home with a Get started checklist, a shorter menu with the rest under
      More, plain page names (Run an agent, Running, Results, Machines), and Run an agent with
      four fields and the rest under More options

## Not planned yet (the docs say so)

- [ ] OS keychain backend for secrets
- [ ] Several hubs sharing one fleet of nodes
- [ ] A DAST bridge for `needs-runtime` findings
