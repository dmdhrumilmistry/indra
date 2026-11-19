---
name: sh-attack-surface
description: "Map a codebase's attack surface and decide which of its vulnerabilities actually impact it: what an attacker can reach without a login, after one, or not at all. Covers known CVEs in dependencies (is the vulnerable code even called?) and review findings. Fills reachability into an existing review run so the indra console can draw the graph, and writes a short report. Use when the user asks for the attack surface, exposure, 'which vulnerabilities actually matter', 'are these CVEs reachable', or to prioritise findings by exposure."
argument-hint: "[target path, default: cwd] [run:<run-id>, default: latest]"
allowed-tools: Read, Write, Edit, Grep, Glob, Bash
---

# Attack surface and reachability

A vulnerability matters when an attacker can reach it. You map who can reach what, and record it in
the run so the console can rank and draw it (**Results > the run > Attack surface**).

## Inputs

- The run: `<TARGET>/.security-harness/<run-id>/`. Without `run:`, use the one named in
  `<TARGET>/.security-harness/latest`.
- Its `codebase-map.json`: entry points with `auth_required`, dangerous sinks, `known_cves`.
- Its findings: `verified.jsonl`, else `findings.jsonl`.
- Its `threat-model.json` when present: its trust boundaries say where trust changes.

If the run has no `codebase-map.json`, run the `sh-security-review` skill with `stage:recon` first.

## 1. Check the entry points

For each entry point, confirm `auth_required` from the code: middleware, decorators, router guards. A
route that is wrongly listed as guarded hides real exposure. Add entry points recon missed when you
find them, such as webhooks, message consumers, file uploads and admin routes.

## 2. CVEs: is the vulnerable code called?

For each `known_cves` entry without a `reachability`:

1. From the advisory, name the affected function, class or feature in `vulnerable`. Keep to what the
   advisory says, and do not invent details.
2. Search the code for its import and its call sites, outside tests.
3. Set `reachability`:
   - `reachable`, with `evidence` (`file:line` of each call site);
   - `not_reachable` (a dev or test-only dependency, or the vulnerable part is never used), with the
     reason in `notes`;
   - `unknown` (transitive, dynamic or reflective use).
4. If it is reachable, say whether input from an entry point can get to the call. Note it in `notes`.

## 3. Findings: who can reach them?

For each finding that is not a false positive and has no `reachability`:

- `entry_point`: where its data flow starts (`"POST /api/login"` as recon lists it, or `file:line`).
- `reachability`: `unauthenticated`, `authenticated`, `internal`, `unreachable` or `unknown`. Base it
  on the guards actually on the path.

## Write it back

Rewrite `codebase-map.json` and `verified.jsonl` (or `findings.jsonl`) with the new fields. Change
nothing else in them, and keep them valid JSON and JSONL.

## Report

Write `<run_dir>/attack-surface.md`:

1. **Summary**:
   - the entry points, and how many need no login;
   - how many vulnerabilities an attacker can reach from the internet, and how many after login;
   - how many are not reachable.
2. **What actually impacts you**: the reachable ones, worst first, each with its path (entry point to
   component to vulnerability) and the fix or the version that fixes it.
3. **Not reachable**: what can wait, and why, so nobody spends time on it.
4. A Mermaid `flowchart LR` from Internet and Signed-in user to the reachable vulnerabilities.

Then tell the user the numbers, and to open the run in the console for the graph.
