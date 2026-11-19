---
name: indra
description: "indra - the entry point for application-security work. Dispatcher for the indra security harness. Use when the user asks for any application-security work - 'security review', 'audit this codebase', 'pentest', 'find vulnerabilities', 'check for SQLi/XSS/IDOR/SSRF', 'generate a security report/SARIF', 'threat model', 'detection rules', 'triage these alerts', 'hardening plan'. Interprets the request and routes it to the full sh-security-review pipeline, a single stage, or a single vulnerability-class knowledge base."
argument-hint: "[what to do - e.g. 'full security review of ./api' or 'find SQLi and IDOR in src/']"
---

# indra - Router

You are the front door. Read the user's request, decide the smallest flow that satisfies it, and hand off.
Prefer routing over doing the work inline - the specialized skills/agents carry the knowledge and contracts.

## Routing table

| The user wants... | Route to |
|---|---|
| To review a **pull request** (a GitHub PR link, "review PR 42", "check this PR", "review these changes before merge") | Invoke the **`sh-pr-review`** skill. Pass the PR **link** verbatim if the user gave one, else the number; with neither it uses the PR for the current branch. |
| A full security review / audit / pentest of a codebase | Invoke the **`sh-security-review`** skill (all stages). Pass the target path and any class filters. |
| To find specific class(es) only ("just SQLi and IDOR") | Invoke **`sh-security-review`** with `classes:<slugs>`; it runs recon + those hunters + verify + report, skipping chaining if a single class. |
| To re-run one stage on an existing run ("re-verify", "regenerate the report", "re-scan deps") | Invoke **`sh-security-review`** with `stage:<recon\|threat\|hunt\|chain\|verify\|report>`. |
| To review again what changed since an earlier review ("what changed since v1.2", "re-review the new code") | Invoke **`sh-security-review`** with `since:<ref>`: it reuses the earlier map and threat model and hunts only the change. |
| Just to learn how a class is detected/exploited, or to hand a hunter its knowledge | Invoke the matching **`sh-kb-<class>`** skill directly (no pipeline). |
| A **threat model**, trust boundaries, a data flow diagram, STRIDE, "what could go wrong", or a review of a design document or architecture | Invoke the **`sh-threat-model`** skill. Pass `design:<path>` for a design document, `since:<ref>` for a change. |
| **Detection rules**, Sigma, SIEM rules, "why didn't we alert", detection gaps, purple-team follow-up | Invoke the **`sh-detection-rules`** skill. |
| To **triage alerts**, investigate a host or alert, **hunt** for a technique or indicator, or review logs | Invoke the **`sh-log-triage`** skill (read-only; SIEM sources through `indra siem`). |
| A **hardening** or remediation plan, "what do we fix first", secure configuration | Invoke the **`sh-hardening`** skill. |
| The **attack surface**, exposure, "which vulnerabilities actually matter", "are these CVEs reachable" | Invoke the **`sh-attack-surface`** skill (it fills reachability into a review run; the console draws the graph). |
| To map/understand a codebase only (no vuln hunt) | Invoke `sh-security-review` with `stage:recon`. |
| To recall or record what reviews have learned ("what do we know about X", "remember this", "show the knowledge graph", "forget/skip memory") | Invoke the **`sh-memory`** skill. For "don't use memory" on a review, pass `memory:off` to `sh-security-review`. |
| To put findings where the team works ("file these in Jira", "open GitHub issues", "put the report in Confluence/Notion") | Invoke the **`sh-publish`** skill. It asks before creating anything and never sends evidence or payloads. |

## Class slug map (route free-text to the right sh-kb-* skill / class filter)

- access control, authorization, IDOR, BOLA, privilege escalation, missing authz -> **access-control**
- SQL injection, SQLi, NoSQL injection (query) -> **sqli**
- XSS, cross-site scripting, DOM XSS -> **xss**
- SSRF, server-side request forgery -> **ssrf**
- command/OS injection, template injection (SSTI), LDAP, eval -> **injection**
- authentication, login, session, JWT, password, MFA -> **auth**
- deserialization, pickle, unmarshal, gadget -> **deserialization**
- path traversal, LFI, RFI, directory traversal, arbitrary file read -> **path-traversal**
- secrets, hardcoded credentials, API keys, tokens in code -> **secrets**
- CSRF, cross-site request forgery -> **csrf**
- XXE, XML external entity -> **xxe**
- open redirect, unvalidated redirect -> **open-redirect**
- crypto, weak hashing, insecure random, encryption misuse -> **crypto**
- race condition, TOCTOU, concurrency bug -> **race-conditions**
- file upload, unrestricted upload, malicious file -> **file-upload**
- outdated/vulnerable dependency, CVE, SBOM -> handled in recon (**stage:recon** or full run)

## Steps
0. **Is this about a pull request?** If the request contains a GitHub PR link, names a PR number, says "this PR", "these changes",
   "before merge", or the user is on a branch with an open PR and asks for a review, route to
   **`sh-pr-review`** and stop. A PR review is scoped to the diff, posts back to the PR, and sets a
   commit status; a codebase review is not and does neither. Do not substitute one for the other.
1. Identify the **target path** (default: current working directory) and whether the user restricted the
   scope to specific classes or a single stage.
2. Confirm authorization context briefly if the request implies attacking systems the user may not own
   (this harness does static analysis only; decline live attacks on third-party targets).
3. Route per the table. When ambiguous between "full review" and "specific classes", default to a full
   review but state the assumption so the user can narrow it.
4. Hand off by invoking the chosen skill with the parsed arguments. **Pass through** any `classes:`,
   `stage:`, `depth:`, and `models:` tokens verbatim so the pipeline honors them (see the model-override
   options in `sh-security-review`). Do not duplicate its work here.

## Notes
- **Token use**: a review reuses an unchanged map, hunts only classes with an open threat, and briefs
  each hunter instead of handing it the whole recon. For repeat reviews suggest `since:<ref>`; for a
  first, cheap pass `models:cheap`. Pass `threats:off` or `reuse:off` through when the user asks.
- **Model / cost control**: `sh-security-review` runs each stage on a cost-appropriate model by default
  (recon=sonnet, hunt=tiered haiku/sonnet/opus per class, chain/verify=opus, report=haiku). Users can
  override with `models:` (e.g. `models:max`, `models:cheap`, `models:verify=opus,hunt=sonnet`) - pass these
  through unchanged.
- The pipeline writes everything under `<target>/.security-harness/<run-id>/`; point the user there.
- If the user asks something outside security review (general coding), say this harness is scoped to
  security work and let the normal assistant handle it.
