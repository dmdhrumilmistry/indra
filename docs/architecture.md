# Architecture

How indra runs and controls agents across machines, and how it is meant to grow: agent control,
profiles and model routing, secrets, transport, a task board fed by trackers, and graph
workflows. Each part says what exists now and what comes next.

## The pieces

```text
                       browser (console UI)
                              |  HTTPS or loopback, token + same-origin
                    +---------v----------+
                    |        hub         |  indra serve
                    |  config  profiles  |
                    |  secrets  board    |------> trackers (Jira, ClickUp, GitHub, Linear)
                    |  workflows engine  |
                    +---+------------+---+
          signaling (WebSocket)       \  SSH (last resort)
              |                        \
     +--------v--------+         +------v---------+
     |  node: indra    |  WebRTC |  node: indra   |
     |  node connect   |<------->|  node exec     |
     |  runners/agents |  data   |  (over SSH)    |
     +-----------------+ channel +----------------+
```

- **Hub** (`indra serve`): the console, the config, the workflow engine and, as they land, the
  secret store and the board. It is the only part a person talks to.
- **Node**: any machine that runs agents. `local` is the hub's own machine. A node runs only
  runners from *its own* configuration; the hub names a runner, it never ships a command line.
- **Runner**: how one agent CLI is started (Claude Code, Codex, Gemini CLI, or anything that
  takes a prompt). **Profile**: a runner plus model, permission mode, effort, tool rules and
  environment, under one name.

## Agent control (built)

An agent started through a `claude-stream-json` runner is driven over Claude Code's control
channel (`--input-format stream-json`, the same channel the Agent SDK uses), so while it runs the
console can:

