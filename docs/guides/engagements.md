# Engagements

An **engagement** is a test of live systems that stays inside a written scope. It records:

- **who authorized it**, and the reference (a signed statement of work, a ticket, an email);
- **what may be tested**: hosts, their subdomains, IPs and CIDR ranges, minus exclusions;
- **when**: a time window, up to 90 days;
- **where its agents run**: one machine you choose;
- **how fast**: calls per second per host.

Every agent started in an engagement is Claude Code with `indra guard` as its hook. Each tool call it
makes is checked against the scope and the window, paced per host, and written to an audit log
before it runs.

## Create one

**Engagements > + New engagement** (admins only):

| Field | What to enter |
|---|---|
| Name | A short name, like `Q4 web app test` |
| Machine | The machine its agents run on. They run there only |
| In scope | One entry per line: a host (`app.example.com`), its subdomains (`*.example.com`, which does not include `example.com` itself), an IP (`10.20.0.5`) or a CIDR range (`10.20.0.0/24`). A URL counts as its host |
| Excluded | Never tested, even inside the scope (`payments.example.com`, `10.20.0.1`) |
| Starts, Ends | The window. Outside it, nothing that names a host runs |
| Calls per second per host | The pace, 0.1 to 100 (default 2) |
| Authorization | Required: who authorized the test, and the reference |

## Run agents in it

On an active engagement, **Run an agent in it** asks for the task, the folder on its machine and a
profile, then opens the session.

Each agent:

- **runs on the engagement's machine**, as Claude Code. Its hooks are what enforce the scope, so other
  runners are refused;
- **runs `indra guard pre` before every tool call.** The call is refused, with the reason shown to the
  agent, when:
  - it names a host outside the scope or in an exclusion;
  - it runs outside the window;
  - its host is booked more than 30 seconds ahead by the pace.

  Otherwise the call is paced and allowed, and your usual Allow and Deny prompts still apply;
- **runs `indra guard post` after every call** to record how it went;
- **stays in the engagement when it is continued**, even with another profile. Its continuation uses
  the engagement's current terms.

The engagement comes only from the console's own records: a launch request cannot bring its own.
**End now** ends the window and stops the agents running in it.

## The audit log

**Audit log** on an engagement shows everything, newest first:

- the console's entries: created, launches (by whom, with the prompt), your approvals, the end;
- the guard's entries from the machine: each tool call with the hosts it named, the decision and why,
  how long it was paced, its input, then how it went.

Each entry holds the SHA-256 of the one before it, and the log shows **chain intact** only when every
entry is unchanged and none is missing. **Export CSV** or **Export JSONL** downloads it. The CSV
neutralises cells that a spreadsheet would run as a formula.

On the machine, the log is `engagements/<id>/audit.jsonl` in indra's data folder, readable by its owner
only.

## What the guard does and does not see

The guard reads the hosts a tool call **names**:

- URLs, including those inside commands;
- IP addresses and CIDR ranges;
- host names with a known top-level domain.

It skips file names (`main.rs`, `notes.md`), paths and version numbers. When it cannot tell, it errs
toward refusing, and its reason tells the agent which host to leave out.

It does **not** see what a program resolves or connects to by itself: a host read from a file, a
redirect, a scanner following links. It is a policy layer for the agent, not a network sandbox.

For hard enforcement, also firewall the engagement's machine so it can reach only the scope. The
recording proxy that is planned next will add enforcement at the network layer.

## Requirements

- **Claude Code** on the engagement's machine.
- **indra 0.13.0 or newer** on that machine. The console refuses to start an engagement agent on an
  older one, which would ignore the engagement.

## API

| Call | What it does |
|---|---|
| `GET /api/engagements` | Every engagement, with its state (scheduled, active, ended) |
| `POST /api/engagements` (admins) | Create one: `{name, machine, scope, exclude, starts, ends, rate, authorization}` (times in Unix seconds) |
| `POST /api/engagements/{id}/end` (admins) | End it now and stop its agents |
| `GET /api/engagements/{id}/audit[?format=csv\|jsonl]` (operators) | The merged audit log and whether its chain is intact |
| `POST /api/procs` with `"engagement": "<id>"` | Start an agent in it |
