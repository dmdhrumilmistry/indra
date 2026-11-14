# Local models with Ollama

indra runs agents on models you host with [Ollama](https://ollama.com), in three ways:

| What runs where | How |
|---|---|
| The whole agent on a local model | a profile with `provider = "ollama"` |
| The main agent on Anthropic, its subagents on a local model | a profile with `subagent_provider = "ollama"` |
| One role or one step on a local model | plan and delegate, or a workflow task, naming such a profile, or a task's own `model` and `provider` |

Claude Code works with all three (Ollama 0.14 and later serves the Anthropic Messages API it
speaks). Codex works with the first (`--oss`). Other runners can be pointed at Ollama with
their own options in a profile's `env`.

## The whole agent on Ollama

```toml
[[profile]]
name        = "local"
runner      = "claude"                # or "codex"
model       = "qwen3-coder"           # a model your Ollama server has (`ollama pull qwen3-coder`)
provider    = "ollama"
# ollama_url = "http://gpu-box:11434" # another machine's Ollama; default: this machine
```

For Claude Code, indra sets the base URL and maps `opus`, `sonnet` and `haiku` (and Claude
Code's own background model and subagent model) to `model`, so agent files that say
`model: opus`, like the indra plugin's, run on the local model too. Anything the profile's `env`
sets itself wins. For Codex, it adds `--oss -m <model>` and points Codex at the server.

## Subagents on Ollama, the main agent on Anthropic

```toml
[[profile]]
name              = "lead-local"
runner            = "claude"
model             = "opus"            # the main agent: plans, decides, reviews
subagent_model    = "qwen3-coder"     # every subagent it spawns
subagent_provider = "ollama"
```

Claude Code sends all of a session's model calls to one base URL, so indra points it at its
**model router**: a small proxy on `127.0.0.1` in the indra process that starts the agent (the
console, or `indra node` on another machine). The router reads each request's model: the
subagent model goes to Ollama, everything else goes to Anthropic (or to the `ANTHROPIC_BASE_URL`
the profile already set, such as a gateway). Responses stream through unchanged.

## What reaches Ollama

Nothing that identifies you. The router drops `authorization` and `x-api-key` before a request
goes to Ollama, and a whole agent on Ollama goes through the router for this reason alone:
Claude Code sends whatever login it has (an API key, or a Claude subscription login) to its base
URL, and that should never reach a model server, least of all one on another machine over plain
HTTP. On the Anthropic leg the router passes the agent's own credentials through and adds none.

Each launch gets its own route behind a random path, so another program on the machine cannot
borrow it by guessing the port. The **Settings > Profiles** page, Launch and `GET /api/ollama`
show how many requests went each way.

## Picking a model

Model fields in the console suggest the models your Ollama server has (from its
`/api/tags`), next to Claude's aliases. On **Launch**, **Runs on** overrides the profile for one
launch: the whole agent on Ollama, or Anthropic with its subagents on Ollama (and the subagents'
model).

## In workflows and teams

A workflow task can name a profile and override its model and provider:

```toml
[[workflow.task]]
id       = "triage"
profile  = "local"
model    = "llama3.3"                 # this step only
provider = "ollama"
prompt   = "Sort the findings in {target}/.security-harness by file"
```

In plan and delegate, each role is a profile, so a planner on Opus and a `local` worker give
"Claude plans, a local model builds" across processes, and a `lead-local` worker gives it inside
each step as well.

## Notes

- A small local model may not follow the security skills' longer instructions as well as Opus
  or Sonnet. Hand it narrow, well-specified steps (the worker role) and keep the planner and
  reviewer on a strong model.
- Nodes need an indra built from the same version as the hub to understand these fields.
