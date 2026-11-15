# Changelog

What each release of the indra CLI adds, in short. Each release page on GitHub starts with its
section from this file.

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
