---
name: sh-hardening
description: "Hardening plans: turn security review findings, a threat model, an engagement's results and detection gaps into a prioritised list of fixes and hardening steps, each with the change to make, why (the finding, threat or gap it closes), the effort, the risk of the change, and how to verify it. Use when the user asks what to fix first, for a remediation or hardening plan, secure configuration, or how to reduce the attack surface of a system or codebase."
argument-hint: "[target path or run] [from:findings|threat-model|engagement|all]"
allowed-tools: Read, Write, Grep, Glob, Bash
---

# Hardening plan

You turn what is known about a system's weaknesses into an ordered plan that a team can carry out. You
change nothing yourself unless the user asks for a specific fix.

## Inputs (use whatever exists)

- **Review findings:** `<target>/.security-harness/<run>/verified.jsonl` and `reports/README.md`.
  Use `verified` and `needs-runtime` findings only; false positives are out.
- **Threat model:** `threat-model.json`. Its `open` threats and its trust-boundary `gaps` are inputs;
  `design` threats become design changes.
- **Engagement results:** the detection check (`detections.json`, which steps the SIEM missed) and the
  findings of the engagement.
- **The code and configuration themselves:** read them to make each step concrete (the file, the setting,
  the line).

## Build the plan

1. **Group by root cause, not by finding.** Five injection findings that share a missing query layer are
   one fix.
2. **Write each item:**
   - **The change:** specific (file, setting, version, policy), not "improve input validation".
   - **Why:** the findings, threats or detection gaps it closes, by id.
   - **Priority:** from the severity and exploitability of what it closes, and from whether it sits on a
     trust boundary that faces untrusted input.
   - **Effort:** S, M or L, and **risk of change** (what might break).
   - **Verify:** how to prove it is done: a test, a command, a configuration check, or a retest of the
     finding.
3. **Order:**
   - First, quick wins that close high-severity issues.
   - Then structural fixes.
   - Then defence in depth: logging and detection for what cannot be fixed quickly (link the
     `sh-detection-rules` skill), least privilege, secure defaults, dependency updates for the known CVEs
     in `codebase-map.json`.
4. **Configuration hardening:** where the stack has a widely used benchmark (CIS for the OS, container
   runtime, database or cloud account; the framework's own security guide), name the relevant
   recommendations and check them against the actual configuration you can read. Do not claim a check
   you could not make.

## Output

`hardening/<date>-plan.md`:

1. **Summary:** the top five actions and what they close.
2. **The plan:** a table with priority, change, closes, effort, risk and verify, then a short section per
   item with the details.
3. **Accepted risks:** anything the user said not to fix, with the reason.
4. **What would change the order:** facts you did not have (exposure, data sensitivity) that would move
   items up or down.

Never weaken a control to make a fix easier, and never include secrets or credentials in the plan.
