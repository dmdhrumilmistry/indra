---
name: sh-threat-model
description: "Threat-model a codebase, a design document, or a change: actors, assets, data flows, trust boundaries with the controls at each, STRIDE threats ranked by risk, a data flow diagram, and a hunt plan for a security review. Use when the user asks for a threat model, trust boundaries, a data flow diagram, STRIDE, an attack surface or architecture risk review, or 'what could go wrong' with a system or design. Usually invoked via the indra skill."
argument-hint: "[target path, default: cwd] [design:<doc path>] [since:<git ref>] [models:threat=<model>]"
---

# Threat model

You are the **orchestrator** of a short pipeline: map (reusing a recent map when there is one), then
model. The modelling is done by the `sh-threat-modeler` agent; the mapping by `sh-recon`.

## Read first

- `${CLAUDE_PLUGIN_ROOT}/references/threat-modeling.md` - the method and the output files.
- `${CLAUDE_PLUGIN_ROOT}/references/state-files.md` - the run directory contract.

## Arguments (all optional)

- **target**: the codebase. Default: current working directory.
- **`design:<path>`** (repeatable): threat-model a design document (Markdown, text, an ADR, an RFC)
  instead of code. No recon runs. If a target codebase also exists, recon still runs so the model
  can say which parts of the design are already built.
- **`since:<git ref>`**: diff mode. Model only what changed since that ref (a branch, tag or commit),
  starting from the previous threat model when there is one.
- **`models:threat=<model>`**, **`models:recon=<model>`**: the model per agent (defaults: sonnet for
  both; use `opus` for a large or high-stakes system).

## Steps

1. **Set up the run** exactly as `sh-security-review` Stage 0 steps 1-3 and 8-9 do (run id, run
   directory, `latest`, `scope.json` with `mode: "threat-model"`, `.gitignore`). Skip tool installs:
   a threat model needs no scanners. Record `git rev-parse HEAD` (when the target is a git repo) as
   `scope.json.commit`.
2. **Map** (code and diff modes): reuse a recent recon when it still describes the code (see
   "Reusing recon" in `sh-security-review`). Otherwise spawn `sh-recon` with `depth:quick` semantics
   (structural Graft only, no SBOM or CVE scan unless the user asked for dependencies).
   In diff mode, list the changed files with `git diff --name-only <ref>...HEAD` and pass them on.
3. **Model**: spawn `sh-threat-modeler` with `model` = `models.threat` (default sonnet), passing
   `run_dir`, the mode, the design document paths or the changed files.
4. **Finish**: tell the user the run directory, the number of trust boundaries, threats by risk and
   how many are open, the top three open threats in one line each, and the output paths. Offer the
   next step: `/sh-security-review stage:hunt` hunts the open threats of this run (it reads the hunt
   plan), and `/sh-publish` files the open threats as issues.

## Rules

- Static analysis of code and documents the user controls. No live testing.
- Never fabricate components, flows or controls. "Not found in the code" is a valid answer.
