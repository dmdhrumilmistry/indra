# The board and trackers

The **Board** page holds work for agents in five columns: **To do**, **Running**, **Review**,
**Done** and **Failed**. An item is generic (a title, details, and how to run it), so the board
works for security reviews, fixes, research or anything else an agent can do.

```text
 To do --Run--> Running --agent finishes--> Review --you accept--> Done
                       \--agent fails-----> Failed
```

- **Run** starts an agent on the item with a [profile](../../tools/indra/README.md#profiles)
  (model, permission mode, tools), on a node and in a target directory. The item's title and
  details become the agent's prompt.
- When the agent finishes, the item moves to **Review** with the agent's result attached, or to
  **Failed**. Moving an item to **Done** is always a person's decision: drag it there.
- Items can be dragged between any columns except Running, which only Run can enter.
- The agent's process page has everything else: its live feed, its tool requests waiting for
  approval, and controls to change its mode or model.
- **Edit** changes an item's title, details, labels, profile and target (an item from a tracker
  keeps its link; the tracker itself is not edited). **Remove from board** deletes it here only.
- **Export** downloads the board's items as JSON; **Import file** adds items from such a file
  (a list of objects with at least a `title`), for example to move work between consoles.

## Connect a tracker

**Settings > Connectors** adds Jira, GitHub Issues, ClickUp or Linear. A connector imports matching items
into To do, and reports each move back to the tracker: a comment when an agent starts, a comment
with the agent's result when it finishes, and the tracker state you map to each column.

1. Create an API token in the tracker (below) and add it in **Settings > Secrets**, scoped to the
   connector as `connector:<name>`. The connector refers to it as `secret:<name>`; a token written
   into `indra.toml` itself is refused.
2. Add the connector, map the columns to states, and pick the profile, node and target its items
   run with.
3. Press **Import from <name>** on the Board, or set **Import every N minutes**. With **Start an
   agent on each new item**, imported items run on their own, as many at a time as the node allows.

```toml
[[connector]]
name    = "jira"
kind    = "jira"
url     = "https://acme.atlassian.net"
email   = "you@acme.com"
project = "SEC"
query   = "labels = agent"              # extra JQL
token   = "secret:jira"
profile = "worker"
target  = "~/code/api"
states  = { running = "In Progress", review = "In Review", done = "Done" }

[[connector]]
name    = "issues"
kind    = "github"
project = "acme/api"                     # owner/repo
query   = "agent"                        # a label
token   = "secret:github"
states  = { running = "in-progress", review = "needs-review", done = "closed" }

[[connector]]
name         = "clickup"
kind         = "clickup"
project      = "901234567"               # list id
query        = "to do, ready"            # statuses to import
token        = "secret:clickup"
poll_minutes = 10
auto_run     = true
states       = { running = "in progress", review = "review", done = "complete" }
```

| Tracker | Token | States |
|---|---|---|
| Jira Cloud | an API token from your Atlassian account, with the account's email | workflow status names; indra picks the transition that leads there |
| GitHub Issues | a fine-grained token with Issues: read and write on the repository | a label, or `open` / `closed` |
| ClickUp | a personal API token (`pk_...`) | status names of the list |
| Linear | a personal API key (`lin_api_...`); project is the team key (ENG), the filter a label | workflow state names of the team (Todo, In Review, Done) |

Requests go through the system `curl` with the token passed on its stdin, never on the command
line. indra only lists open items, comments, and changes state (on GitHub, by adding the label
of the new state and removing the labels of the other mapped states). It never deletes an item,
and closes one only through a state you map, such as `done = "closed"`.

## Be careful with imported text

An imported item's title and description become the agent's instructions, and anyone who can
edit the ticket writes them. Run tracker work with a profile whose permission mode and tool rules
match how much you trust that tracker: `plan` or `manual` to review what it would do, a narrow
`allowed_tools` list for unattended runs, and secrets scoped to that profile only.
