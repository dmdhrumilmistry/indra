# Ideas from ARTEX

[ARTEX](https://github.com/BaaRai/ARTEX) is a self-hosted, multi-agent penetration testing platform:
a planner splits a goal into intents, workers run real tools against live targets, and every finding
stays bound to the assets and HTTP traffic that prove it. This note records which of its features fit
indra, which indra already has, and in what order to build the rest. The open items are tracked in
[the checklist](checklist.md).

## Two constraints

- **Ideas only, no code.** ARTEX is AGPL-3.0; indra is MIT. Copying its code would put indra under
  AGPL terms. Everything below is written fresh in Rust.
- **indra does not own the model loop.** ARTEX runs its own agent runtime (Go, PostgreSQL), so it can
  frame tool output, inject steering and count tokens on every call. indra drives Claude Code, Codex and
  Gemini CLI from outside. Each feature has to go through what indra controls: the `indra guard`
  pre and post hooks, the agent's environment, the stream-json control channel (`interrupt`,
  `set_model`), skills and agent prompts, and files in the run directory.

## What indra already has

| ARTEX | indra | Gap |
|---|---|---|
| Scope and target gate | Engagements and `indra guard pre` (`tools/indra/src/deck/engage.rs`) | No network-layer enforcement: the guard does not see redirects or hosts a program reads from a file |
| Interception gate | Console approvals, hold to allow | Rules by argv prefix that cannot be widened through pipes or wrappers (`sudo`, `sh -c`), and who decided (human, rule, system) |
| Task and intent budgets | `max_budget_usd` per launch or profile | A budget across a workflow or engagement, turn caps, and a forced wrap-up before the limit |
| Noise governor | Calls per second per host (`pace()`) | Profiles (quiet, balanced, aggressive) and backoff on 429 and 503 |
| Approval and audit history | SHA-256-chained audit log | None |
| Evidence-gated lessons | `indra mem`, the learnings inbox, triage reasons | Each lesson linked to the finding it came from, withdrawn when the triage is reversed, a support count |
| Shared worker traces | The attempts ledger shared by hunters | None for static review |
| LLM profiles | Profiles, the model router, Ollama | Failover to another provider |
| Secrets at rest | The encrypted secrets store | None |

## To build, in order

1. **Coverage.** For an engagement: in-scope hosts against the hosts the audit log shows were
   touched. For a review: entry points in `codebase-map.json` against the attempts ledger. Both come
   from data indra already writes, shown on the attack surface graph as tested and untested.
2. **Steering through the hook.** A queued instruction that `indra guard pre` hands to the agent
   before its next tool call, so it changes course without being interrupted. A lighter companion to
   Pivot.
3. **Lessons with evidence.** A lesson is admitted only with a cue (when it applies), an action (what
   to do) and the finding it came from; reversing that finding's triage withdraws it. Rules only, no
   model summary, so every lesson traces to a finding.
4. **Pacing and budgets.** Back off a host after a 429 or 503 seen by `indra guard post`. A budget
   across an engagement or workflow, with a wrap-up instruction near the limit.
5. **Recording proxy and evidence-bound findings.** `indra proxy` per engagement, with its own CA,
   handed to the agent through its environment (never installed system-wide). It enforces the scope
   at the network layer and records every exchange. A finding links to the exact request and
   response, each with a role (baseline, proof, verification, supporting), stored by SHA-256. A
   report is flagged stale when its evidence changes.
6. **Retest.** A `sh-retest` skill and a Retest button on a finding: one verifier re-checks it on new
   code, or replays its recorded traffic inside an engagement, and answers Still reproducible, Fixed
   or Cannot confirm. The replay path is the DAST bridge for `needs-runtime` findings.
7. **Untrusted output framing.** Nonce-tagged markers around the output of indra's own MCP server and
   data sources. For built-in tools of Claude Code, only a note is possible through hooks.

Items 1 to 4 are small and reuse what indra already records. Item 5 is the large one and unlocks 6.

## Not porting

- **The planner and worker engine.** Autonomous live testing fits better as a skill run inside an
  engagement, using workflow loops and plan-and-delegate, with its goals, intents, facts and findings
  written as linked notes in a memory vault.
- **PostgreSQL, the Next.js UI, S3 task archives, chat bridges, ScopeSentry sync.** They cut against
  one binary with plain files.
