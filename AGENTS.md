# Agent instructions

Shared instructions for every AI agent working in this repository: Claude Code,
OpenAI Codex, Gemini CLI, Cursor, and anything else. `CLAUDE.md` points here, so
there is one source of truth.

## Writing style

**Never use em dashes (`-`) or en dashes (`-`). Use a plain ASCII hyphen (`-`).**

This applies to everything you produce here: Markdown, code comments, commit
messages, PR titles and bodies, YAML comments, and generated knowledge-base
content.

- Correct: `the reviewer is fail-closed - a missing verdict is a REJECT`
- Wrong: the same sentence with an em dash in place of that hyphen.

For an aside that would take an em dash, use a spaced hyphen (` - `), a comma, a
colon, or parentheses. Prefer splitting into two sentences where that reads
better.

Other typography rules:

- Plain ASCII quotes (`'` and `"`), not curly quotes.
- `...` rather than the ellipsis character.
- Keep Unicode out of file paths, identifiers, and CLI flags.

A repo-wide sweep is available if a dash slips in:

```bash
indra dev dashes          # list any (exit 1 if found; CI runs this)
indra dev dashes --fix    # replace them with a plain hyphen
```

## Repository layout

```
.claude-plugin/marketplace.json     # Claude Code marketplace manifest
plugins/indra/                      # the plugin itself
  agents/                           # sh-recon, sh-hunter, sh-chainer, sh-verifier, sh-reporter
  skills/                           # CANONICAL skills: indra, sh-security-review, sh-pr-review, sh-memory, sh-publish, sh-kb-* (15)
  references/                       # shared reference docs
gemini-extension.json               # Gemini CLI extension manifest
GEMINI.md                           # Gemini context file
skills/                             # GENERATED mirror of the canonical skills (`indra skills sync`)
site/                               # the GitHub Pages landing page (deployed by .github/workflows/pages.yml)
site/pages.toml                     # the docs pages `indra dev site` renders from the Markdown
assets/                             # logo, banner, social image and screenshots (README, site, console icon)
tools/indra/                        # indra: the one Rust binary - memory, console, review tooling, site builder; no Python anywhere
docs/DISTRIBUTION.md                # packaging and public-listing steps
docs/guides/                        # user guides: processes, pivoting, knowledge base and integrations
.github/actions/ai-agent/           # pluggable agent runner (claude | codex | gemini | custom)
.github/workflows/                  # KB stage 1 (open PR), stage 2 (review and merge)
.github/kb-update/                  # prompts and trusted-source allowlist for the KB updater
```

**`skills/` at the repo root is generated. Never edit it.** Edit
`plugins/indra/skills/` and run:

```bash
indra skills sync           # regenerate
indra skills sync --check   # verify it is current (CI runs this)
```

It exists because Gemini CLI loads an extension's skills from the extension root,
while the Claude Code plugin format fixes them under the plugin directory.

## indra (Rust)

`tools/indra` is one Rust crate and one binary: `src/mem/` is the graph memory
(`indra mem`), `src/deck/` the agent web console (`indra serve`, `ls`, `check`,
`init`), and `ui/` the console's embedded front end. Before committing a change there, run:

```bash
cd tools/indra
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
```

Keep it cross-platform: no Unix-only APIs outside `#[cfg(unix)]` with a Windows
counterpart, paths built with `Path`, and tests that pass on Windows. Memory notes must
stay readable by Obsidian. The console can start processes, so keep its security model
intact (see `tools/indra/README.md`): token on every API call, loopback Host check,
same-origin writes, no shell, files confined to what discovery found or to a run
directory, and UI strings rendered with `textContent` only. Remote nodes go over SSH
with `BatchMode=yes`; the only remote command is `indra node <info|runs|exec>` and the job
travels as JSON on stdin, never through a remote shell, so keep it that way. `.github/workflows/indra.yml`
tests it on all three OSes and publishes binaries on an `indra-v*` tag.

## CI: the two-stage knowledge-base updater

1. `update-knowledge-base.yml` runs an agent that edits the `sh-kb-*` knowledge
   bases from an allowlisted set of sources, then opens a PR. It never writes to
   `main`, and it never merges.
2. It then dispatches `kb-review-and-merge.yml`, which runs a second, adversarial
   agent over the diff. That workflow merges the PR only on `VERDICT: APPROVE`,
   and is fail-closed: no verdict file means REJECT.

If you change the generator's `WebFetch(domain:...)` allowlist, change
`.github/kb-update/trusted-sources.md` to match. The two must stay in sync.

## Boundaries

- Do not edit `.github/workflows/**` or `.github/kb-update/*prompt*.md` from an
  automated KB run. The reviewer rejects those diffs by design.
- Do not commit secrets. CI reads `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`,
  `OPENAI_API_KEY`, and `GEMINI_API_KEY` from repository secrets.
- Knowledge-base edits belong in `plugins/indra/skills/sh-kb-*/`.
