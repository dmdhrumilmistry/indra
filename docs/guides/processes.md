# Handling agent processes

Every agent that indra starts, from the console's **Launch** page, a workflow, a fan-out or a
pivot, is a *process*: one run of one runner (`claude`, `codex`, `gemini` or your own) in one
target directory on one node. This guide covers starting, watching, stopping and cleaning up
after them.

## Start one

In the console (`indra serve --target <dir>`), open **Launch**, pick the node, target, runner
and prompt, and press Launch. The process page opens and streams the agent live.

From a script, the same thing over the API (the token is the one `indra serve` printed):

```bash
curl -s -H "Authorization: Bearer $INDRA_TOKEN" -H 'Content-Type: application/json' \
  -d '{"node":"local","runner":"claude","target":0,"prompt":"/indra review {target} depth:quick"}' \
  http://127.0.0.1:7878/api/procs
# {"id":3}
```

`target` is an index into the node's targets (`GET /api/nodes` lists them). For many agents at
once, use a [workflow or fan-out](../../tools/indra/README.md#workflows) instead of a loop: they
respect each node's capacity.

## Watch it

| Where | What you get |
|---|---|
| **Processes** page | every process with its node, runner, status, cost and last line |
| a process's page | the live stream: text, each tool call, each subagent with its own tool calls, refused permissions, and the final cost (Claude Code); plain output lines for other runners |
| `GET /api/procs`, `GET /api/procs/{id}` | the same as JSON |
| `GET /api/procs/{id}/events?after=N` | a Server-Sent Events stream of every event after sequence `N`, so a client can reconnect without missing or repeating anything |
| the log file | the raw output, kept after the console exits (below) |

A process is in one of four states:

| Status | Meaning |
|---|---|
| `running` | still going |
| `exited` | ended with exit code 0 |
| `failed` | ended with a non-zero exit code, or could not start (missing program, bad target) |
| `stopped` | you stopped it (or a workflow was cancelled) |

`exit_code` holds the runner's own code. For a remote process it is the code of the agent on
the node; an SSH failure (unreachable host, unknown host key) shows up as `failed` with ssh's
message in the stream.

## Stop it

Press **Stop** on the process page, or `POST /api/procs/{id}/stop`.

- **Local:** indra sends SIGTERM to the runner's whole process group (on Windows it ends the
  process tree), waits up to 5 seconds for all of it to exit, then sends SIGKILL to whatever is
  left, including children that ignore SIGTERM. Subagents, MCP servers and the tools they
  started go with it.
- **Remote:** the hub closes the job's stdin; the node sees that and terminates the agent's
  process tree on its side. A dropped SSH connection has the same effect, so a remote agent
  does not outlive the hub that started it.
- **Workflows:** cancelling (the Workflows page, or Ctrl-C on `indra run`) stops every running
  task's process and cancels the tasks that have not started.

Stopping is final. To change direction without losing the agent's work, *pivot* it instead:
see [pivoting](pivoting.md).

## Logs

Each process writes its full output to `logs/` in the indra data directory:

| OS | Directory |
|---|---|
| Linux | `~/.local/share/indra/logs/` (or `$XDG_DATA_HOME/indra/logs/`) |
| macOS | `~/Library/Application Support/indra/logs/` |
| Windows | `%LOCALAPPDATA%\indra\logs\` |

Files are named `<start time>-<id>-<node>-<runner>.log`. The process page and `indra run`'s
summary show the exact path. For Claude Code the log is the stream-json, one event per line,
so `jq` works on it:

```bash
jq -r 'select(.type=="result") | "\(.subtype) $\(.total_cost_usd)"' ~/.local/share/indra/logs/*-3-local-claude.log
```

indra does not rotate or delete logs; remove old ones yourself when you no longer need them.

## Capacity

`max_parallel` (under `[local]`, default 4, and on each `[[node]]`, default 2) is how many agents a node
runs at once *for workflows and fan-out*: a task waits until its node has room, counting every
agent there, including ones you launched by hand. A manual Launch is never queued: it starts
right away, so you can always run one more on purpose. The Nodes page shows `running / max`
for each node.

## Permissions

An agent runs with your user's permissions in its target directory, and with its CLI's own
permission settings (for Claude Code, the target's `.claude/settings.json` and your user
settings). indra does not add flags that loosen them. If you see "permission denied" events in
the stream, the agent asked for a tool it is not allowed to use: either allow it in the agent's
settings, or use a runner with a permission mode, for example:

```toml
[[runner]]
name    = "claude-edits"
command = "claude"
args    = ["-p", "{prompt}", "--output-format", "stream-json", "--verbose", "--permission-mode", "acceptEdits"]
format  = "claude-stream-json"
```

On a remote node the node's own `indra` config decides which runners exist; the hub only names
one.

## When the console exits

Processes are tracked while `indra serve` (or `indra run`) is running. When the console gets
Ctrl-C, SIGTERM or SIGHUP (a closed terminal, a service manager), it cancels running workflows
and stops every agent it started, as Stop would, before it exits; that can take a few seconds,
and it prints how many it is stopping. Logs stay on disk. `indra run` does the same on Ctrl-C.

So an agent never outlives the console that started it. To keep agents running unattended,
run the workflow with `indra run <name>` from cron or CI, or keep `indra serve` running under
a service manager (systemd, launchd, tmux), rather than leaving a terminal open.

## Troubleshooting

| Symptom | Likely cause and fix |
|---|---|
| `failed` immediately, "No such file or directory" | the runner's `command` is not on `PATH` for the user running indra; set an absolute path in `[[runner]]` |
| a remote process `failed` with "Host key verification failed" | indra uses `BatchMode=yes` and never accepts an unknown key: `ssh <host>` once by hand to trust it |
| "Permission denied (publickey)" | the hub's user needs key or agent access to the node; check with `ssh -o BatchMode=yes <dest> true` |
| "unknown runner" on a node | the runner is not in *that node's* config: `indra nodes` lists what each node offers |
| no tool calls or cost in the live view | the runner's `format` is `text`; use `format = "claude-stream-json"` with Claude Code's `--output-format stream-json --verbose` |
| a task waits forever in a workflow | its node is at `max_parallel`; stop something there or raise the limit |
| Stop seems to do nothing | something in the tree ignored SIGTERM; the whole group is killed 5 s later. A remote agent is ended by its node once its SSH session closes |
