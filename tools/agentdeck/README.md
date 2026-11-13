# agentdeck

A local web console for AI agent harnesses. One ~2 MB Rust binary, with the UI embedded,
no runtime, no database and no CDN.

- **Agents, skills, prompts**: everything a harness is made of, discovered from its files.
  Read them, see what each one uses and is used by, and edit them in the browser.
- **Graph map**: how the harness fits together. Pipeline stages run agents, agents spawn
  agents and invoke skills, and everything reads its prompts.
- **Memory**: the [memex](../memex/README.md) vaults the harness reads and writes, drawn
  as graphs: past findings, learnings, and how they relate.
- **Runs**: live monitoring of run directories. Stage progress, results per agent or
  class, severity and status counts, and every file the run wrote, refreshed every 3 s.
- **Launch and watch**: start an agent through a configured runner (Claude Code, Codex,
  Gemini CLI or your own) and follow it live. With Claude Code you see each subagent it
  spawns, the tool calls each one makes, refused permissions, and the final cost.

The **security harness is the default profile**. It also works with any Claude Code
project and with any custom harness described by a few globs.

## Install

```bash
cargo install --locked --git https://github.com/dmdhrumilmistry/security-harness agentdeck
```

Or download `agentdeck-<target>` from the latest `agentdeck-v*`
[release](https://github.com/dmdhrumilmistry/security-harness/releases) and put it on your
`PATH`. Builds exist for Linux (x86_64 and arm64, static), macOS (Apple silicon and Intel)
and Windows (x86_64).

## Run it

```bash
cd ~/code/security-harness            # or any project; see "Profiles" below
agentdeck --target ~/code/my-api      # the codebase whose runs to watch and where agents run
```

```text
agentdeck 0.1.0
  harness  security-harness  (~/code/security-harness/plugins/security-harness)
  harness  claude-code  (~/code/security-harness)
  runners  claude

Open: http://127.0.0.1:7878/?token=3f9c...
(the token in this URL is the password; keep it private)
```

Open the printed URL once. It exchanges the token for a cookie and you are in. Add
`--open` to open the browser for you, `--read-only` to turn off launching and editing.

Everything is also available without the browser:

```bash
agentdeck ls --defs        # harnesses, every agent/skill/prompt, recent runs with stage progress
agentdeck check            # validate the config: globs that match nothing, unknown stage agents, runners
agentdeck init             # write a commented agentdeck.toml to start from
```

## Profiles

A *profile* tells agentdeck where a harness keeps its parts. Two are built in and detected
automatically:

| Profile | Found when | Agents | Skills | Prompts | Runs |
|---|---|---|---|---|---|
| `security-harness` | you run inside this repository, or the plugin is installed under `~/.claude/plugins` | `agents/*.md` | `skills/*/SKILL.md` | `references/*` | `<target>/.security-harness/run-*`, stages recon -> hunt -> chain -> verify -> report |
| `claude-code` | the project or `~/.claude` has agents, skills or commands | `.claude/agents/*.md` | `.claude/skills/*/SKILL.md` | `CLAUDE.md`, `AGENTS.md`, commands | - |

A custom harness is a few lines of TOML (`agentdeck.toml` in the current directory, or
`~/.config/agentdeck/agentdeck.toml`, or `--config FILE`):

```toml
[[harness]]
name     = "research-agents"
root     = "."
agents   = ["agents/*.md"]
skills   = ["skills/*/SKILL.md"]
prompts  = ["prompts/**/*.md"]
memory   = ["{target}/.memory"]         # memex vaults; {target} expands per target
targets  = ["../project-a"]
runs     = ".runs/*"                    # one directory per run, under each target
findings = ["results.jsonl"]            # JSONL results inside a run
group_by = "agent"                      # which field names the agent that produced a result
stages   = [
  { name = "plan",     agent = "planner",    done_when = "plan.md" },
  { name = "research", agent = "researcher", done_when = "results.jsonl" },
  { name = "write",    agent = "writer",     done_when = "report.md" },
]
```

Globs support `*`, `?`, `**`, `~/` and absolute paths. Agent and skill files are Markdown
with optional frontmatter (`name`, `description`, `model`, `tools` or `allowed-tools`), the
format Claude Code uses. A stage is *done* once its `done_when` file exists; while a run is
live, the stage whose file changed last is shown as *running*.

The graph map's edges come from the files themselves: if `planner.md` mentions `researcher`,
the planner *spawns* the researcher; a mention of a skill *invokes* it; a mention of a prompt
file *reads* it; and a family reference such as `sh-kb-*` or `sh-kb-<class>` *loads* every
skill with that prefix.

## Runners

A runner is how agentdeck starts an agent. Built-in runners are shown when installed:

| Runner | Command | Live view |
|---|---|---|
| `claude` | `claude -p {prompt} --output-format stream-json --verbose` | text, every tool call, subagent spawns and results, permission refusals, cost |
| `codex` | `codex exec {prompt}` | output lines |
| `gemini` | `gemini -p {prompt}` | output lines |

Add or override runners in the config:

```toml
[[runner]]
name    = "claude-edits"
command = "claude"
args    = ["-p", "{prompt}", "--output-format", "stream-json", "--verbose", "--permission-mode", "acceptEdits"]
format  = "claude-stream-json"          # or "text"

[[preset]]                              # saved prompts on the Launch page
name    = "Nightly quick review"
harness = "security-harness"
runner  = "claude"
prompt  = "/sh-security-review {target} depth:quick"
```

The agent runs in the chosen target directory with your user's permissions, and with your
agent CLI's own permission settings (for Claude Code, `.claude/settings.json`); agentdeck
does not loosen them. Stop sends SIGTERM to the runner's whole process group (Windows: the
process tree), then kills it after 5 s. Output is kept in memory for the live view and
written to a log under the data directory (`~/.local/share/agentdeck/logs`, `~/Library/
Application Support/agentdeck/logs`, or `%LOCALAPPDATA%\agentdeck\logs`).

