# Changelog

What each release of the indra CLI adds, in short. Each release page on GitHub starts with its
section from this file.

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
