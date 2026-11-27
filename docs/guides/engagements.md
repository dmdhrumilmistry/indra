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
| Noise, Calls per second per host | The pace: quiet (1), careful (2, the default), balanced (5), aggressive (20), or a number of your own from 0.1 to 100 |
| Budget (US$) | Optional: what all its agents may spend on API calls together (see [Budget](#budget)) |
| Record traffic | On by default: its agents' web requests go through the [recording proxy](#the-recording-proxy) |
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

When a host answers **429** (too many requests) or **503** (overloaded), the guard sees it in the tool's
output and backs that host off: its next calls wait for the `Retry-After` it gave, or 2 seconds, then 4,
then 8 (at most 5 minutes) while it keeps answering that way. A wait longer than 30 seconds is refused
with the reason, so the agent slows down instead of hanging. The audit log shows each `backoff`.

The guard reads the hosts a tool call **names**:

- URLs, including those inside commands;
- IP addresses and CIDR ranges;
- host names with a known top-level domain.

It skips file names (`main.rs`, `notes.md`), paths and version numbers. When it cannot tell, it errs
toward refusing, and its reason tells the agent which host to leave out.

It does **not** see what a program resolves or connects to by itself: a host read from a file, a
redirect, a scanner following links. That is what the recording proxy is for.

## The recording proxy

With **Record traffic** on, each agent gets its own proxy on its machine, on loopback, for as long as it
runs. Its tools are pointed at it (`HTTPS_PROXY`, `HTTP_PROXY`, and the CA variables curl, Python,
Node and git read), so the requests the programs actually make go through it, redirects included.

For each request the proxy:

- **checks the scope at connection time**: it resolves the host, checks the name and every address it
  resolved to against the scope, the exclusions and the window, then connects to an address it checked,
  so DNS cannot change the answer in between. A refusal is a `403` with the reason;
- **paces** it per host (the engagement's rate) and **backs off** a host that answers 429 or 503,
  answering `429` with a `Retry-After` itself when a host is booked too far ahead;
- **records** the request and the response: headers, and up to 256 KiB of each body, stored by SHA-256
  in `engagements/<id>/traffic/` in indra's data folder (readable by its owner only), and a `traffic`
  entry in the audit log. It asks the server for an uncompressed answer, so a finding can quote it.

HTTPS is read with a certificate authority made for that one agent when it starts. Its key never
touches the disk, and its certificate is removed when the agent ends. The agent's own model API
(Anthropic, the profile's base URL) and the MCP servers the console gave it are tunnelled untouched:
never decrypted, never recorded, so their credentials are never seen.

Still not seen: a program that ignores the proxy settings (raw sockets, `nmap`, a tool told to skip the
proxy). For hard enforcement, also firewall the engagement's machine so it can reach only the scope.

### Traffic as evidence

**Traffic** on an engagement lists what was recorded, newest first, searchable by host and by text in
the URL or a body; open one to see its headers (cookies and tokens hidden) and bodies. An agent in the
engagement reads them with `indra traffic`:

```bash
indra traffic list --host app.example.com --grep '"role":"admin"'
indra traffic show 1764200000000-7          # secret headers masked; --full shows them
indra traffic replay 1764200000000-7        # the same request again: scope, pace and recording apply
indra traffic verify verified.jsonl         # check every citation in a findings file
```

A finding cites the exchanges that prove it in `evidence_traffic`, each with a role (`baseline`,
`proof`, `verification`, `supporting`), and optionally a `quote` and the response's `sha256`:

```json
"evidence_traffic": [
  { "engagement": "abcd1234", "exchange": "1764200000000-3", "role": "baseline" },
  { "engagement": "abcd1234", "exchange": "1764200000000-7", "role": "proof", "quote": "\"role\":\"admin\"" }
]
```

A finding is **evidence-verified** only when every citation holds (the exchange exists, its kept body
is unchanged, the quote is in it verbatim) and one `proof` carries a quote. **Check evidence** on a
run's findings does this on the machines that recorded the traffic. IDs come from `indra traffic list`;
an agent never guesses one.

## Coverage

**Coverage** on an engagement compares its scope with what its agents reached, from the audit log:
for each scope entry, the hosts under it that an allowed tool call named or the proxy passed, with
how many calls and when the last was, and the entries nothing reached yet. Refused hosts are listed
apart.

## Budget

An engagement's **budget** caps what all its agents spend together, as Claude Code reports it at the
end of each turn:

- each agent starts with what is left as its own spending limit (`--max-budget-usd`);
- at 90%, every agent still running is told, through a [steer](pivoting.md#steer-without-stopping), to
  record what it found and finish;
- at 100%, they are stopped, and no new agent starts in it.

Each step is written to the audit log. A subscription login is not metered this way.

## Requirements

- **Claude Code** on the engagement's machine.
- **indra 0.13.0 or newer** on that machine (**0.18.0** with Record traffic). The console refuses to
  start an engagement agent on an older one, which would ignore the engagement.

## API

| Call | What it does |
|---|---|
| `GET /api/engagements` | Every engagement, with its state (scheduled, active, ended) |
| `POST /api/engagements` (admins) | Create one: `{name, machine, scope, exclude, starts, ends, rate, authorization, budget_usd, record}` (times in Unix seconds) |
| `POST /api/engagements/{id}/end` (admins) | End it now and stop its agents |
| `GET /api/engagements/{id}/audit[?format=csv\|jsonl]` (operators) | The merged audit log and whether its chain is intact |
| `GET /api/engagements/{id}/coverage` | Scope entries reached and not, refused hosts, spend against the budget |
| `GET /api/engagements/{id}/traffic[?host=&q=&limit=]` | Recorded exchanges, newest first |
| `GET /api/engagements/{id}/traffic/{exchange}` | One exchange, secret headers masked, the start of each body |
| `POST /api/procs` with `"engagement": "<id>"` | Start an agent in it |
