# Threat modelling method

`sh-threat-modeler` follows this method, and `sh-threat-model` and `sh-security-review` read its
output. A threat model answers four questions, in this order:

1. **What are we working on?** Components, data stores, external parties and data flows.
2. **What can go wrong?** Threats where data crosses a trust boundary (STRIDE).
3. **What are we doing about it?** The controls that exist in the code today, with `file:line`.
4. **Did we do a good job?** The threats that remain open, ranked, with what to check or fix.

Ground every element in the code or in a design document the user gave. A threat model that names
components the code does not have is worse than none: it sends the hunters to the wrong places.

## 1. Model the system

From `recon.md` and `codebase-map.json` (or from the design document in design mode):

- **Actors**: who uses or reaches the system (anonymous visitor, signed-in user, admin, another
  tenant, an internal service, a CI job, a webhook sender, the operator at the shell).
- **Components** (`kind: process`): each service, worker, CLI or function that runs code.
- **Data stores** (`kind: store`): databases, caches, queues, buckets, files, secrets stores.
- **External entities** (`kind: external`): third-party APIs, identity providers, payment providers,
  LLM APIs, the browser, mobile apps.
- **Assets**: what an attacker wants: credentials and sessions, personal data, payment data, tenant
  data, secrets and keys, money-moving actions, admin actions, availability. Rate each `high`,
  `medium` or `low`.
- **Data flows**: `from -> to`, what it carries (which assets), the protocol, and whether it is
  authenticated and encrypted.

## 2. Draw the trust boundaries

A **trust boundary** is a line where the level of trust changes: on one side something the system
controls, on the other something it does not, or something with less privilege. Typical ones:

| Boundary | Example |
|---|---|
| internet -> edge | public HTTP routes, webhooks, OAuth callbacks |
| anonymous -> authenticated | the login, sign-up and password-reset routes |
| user -> admin | admin routes, role checks, feature flags that gate power |
| tenant A -> tenant B | every lookup by a client-supplied id in a multi-tenant app |
| app -> data store | the query layer (SQL, NoSQL, search, cache) |
| app -> internal network / cloud metadata | outbound HTTP, URL fetchers, image proxies |
| app -> OS | shell commands, file paths, archive extraction, deserialization |
| app -> third party | payment, email, LLM and identity providers, and what they send back |
| build -> runtime | CI secrets, dependency installs, generated code |
| LLM output -> action | an agent or prompt whose output is executed, rendered or used in a query |

Each boundary lists the data flows that cross it and the **controls** found at the crossing
(authentication, authorization, validation, encoding, rate limits, signatures, TLS), each with
`file:line`. A crossing with no control is where the hunters start.

## 3. Find threats with STRIDE

Walk every data flow that crosses a boundary and ask each STRIDE question of it:

| Letter | Threat | Breaks | Ask | Classes to hunt |
|---|---|---|---|---|
| S | Spoofing | authenticity | Can someone pretend to be another user, service or webhook sender? | auth, crypto, secrets |
| T | Tampering | integrity | Can input change a query, a command, a file, a price, another user's data? | sqli, injection, xss, path-traversal, deserialization, xxe, file-upload, csrf, race-conditions |
| R | Repudiation | accountability | Can someone act without a trace (no audit log for admin or money actions)? | (design: logging) |
| I | Information disclosure | confidentiality | Can data leak across the boundary (IDOR, verbose errors, SSRF to metadata, secrets in code)? | access-control, ssrf, path-traversal, secrets, xxe |
| D | Denial of service | availability | Can one request exhaust CPU, memory, disk or money (regex, unbounded loops, LLM spend)? | (design: limits) |
| E | Elevation of privilege | authorization | Can a user reach admin actions, another tenant, or code execution? | access-control, injection, deserialization, file-upload |

For LLM features, also ask: prompt injection through any text the model reads (documents, web pages,
tool output), excessive agency (what the model's tools can do), and output handling (is the model's
output rendered, executed or used in a query without the same care as user input?).