| Action | How |
|---|---|
| Approve or deny a tool request | Claude Code asks the host (`can_use_tool`); the console shows the request with **Allow**, **Allow always** (adds Claude Code's suggested rule for the session) and **Deny** |
| Change the permission mode | `set_permission_mode`: `manual`, `acceptEdits`, `auto`, `plan`, `dontAsk`, `bypassPermissions` |
| Change the model | `set_model`, for the turns that follow |
| Interrupt | `interrupt`: stops the current turn, keeps the session |
| Send a follow-up | another user message in the same session |

The agent's input stays open while it has work: indra closes it after the last turn's result,
so the agent exits as before. Remote agents get the same controls: the hub forwards control
messages to the node, which writes them to the agent. Runners that are not Claude Code keep the
simpler model (launch, watch, stop, pivot).

## Profiles and model routing (built)

A profile is the unit of delegation: "plan with a strong model, do the work with the model I
choose, try small jobs on a local model".

```toml
[[profile]]
name            = "planner"
runner          = "claude"
model           = "opus"
permission_mode = "plan"

[[profile]]
name            = "worker"
runner          = "claude"
model           = "sonnet"
permission_mode = "acceptEdits"
allowed_tools   = ["Read", "Grep", "Glob", "Edit", "Bash(git diff:*)"]

[[profile]]
name     = "local"                     # Claude Code against a local Ollama model
runner   = "claude"
model    = "qwen3-coder"
provider = "ollama"
```

Launch, presets, workflow tasks and board items name a profile; the model and mode can still be
overridden per launch and changed live. Ollama serves the Anthropic Messages API (v0.14 and
later), so Claude Code runs unchanged on a local model. `subagent_provider = "ollama"` keeps
the main agent on Anthropic and sends its subagents to Ollama through a model router on
loopback that routes each request by its model and keeps credentials off Ollama (see
`docs/guides/local-models.md`).

## Secrets (built)

Agents need tokens (tracker APIs, cloud credentials, model keys). The rules:

1. **One store, encrypted at rest.** Secrets live in the hub's data directory, encrypted
   (XChaCha20-Poly1305) with a key from `INDRA_SECRETS_KEY` or a key file with `0600`
   permissions. An OS keychain backend is a possible later addition.
   `indra.toml` never holds a secret value, only a reference: `env = { JIRA_TOKEN = "secret:jira" }`.
2. **Write-only from the console.** The Secrets page can set, rotate and delete a secret and
   shows its name, scope and last use, never its value.
3. **Scoped.** A secret lists the profiles and nodes that may receive it. A launch that references
   a secret outside its scope fails before anything starts.
4. **Delivered just in time, in memory.** Values are resolved when an agent starts and passed in
   its environment. To a remote node they travel inside the job message over the encrypted
   transport; the node never writes them to disk.
5. **Never echoed.** Every event, log line and API response passes through a redactor that
   replaces known secret values with `[secret:name]`.
6. **Audited.** Each delivery is recorded (secret, profile, node, process, time).

## Transport: WebRTC first, SSH as the last resort (built)

Today the hub reaches nodes over SSH. The next transport inverts the direction, so nodes need no
open port and no SSH account:

1. **Enrol.** On the hub, **Nodes > Add a machine** creates a one-time enrolment code (15
   minutes). On the node: `indra node connect https://hub.example --name box --code ABCDE-FGHJK`.
   The hub answers with a random 256-bit key for that node, kept in its encrypted secret store
   and in an owner-only file on the node; from then on the node presents that key.
2. **Signal.** The node keeps one outbound WebSocket to the hub (`/agent/connect`). It carries
   presence, heartbeats and WebRTC offers, answers and ICE candidates.
3. **Connect.** Hub and node open a WebRTC peer connection (STUN for NAT traversal, optional
   TURN) with one reliable, ordered data channel; job frames carry their job id. DTLS encrypts
   it end to end. Frames over 60 KB take the WebSocket.
4. **Fall back.** If ICE fails, jobs flow over the signaling WebSocket instead (relayed by the
   hub). If the node has no agent connection at all and has an `ssh` destination configured,
   the hub uses SSH as it does today.
5. **Reconnect.** Both sides detect loss with heartbeats (5 s interval, 15 s timeout), reconnect
   the WebSocket with exponential backoff and jitter (1 s up to 60 s), and restart ICE without
   dropping the session. A job survives a reconnect: events carry sequence numbers, the node
   buffers the last events of each job, and the hub resumes from the last one it saw. A node that
   stays away past a grace period (default 2 minutes) has its jobs stopped, exactly as a closed
   SSH session stops them today.

**Scale.** Nodes dial in, so the hub needs no route to them; a hub handles thousands of idle
nodes (one WebSocket each, a heartbeat every few seconds) and the job traffic goes peer to peer.
Sharing one fleet between several hubs (behind a signaling relay) is a possible later step;
the workflow engine already schedules by per-node capacity.

## Tasks and the board (built)

A **board** shows work items in columns: **To do**, **Running**, **Review**, **Done**,
**Failed**. It is generic: an item is a title, a body, labels, an optional source, and a profile
to run it with, so it serves reviews, fixes, research or anything else an agent can do.

```text
 tracker (Jira / ClickUp / GitHub / Linear)        indra board
 ------------------------------------------        ------------
 issue in "To Do" matching the filter  --import-->  To do
                                                     | Run (profile, node, target)
 move to "In Progress"                  <--sync---   Running   (agent process)
 comment with the result, move to "Review" <-sync--   Review    (agent finished)
 move to "Done" when you accept it      <--sync---   Done
```

- **Connectors** are per tracker (Jira, GitHub Issues, ClickUp, Linear): how to list items
  with a filter, comment, and move one between states. The token is a secret reference, scoped
  to the connector. A connector maps board columns to tracker states. Requests go through the
  system `curl`, credentials on its stdin.
- **Flow.** Import (manual or every few minutes), run with a profile on a node, and when the
  agent finishes, post its result as a comment and move the item to the mapped state.
  A person moves items to Done; agents never close tickets on their own.
- Items can also be created on the board directly.

## Workflows as graphs (built)

Workflows are already DAGs of tasks with dependencies. The editor makes them visual:

- Drag tasks onto a canvas, connect them to say what runs after what; each node picks a profile,
  a machine and a target.
- **Components** are saved task templates (a prompt with parameters and a profile), reused by
  dragging them in.
- A **workflow can be a node** in another workflow, so pipelines compose. It is inlined when the
  config loads (`<step>__<task>`), so the engine runs one flat graph and capacity, retries and
  cancel work as for any task.
- The graph is stored as ordinary `[[workflow]]` TOML (with positions), so `indra run` and the
  text editor keep working.

## Plan and delegate (built)

Delegation is orchestration from the outside, so roles can mix runners and providers:

1. The hub starts the **planner** (a profile) with the task and a fixed request for a JSON plan:
   a summary and steps, each with self-contained instructions, dependencies and a size
   (`standard` or `light`).
2. When the planner exits, the hub extracts the plan from its result (any runner: the last JSON
   object with `steps`), normalises ids and dependencies, caps the step count, and holds it for
   review unless the team runs it straight away.
3. The approved plan becomes an ad-hoc workflow on the existing engine: one task per step on the
   worker (or light worker) profile, chained in plan order unless the team allows parallel steps,
   then a review task on the reviewer profile. Capacity, cancel, skips after a failure and
   per-step process pages come from the engine.
4. The delegation reports each step's state and the cost per role from the agents' results.

## Knowledge and context (built)

Agents share what they learn through the same memory vaults reviews use (`indra mem`):

- **In:** a launch with `memory` (per launch, or from its profile, so workflow tasks and board
  items inherit it) recalls the notes most relevant to the prompt from the harness's vaults on
  the hub and appends them to the prompt, so local and remote agents get the same context. The
  process records which notes it was given.
- **Out:** with `reflect`, when an interactive agent's last turn ends, the hub sends one more
  user message asking for learnings as JSON before it closes the agent's input. The answer is
  parsed into proposals in an inbox (`learnings.json`), with the process, session, profile and
  directory they came from. A person accepts a proposal into a vault (a `learning` note, merged
  into a note of the same title) or discards it; nothing an agent says becomes memory unreviewed.
- **Context size:** the hub tracks the tokens of the agent's last top-level turn and the model's
  context window from Claude Code's stream, and shows them on the process page and in
  `indra attach`.

## Terminals (built)

Two terminal surfaces sit on the same API as the console, so they add views, not powers:

- **`indra attach` / `indra agents`** run in the user's own terminal. They call the console's API
  with its token (found through an owner-only `console.json` in the data directory on the same
  machine, or `--url` / `--token`) and follow an agent over the same Server-Sent Events stream as
  the process page. Commands map one to one onto the permission, control, message, pivot and
  stop endpoints, so remote agents work exactly like local ones.
- **The browser terminal** (`indra serve --shell`) is a PTY on the hub bridged over a WebSocket
  (`/api/term`): keystrokes and output as binary frames, resizes as small JSON frames. It starts
  the user's shell or Claude Code in a target, or resumes an agent's session (forked while the
  agent runs). Because it is a shell, it is opt-in, loopback-only, never read-only, same-origin
  checked, capped, and logged; it runs on the hub only, as remote sessions live on their nodes.

## Order of work

1. Agent control and profiles (including Ollama), and this document. Done.
2. Secrets. Done.
3. Board and tracker connectors (Jira, GitHub Issues, ClickUp). Done.
4. Graph workflow editor with components and sub-workflows. Done.
5. WebRTC transport with reconnection, SSH kept as the fallback. Done.
