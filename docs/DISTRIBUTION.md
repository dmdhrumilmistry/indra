# Distribution

How this repo is packaged for each agent ecosystem, and the exact steps to list
it in the public directories.

## Layout

One canonical copy of the skills, mirrored where each ecosystem looks for them.

| Path | Purpose | Generated? |
|---|---|---|
| `plugins/indra/skills/` | canonical source of truth | no, edit here |
| `plugins/indra/.claude-plugin/plugin.json` | Claude Code plugin manifest | no |
| `.claude-plugin/marketplace.json` | Claude Code marketplace manifest | no |
| `skills/` | Gemini CLI loads an extension's skills from the extension root | **yes** |
| `gemini-extension.json` | Gemini CLI extension manifest | no |
| `GEMINI.md` | Gemini context file, named by `contextFileName` | no |
| `AGENTS.md` | read by Codex and most other agents | no |
| `CLAUDE.md` | Claude Code; points at `AGENTS.md` | no |

Regenerate the mirror after any skill edit:

```bash
indra skills sync
indra skills sync --check   # CI-friendly, non-zero if stale
```

## Per-ecosystem support

### Claude Code

Native plugin. `.claude-plugin/marketplace.json` makes the repo its own
marketplace, so no third-party hosting is required:

```
/plugin marketplace add dmdhrumilmistry/indra
/plugin install indra@indra
```

### Gemini CLI

Native extension. The manifest sits at the repo root, and Gemini loads the
bundled skills from `skills/`:

```bash
gemini extensions install https://github.com/dmdhrumilmistry/indra
```

Manifest fields used (`name`, `version`, `description`, `contextFileName`,
`excludeTools`) are per the Gemini CLI extension reference.

### OpenAI Codex

Codex reads `AGENTS.md`, which is already at the repo root and is the shared
source of truth for every agent. For the skills, copy them into a discovery
directory:

```bash
indra skills install --to agents
```

### opencode and other agentskills.io agents

opencode discovers `SKILL.md` from `.agents/skills/`, `~/.agents/skills/`,
`.claude/skills/`, `~/.claude/skills/`, `.opencode/skills/`, and
`~/.config/opencode/skills/`. `indra skills install` writes to whichever you want:

```bash
indra skills install --to agents                       # ~/.agents/skills
indra skills install --to opencode                     # ~/.config/opencode/skills
indra skills install --to claude                       # ~/.claude/skills
indra skills install --to agents --scope workspace     # ./.agents/skills
```

`~/.agents/skills/` is the interoperable path, so prefer it unless a specific
agent needs its own directory.

## Public directory listings

Run these yourself. They are outward-facing and touch accounts or third-party
repos.

### 1. Gemini CLI extension gallery (automatic, no form)

The gallery crawls public repos daily. Three requirements: public repo, the
`gemini-cli-extension` topic, and `gemini-extension.json` at the repo root. The
manifest is already in place, so add the topic and cut a matching release:

```bash
gh repo edit dmdhrumilmistry/indra --add-topic gemini-cli-extension

# Keep the release tag and the manifest version in step.
git tag v0.2.0 && git push origin v0.2.0
gh release create v0.2.0 --title "v0.2.0" --generate-notes
```

Also worth adding for discoverability:

```bash
gh repo edit dmdhrumilmistry/indra \
  --add-topic claude-code-plugin \
  --add-topic agent-skills \
  --add-topic appsec \
  --add-topic security-tools
```

### 2. Anthropic official plugin directory (form)

Submit at <https://clau.de/plugin-directory-submission>. External plugins are
reviewed against quality and security standards.

Details to paste into the form:

- **Name:** `indra` (immutable once published; the UI label is the
  `displayName`, `Indra`)
- **Repository:** <https://github.com/dmdhrumilmistry/indra>
- **Marketplace:** `dmdhrumilmistry/indra`
- **License:** MIT
- **Category:** security
- **Description:** Multi-agent application-security review harness. A router
  skill dispatches to a full pipeline (recon, hunt, chain, verify, report)
  backed by 15 per-class vulnerability knowledge bases, using Graft for codebase
  mapping and SCA tooling for SBOM and CVE data. Outputs README, JSON, SARIF,
  HTML, and PDF.

Before submitting, confirm the plugin directory carries what reviewers expect:
`plugin.json`, `README.md`, `agents/`, `skills/`, and a stated license. All are
present.

### 3. Community directories (PRs to third-party repos)

Each wants an entry in its own list. Suggested one-liner:

> **[indra](https://github.com/dmdhrumilmistry/indra)** -
> Multi-agent application-security review: recon, parallel per-class vulnerability
> hunting, exploit chaining, impact verification, and SARIF/PDF reporting.

Targets:

- <https://github.com/Chat2AnyLLM/awesome-claude-plugins>
- <https://github.com/ananddtyagi/cc-marketplace>
- <https://claudemarketplaces.com/> (indexes GitHub, so repo topics may be enough)
- <https://buildwithclaude.com/>
- <https://www.aitmpl.com/plugins/>

Several of these index GitHub automatically, so doing step 1's topics first may
make some of these unnecessary.

## Release checklist

1. `indra skills sync --check`
2. Version matches in `plugins/indra/.claude-plugin/plugin.json`,
   `.claude-plugin/marketplace.json`, and `gemini-extension.json`
3. No em dashes: `indra dev dashes`
4. Tag and release with the same version as the manifests

### The indra binary

The CLI is released as `v<version>` (releases up to 0.6.0 were `indra-v<version>`; the
installers and `indra update` understand both):

1. Set `version` in `tools/indra/Cargo.toml` (CI refuses a release that does not match it).
2. Merge, then either run **Actions > indra > Run workflow** on `main` with the version
   (`0.7.0`), which creates the tag and the release on that commit, or push a tag yourself:
   `git tag v0.7.0 && git push origin v0.7.0`.
3. CI tests on Linux, macOS and Windows, builds the five binaries (Linux x86_64 and arm64
   static, macOS Apple silicon and Intel, Windows x86_64), uploads each with its `.sha256`, then
   adds `SHA256SUMS`, `install.sh` and `install.ps1` to the release, and only then marks it the
   repository's **Latest** release (it is created as not-latest, so a half-uploaded release never
   is). Last, it publishes the crate to crates.io (`cargo install indra`) with the
   `CARGO_REGISTRY_TOKEN` repository secret (a crates.io API token with the publish-new and
   publish-update scopes); without the secret it says so and skips it, and a version already on
   crates.io is left alone.
   To mark an existing release Latest without building, run the workflow with **latest** set to
   its version (`0.6.0`); an old `indra-v<version>` release is renamed to `v<version>` on the way.
4. Check the one-liners against it:
   `curl -fsSL https://raw.githubusercontent.com/dmdhrumilmistry/indra/main/install.sh | sh`
   and, on Windows, `irm https://raw.githubusercontent.com/dmdhrumilmistry/indra/main/install.ps1 | iex`.
