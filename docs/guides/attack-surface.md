# Attack surface: what an attacker can reach

A review run's page in the console (**Results > a run**) has an **Attack surface** card when the run
has a codebase map. It answers one question: of everything the review found, what can an attacker
actually reach?

## What it shows

- **The numbers**: how many vulnerabilities an attacker can reach from the internet, how many after
  login, and how many are internal only, not reachable or unknown. Also how many entry points the code
  has, and how many need no login.
- **What actually impacts you**: the reachable vulnerabilities, worst first. Each shows its severity and
  **how an attacker gets there**: Internet, then `POST /api/login`, then the finding. **Show everything**
  lists the rest too.
- **The graph**: Internet and Signed-in user, then entry points, then components (the code's folders),
  then findings, dangerous sinks, and the dependencies components use with their CVEs. Reachable
  findings and CVEs have their own colour. Click a node for its details.

## How reachability is decided

| Vulnerability | Reachable from the internet when | After login when |
|---|---|---|
| A finding | Its data flow starts at, or its `entry_point` names, an entry point without a login | The entry point needs a login |
| A CVE | It is `reachable` (its vulnerable code is called) in a component that an entry point without a login leads into | The same, behind a login |

**Explicit verdicts win over the graph:**
- **Findings:** the verifier records each finding's `entry_point` and its `reachability`
  (`unauthenticated`, `authenticated`, `internal`, `unreachable` or `unknown`), based on the guards on
  the path.
- **CVEs:** recon records `reachable`, `not_reachable` or `unknown`, with the call sites of the
  vulnerable code as `evidence`.

A CVE in code that nothing calls is **not reachable**: it can wait, and the card says so.

## Fill it in for an older run

Runs made before these fields existed can be completed by asking:

```
map the attack surface of this repo and tell me which vulnerabilities actually matter
```

The `sh-attack-surface` skill then:
1. checks which entry points really need a login;
2. decides whether each CVE's vulnerable code is called;
3. records who can reach each finding;
4. writes `attack-surface.md` next to the run.

Reload the run's page to see the result.

## API

`GET /api/run/surface?key=<run>` returns the graph (`nodes`, `edges`), the vulnerabilities with their
`reach` and `path`, and the summary.
