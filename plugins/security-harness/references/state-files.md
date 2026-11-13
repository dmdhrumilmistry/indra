# Shared state files - the coordination bus

Subagents run in isolated contexts and cannot see each other's memory. All cross-agent coordination
happens through files in a single per-run working directory. This document is the contract; every agent
reads and writes these exact paths and shapes.

## Working directory

```
<TARGET>/.security-harness/<run-id>/
```

- `<TARGET>` = the codebase under review (default: current repo root).
- `<run-id>` = `run-YYYYMMDD-HHMMSS` chosen by the orchestrator at Stage 0 and passed to every agent.
- The orchestrator also maintains a `latest` pointer: `<TARGET>/.security-harness/latest` (a file whose
  contents is the current run-id) so ad-hoc/router invocations can find the active run.
- Add `.security-harness/` to the target's `.gitignore` if writing inside a git repo (recon does this).
- `<TARGET>/.security-harness/memory/` is the target's **long-term memory vault** (`indra mem` notes). Unlike a
  run directory it persists across runs: Stage 6 writes it, the next run's recall reads it. It is covered
  by the same `.gitignore` entry.

## Files

| File | Writer(s) | Reader(s) | Shape |
|---|---|---|---|
| `capabilities.json` | orchestrator (Stage 0) | all | `{ "graft": bool, "graft_version": "0.12.0", "graft_installed_this_run": bool, "graft_wired": bool, "graft_deep": bool, "syft": bool, "grype": bool, "trivy": bool, "osv_scanner": bool, "pandoc": bool, "wkhtmltopdf": bool, "chrome": bool, "indra": bool, "notes": "..." }` - `graft_deep` true only when LLM creds (GRAFT_API_KEY/PROVIDER/MODEL) are set; `graft_wired` true when `graft init` registered the MCP server for the target. |
| `scope.json` | orchestrator (Stage 0) | all | `{ "target": "abs/path", "include": ["src/**"], "exclude": ["**/test/**","**/vendor/**"], "classes": ["sqli","access-control", ...], "run_id": "...", "mode": "full|single-class|single-stage", "models": { "setup":"inherit", "recon":"sonnet", "hunt": {"pattern":"haiku","trace":"sonnet","logic":"opus"} \| "<model>", "chain":"opus", "verify":"opus", "report":"haiku" } }` - `models` is the resolved per-stage model map (see the orchestrator's Model-selection section); `hunt` is either a single model string (flattened) or the tiered object. |
| `recon.md` | sh-recon | hunters, chainer, verifier, reporter | Human-readable recon report (stack, versions, SBOM summary, CVEs, attack surface, entry points, trust boundaries, Graft usage notes). |
| `codebase-map.json` | sh-recon | hunters, chainer | Machine map: see schema below. |
| `attempts.md` | every hunter (append) | every hunter (read first) | Append-only ledger. See format below. |
| `recall-<class>.md` | orchestrator (after recon, when `indra` is available) | that class's hunter, verifier | Markdown output of `indra mem recall` from the target and global memory vaults. See the `sh-memory` skill. |
| `findings.jsonl` | hunters (append) | chainer, verifier | One `finding-schema.json` object per line, `status: candidate`. |
| `chains.md` | sh-chainer | verifier, reporter | Escalation chains referencing finding ids. See format below. |
| `verified.jsonl` | sh-verifier | reporter | Final findings (candidate objects re-emitted with updated `status`, `verification`, `cvss`, `poc`, `confidence`). |
| `reports/` | sh-reporter | user | `README.md`, `findings.json`, `results.sarif`, `report.html`, `report.pdf` (+ `report.docx` when pandoc present). |

### `codebase-map.json` schema (recon output)

```json
{
  "target": "abs/path",
  "generated_by": "graft-deep | graft | native-fallback",
  "stack": { "languages": ["python","javascript"], "frameworks": ["flask","react"], "runtimes": {"python":"3.11"} },
  "dependencies": [ { "name": "flask", "version": "2.0.1", "ecosystem": "pypi", "direct": true } ],
  "sbom": { "format": "cyclonedx | spdx | partial-manifest", "path": "sbom.json | null", "component_count": 0 },
  "known_cves": [ { "package": "flask", "version": "2.0.1", "id": "CVE-XXXX-YYYY", "severity": "high", "fixed_in": "2.2.5", "source": "grype|osv|kb" } ],
  "entry_points": [ { "kind": "http-route|cli|cron|queue|graphql", "method": "POST", "path": "/api/login", "handler": "auth.login", "file": "app/auth.py", "line": 20, "auth_required": false } ],
  "trust_boundaries": [ { "description": "unauthenticated public API surface", "entry_points": ["/api/login","/api/reset"] } ],
  "dangerous_sinks": [ { "class": "sqli", "symbol": "cursor.execute", "file": "app/db.py", "line": 88 } ],
  "graft": { "graph_path": "graft/.graph/wiring.json", "built": true, "deep": false }
}
```

### `attempts.md` format (dedup ledger - READ THIS BEFORE HUNTING)

Append-only. Each hunter appends a block when it finishes probing an area so no other hunter (or a later
run) repeats the same query. Read the whole file first; skip any `(class, area, technique)` already logged.

```
## [<class>] <area-or-file> - <ISO-8601-ish timestamp from `date` if available, else run-id + seq>
- technique: <what was tried, e.g. "graft grep 'cursor.execute\(.*%'">
- scope: <files/symbols covered>
- result: <found: SH-SQLI-003 | clean | inconclusive: needs X>
- notes: <bypasses ruled out, follow-ups for other classes>
```

### `chains.md` format (escalation)

```
## CHAIN-001: <name, e.g. "Open redirect -> session theft -> account takeover"> [severity: critical]
- steps:
  1. SH-OPENREDIR-002 - attacker crafts redirect to attacker host
  2. SH-AUTH-004 - session token leaked via Referer to that host
  3. -> full account takeover
- preconditions: <e.g. victim clicks link; app uses Referer-leaking token>
- combined_impact: <why the chain is worse than any single finding>
- member_findings: [SH-OPENREDIR-002, SH-AUTH-004]
```

## Concurrency rules

- Hunters run in parallel and all append to `findings.jsonl` and `attempts.md`. Appends must be a single
  atomic write of complete lines/blocks (build the text, then one `>>` append) - never interleave partial writes.
- Each hunter owns a **disjoint finding-id namespace** by class prefix (`SH-<CLASS>-NNN`), so ids never collide.
- Only the verifier writes `verified.jsonl`; only the reporter writes `reports/`. Single-writer for those two.