## Security model

This console can start programs and rewrite prompts, so it is built like a remote shell:

- **Loopback only by default.** Binding elsewhere prints a warning.
- **A token on every API call.** It is random per start (or `--token` / `AGENTDECK_TOKEN`,
  16+ characters), and travels as an `HttpOnly`, `SameSite=Strict` cookie or an
  `Authorization: Bearer` header. It is compared in constant time.
- **No DNS rebinding.** On loopback, a request whose `Host` is not `localhost`, `127.0.0.1`
  or `[::1]` is refused, token or not.
- **No CSRF.** State-changing requests must be same-origin (`Origin`, `Sec-Fetch-Site`) and
  JSON.
- **No shell.** A runner's program is executed directly with an argument vector, and the
  prompt is a single argument: `; rm -rf ~` in a prompt is just text. Runners come only from
  the config file, never from a request.
- **Confined files.** Prompts can be read and written only if discovery found them, and a
  save is refused (409) if the file changed since it was loaded. Run files resolve only
  inside their run directory: `..`, absolute paths and symlinks that leave it are refused.
- **No injected markup.** The UI renders every string with `textContent`, under a strict
  `Content-Security-Policy` (`script-src 'self'`, nothing third-party).

## Limits

- Processes are tracked while agentdeck runs; their logs stay on disk after it exits.
- Subagent detail needs a runner that reports it. Claude Code's stream-json does; plain-text
  runners show output lines.
- Run monitoring reads files, so it works for any harness that writes its state to disk,
  whether or not agentdeck started the run.

## Development

```bash
cd tools
cargo test -p agentdeck     # unit tests + API tests (auth, CSRF, traversal, launch, stop)
cargo run -p agentdeck -- --target /path/to/project
```

The UI is plain JavaScript in `ui/`, embedded at build time; edit and rebuild.
