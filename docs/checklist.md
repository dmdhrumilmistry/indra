# Checklist

What is being finished, what is done, and what is still open. New items go to the end of
their section; nothing is deleted, items are checked off with the change that did them.

## Claude Code on Ollama

- [x] `provider = "ollama"` for a whole agent (Claude Code, Codex) and `subagent_provider` for
      Claude's subagents only (#32)
- [x] The model router: routes by model, streams, keeps every credential off Ollama (#32)
- [x] Workflow tasks override `model` and `provider`; Launch has **Runs on** (#32)
- [x] One shared setup for Claude Code on Ollama (console launches, terminal, CLI)
- [x] `indra claude -m MODEL`: Claude Code in your own terminal on an Ollama model, with checks,
      `--pull`, `--subagents`, `--profile`, `--dry-run`
- [x] Preflight checks: server up, model pulled, tool calling, context window, Ollama version
- [x] Tell Claude Code the real context window (`CLAUDE_CODE_MAX_CONTEXT_TOKENS`); it assumes
      1M for models it does not know and would never compact
- [x] Browser terminal: Claude Code with a profile (an Ollama one included), and resuming an
      Ollama agent's session on Ollama, not Anthropic
- [x] Console: Ollama card on Tools (server, models, tool support, context, pull with progress)
- [x] Launch warns before starting an agent on a missing or unsuitable Ollama model
- [x] Docs: `indra claude`, the context window, the checks
- [x] Browser terminal: the profile was dropped by term.js before it reached the server (found
      by running the real Claude Code TUI through the terminal; fixed)
- [ ] Test against a real Ollama server (needs permission to run the Ollama binary in the
      test container; done so far against a stand-in that speaks Ollama's API)

## Review loop

- [x] Start a PR review from the console, triage findings, compare runs (#33)
- [x] Claude Code on Ollama end to end: `indra claude`, terminal profiles, checks, pulls (#34)

## Partly implemented (from the audit)

- [x] Settings > General save drops `[server] webrtc` and `ice_servers`; no UI for them
- [x] Runner form drops `model_args` (Customize or Edit on codex or gemini breaks `model`)
- [x] Workflow "Edit as form" drops `component`, `workflow` and `pos` on tasks
- [x] Settings forms in general: keep keys a form does not show instead of dropping them
- [x] Nodes that dial in (no `ssh`): the form requires ssh, the summary prints "undefined",
      `/api/nodes/runs` and `indra nodes` only work over SSH (runs now come over the link)
- [x] TURN servers cannot carry a username and credential
- [x] Linear is named in the architecture docs but there is no Linear connector
- [x] A fresh pivot cannot switch runner, profile or model (the guide says it can)
- [x] Tracker import stops at 50 items (no paging)
- [x] Leftover "bundled script" wording and a stale `scripts/` folder after the Python removal
- [x] Dead `/api/memory` endpoint
- [x] Board: the Run form cannot pick a runner
- [ ] Runs on remote nodes are listed but cannot be opened, triaged or posted (needs the run's
      files from the node, over SSH or the link)
- [x] tools/indra/README.md Settings table lists 7 of the 13 tabs

## Not planned yet (the docs say so)

- [ ] OS keychain backend for secrets
- [ ] Several hubs sharing one fleet of nodes
- [ ] A DAST bridge for `needs-runtime` findings