Each threat records: the STRIDE letter, the boundary and flow it sits on, the asset at risk, a
one-line attack scenario, the controls already in place, `likelihood` and `impact` (`high`,
`medium`, `low`), the resulting `risk` (the table below), its `status`, and the vulnerability
classes and code locations a hunter should check.

| likelihood \ impact | high | medium | low |
|---|---|---|---|
| **high** | critical | high | medium |
| **medium** | high | medium | low |
| **low** | medium | low | low |

`status` is one of:
- `open` - nothing in the code stops it, or the control is incomplete: hunt it.
- `mitigated` - a control stops it; name the control with `file:line`.
- `accepted` - out of scope or a deliberate trade-off the user stated.
- `design` - not a code bug (missing audit log, no rate limit): goes to the report as a design gap.

Do not inflate. A threat with a solid control is `mitigated`, and saying so is as useful as finding a
gap: it tells the hunters where not to spend time.

## 4. Outputs

### `threat-model.json`

```json
{
  "target": "abs/path",
  "mode": "code | design | diff",
  "summary": "One paragraph: what the system is and the biggest risks.",
  "actors": [ { "id": "A1", "name": "anonymous visitor", "trust": "untrusted" } ],
  "assets": [ { "id": "AS1", "name": "user sessions", "sensitivity": "high" } ],
  "elements": [
    { "id": "P1", "kind": "process", "name": "web API", "file": "app/main.py" },
    { "id": "D1", "kind": "store", "name": "Postgres" },
    { "id": "E1", "kind": "external", "name": "Stripe" }
  ],
  "flows": [
    { "id": "F1", "from": "A1", "to": "P1", "data": "login form", "assets": ["AS1"],
      "protocol": "HTTPS", "authenticated": false, "boundary": "TB1" }
  ],
  "boundaries": [
    { "id": "TB1", "name": "internet -> web API", "from": "untrusted", "to": "app",
      "flows": ["F1"],
      "controls": [ { "name": "session cookie, HttpOnly", "file": "app/auth.py", "line": 40 } ],
      "gaps": ["no rate limit on /login"] }
  ],
  "threats": [
    { "id": "T1", "stride": "S", "title": "Password guessing on /login",
      "boundary": "TB1", "flow": "F1", "asset": "AS1",
      "scenario": "An attacker tries common passwords against many accounts.",
      "controls": [], "likelihood": "high", "impact": "high", "risk": "critical",
      "status": "open", "classes": ["auth"],
      "locations": [ { "file": "app/auth.py", "line": 20 } ],
      "check": "Is there a lockout or rate limit on the login route?" }
  ],
  "hunt_plan": [ { "class": "auth", "threats": ["T1"], "focus": ["app/auth.py"] } ]
}
```

Keep it strictly valid JSON: the console renders it and the orchestrator parses it.

### `threat-model.md`

The readable version, in this order:

1. **Summary**: the system in two sentences and the top three risks.
2. **Data flow diagram**: a Mermaid `flowchart LR` with one `subgraph` per trust zone, so each
   subgraph edge is a trust boundary. Label each edge with its flow id and data. Keep it to the
   flows that cross a boundary plus the ones that carry high-sensitivity assets.
3. **Trust boundaries**: a table of boundary | flows | controls (`file:line`) | gaps.
4. **Threats**: a table of id | STRIDE | title | boundary | risk | status, ranked by risk, then
   one short paragraph for each `open` threat (scenario, why the controls fall short, what to check).
5. **Design gaps**: the `design` threats, as recommendations.
6. **Hunt plan**: which classes to hunt and where, from `hunt_plan`.

### `focus-<class>.md` (one per class in the hunt plan)

A short brief for that class's hunter, so it does not have to read the whole recon and threat model:
the open threats for the class (id, scenario, check), the boundaries they sit on with their controls,
the entry points and sinks from `codebase-map.json` that matter for it (`file:line`), and the
framework defaults that affect it. Aim for under 150 lines. A hunter reads this first and opens
`recon.md` only for what the brief leaves out.
