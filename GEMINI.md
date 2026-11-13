# security-harness

Context file for the Gemini CLI extension. Loaded at session start.

This extension bundles a multi-agent application-security review harness. The
capability lives in the agent skills under `skills/`, which are disclosed on
demand rather than loaded here, so this file stays short.

## When to activate a skill

- Any request to **security review, audit, or pentest a codebase**, or to find
  vulnerabilities in it: activate `sh-router`. It interprets the request and
  points at the right pipeline stage or knowledge base.
- A request naming **one vulnerability class** (SQLi, XSS, IDOR, SSRF, CSRF,
  XXE, SSTI, path traversal, deserialization, secrets, crypto, race conditions,
  file upload, open redirect, auth, access control): activate that class's
  `sh-kb-*` skill directly.
- A request for the **full pipeline** or a written report: activate
  `sh-security-review`.
- A request to **recall or record what earlier reviews learned** ("what do we
  know about X", "remember this"): activate `sh-memory`. It needs the `indra`
  CLI and is skipped cleanly without it.

## Pipeline shape

`recon -> recall -> hunt (parallel, one per class) -> chain -> verify -> report -> remember`

`recall` and `remember` are the optional long-term memory steps (`sh-memory`).

Each stage has a matching sub-agent definition under
`plugins/security-harness/agents/` and shared reference material under
`plugins/security-harness/references/`. Read those when you need the detail of a
stage rather than guessing at it.

## Scope and safety

This harness is for reviewing code you are authorized to review. It reads and
reasons about source; it does not attack live systems. Findings carry payloads
and proof-of-concept steps so a human can reproduce them in a test environment.

Do not weaken a knowledge base's false-positive filters or remove a mitigation
in order to produce more findings.

## Writing style

Never use em dashes or en dashes. Use a plain ASCII hyphen (`-`). See
`AGENTS.md` for the full style rules, which apply to every agent working here.
