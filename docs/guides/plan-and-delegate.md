# Plan and delegate

Let a strong model do the thinking and cheaper models do the typing. In **Plan and delegate**,
one agent (the **planner**) reads the code and writes a plan of small, self-contained steps;
each step then runs as its own agent on a cheaper **worker** model; an optional **reviewer**
checks the result. You can read and edit the plan before anything changes.

```text
 task --> planner (Opus, Fable, a strong Codex model)       reads, plans, changes nothing
            |
            v  plan: steps with instructions, order, size
          you review: edit, reorder, drop, add            (or let it run straight away)
            |
            v
          step 1 --> step 2 --> step 3 ...                 worker: Haiku, a local model (Ollama)
                                \--> light steps            light worker: the cheapest model
            |
            v
          reviewer                                          checks against the plan, fixes small things
```

Every role is a [profile](../../tools/indra/README.md#profiles), so each can use any runner
and model: Claude Code on Opus to plan and on Haiku to build, a Codex planner with a local
Ollama builder, or anything else. indra drives the roles from the outside, so they can mix
providers, which one agent's own subagents cannot.

## Set up a team

A **team** names the profiles for each role. In **Settings > Profiles**, add the templates
**Planner (Opus, read-only)** and **Builder (Haiku)** (and **Local model (Ollama)** if you have
one); then in **Settings > Teams**, add **Opus plans, Haiku builds** or **Opus plans, local
model builds**. Or write it in `indra.toml`:

```toml
[[profile]]
name            = "planner"
runner          = "claude"
model           = "opus"
permission_mode = "dontAsk"                 # never stops to ask; only reads
allowed_tools   = ["Read", "Grep", "Glob", "Bash(git log:*)", "Bash(git diff:*)"]

[[profile]]
name            = "builder"
runner          = "claude"
model           = "haiku"
permission_mode = "acceptEdits"

[[profile]]
name   = "local"                            # Claude Code against a self-hosted model
runner = "claude"
model  = "qwen3-coder"
env    = { ANTHROPIC_BASE_URL = "http://localhost:11434", ANTHROPIC_AUTH_TOKEN = "ollama", ANTHROPIC_API_KEY = "" }

[[team]]
name         = "opus-haiku"
description  = "Opus plans, Haiku builds, Opus reviews"
planner      = "planner"
worker       = "builder"
light_worker = "local"                      # mechanical steps on the cheapest model (optional)
reviewer     = "planner"                    # optional: no reviewer, no review step
# auto_run   = true                         # run the plan without waiting for review
# parallel   = true                         # let independent steps run at the same time
# max_steps  = 8
```

A Codex or Gemini CLI profile works the same way (`runner = "codex"`, with the runner's
`model_args` to pick a model).

## Run one

**Launch > Plan and delegate**: describe the task, pick the node and directory, and a team (or
pick the four roles there). **Plan it** starts the planner; its page shows it reading the code.

When the plan is ready (and unless the team runs it straight away) the plan's page shows the
planner's approach and every step: its title, the instructions the worker will get, who does it
(the worker, or the light worker for mechanical steps), and what it waits for. Edit any of them,
move steps up or down, remove or add steps, then **Run the plan**.

The steps run as a workflow: one after another by default, because they share a directory (tick
**Independent steps may run at the same time** to let steps without dependencies overlap). Each
step is an ordinary agent with its own process page, approvals and controls; a failed step skips
the ones after it. Each worker gets a short brief: the goal, the approach, the list of steps with
its own marked, and its instructions, so a small model has what it needs without the planner's
whole conversation. The reviewer then looks at what changed (for example `git diff`), runs the
checks the plan names, fixes small mistakes and reports.

The plan's page shows each step's state and process, and what each role cost; **Processes**
lists all plans. **Cancel** stops the planner or the remaining steps.

## Why it saves tokens

- The expensive model reads and plans once and never edits; edits, test runs and retries happen
  on the worker's model.
- Each worker starts fresh with a brief of a few hundred words instead of a long conversation.
- Mechanical steps can go to the cheapest model you have, including a local one with no API cost.

The costs per role on the plan's page (for Claude Code runners) show the split.

## Inside one Claude Code agent

Claude Code's own subagents (its Task tool) can use a cheaper model too. Set **Model for its
subagents** on a profile (`subagent_model = "haiku"`); indra passes it as
`CLAUDE_CODE_SUBAGENT_MODEL`, so an Opus agent hands its subagents to Haiku. That works within
one provider; Plan and delegate works across them.

## API

| Call | What it does |
|---|---|
| `POST /api/delegations` `{h, node, target, prompt, team}` | plan a task with a team (or `roles: {planner, worker, light_worker, reviewer, parallel, auto_run, max_steps}` instead of `team`); `auto_run` overrides the team's choice |
| `GET /api/delegations`, `GET /api/delegations/{id}` | plans, with the planner's process, each step's state and process, and the cost per role |
| `POST /api/delegations/{id}/approve` `{plan}` | run the plan, as written or edited (`{summary, steps: [{id, title, prompt, needs, size}]}`) |
| `POST /api/delegations/{id}/cancel` | stop it |
