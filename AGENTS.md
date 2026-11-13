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
python3 - <<'PY'
import os, io
DASHES = {chr(0x2014): '-', chr(0x2013): '-', chr(0x2015): '-'}
for root, dirs, fs in os.walk('.'):
    dirs[:] = [d for d in dirs if d != '.git']
    for f in fs:
        p = os.path.join(root, f)
        try: s = io.open(p, encoding='utf-8').read()
        except Exception: continue
        if any(k in s for k in DASHES):
            for k, v in DASHES.items(): s = s.replace(k, v)
            io.open(p, 'w', encoding='utf-8', newline='').write(s)
            print('fixed', p)
PY
```

## Repository layout

```
.claude-plugin/marketplace.json     # Claude Code marketplace manifest
plugins/indra/                      # the plugin itself
  agents/                           # sh-recon, sh-hunter, sh-chainer, sh-verifier, sh-reporter
  skills/                           # CANONICAL skills: indra, sh-security-review, sh-pr-review, sh-memory, sh-kb-* (15)
  references/                       # shared reference docs
gemini-extension.json               # Gemini CLI extension manifest
GEMINI.md                           # Gemini context file
skills/                             # GENERATED mirror of the canonical skills
scripts/sync-agent-skills.py        # regenerates that mirror
site/                               # the GitHub Pages website (deployed by .github/workflows/pages.yml)
assets/                             # logo, banner, social image and screenshots (README, site, console icon)
tools/indra/                        # indra: one Rust CLI - graph memory (indra mem) and agent web console (indra serve)
docs/DISTRIBUTION.md                # packaging and public-listing steps
.github/actions/ai-agent/           # pluggable agent runner (claude | codex | gemini | custom)
.github/workflows/                  # KB stage 1 (open PR), stage 2 (review and merge)
.github/kb-update/                  # prompts and trusted-source allowlist for the KB updater
```

**`skills/` at the repo root is generated. Never edit it.** Edit
`plugins/indra/skills/` and run:

```bash
python3 scripts/sync-agent-skills.py           # regenerate
python3 scripts/sync-agent-skills.py --check   # verify it is current
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
directory, and UI strings rendered with `textContent` only. `.github/workflows/indra.yml`
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
