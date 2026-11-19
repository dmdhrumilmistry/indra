# Blue team: MCP & tools, detection checks, rules, triage and hardening

**MCP & tools** gives indra's agents any third-party tool you need. It also connects to your SIEM
**read-only**, and indra adds three defensive skills.

| Piece | What it does |
|---|---|
| **MCP & tools** (`[[tool]]`) | Any MCP server: EDR, threat intel, ticketing, scanners, a vendor's own SIEM server. Also read-only data sources: Wazuh, Elastic, OpenSearch, Splunk, log files. Managed in **Settings > MCP & tools** |
| **Check detections** on an engagement | Which of the engagement's steps your SIEM alerted on: detected, missed, and the ATT&CK techniques its rules named |
| **`sh-detection-rules`** | Drafts Sigma rules for what was missed, with the log source each needs, noise counted against your real logs, and a test for each. Never deploys them |
| **`sh-log-triage`** | Triages alerts and hunts in logs: incidents, timelines, indicators, verdicts with evidence. Takes no response action |
| **`sh-hardening`** | Turns findings, a threat model and detection gaps into an ordered plan of fixes, each with how to verify it |

## MCP & tools

**Settings > MCP & tools** (admins) lists every tool. Start from a template, or **Add tool**. **Test**
checks that a tool works:
- a data source runs a search;
- an MCP server is started (or called at its URL) and asked for its tools.

The same list in `indra.toml`:

```toml
# Any MCP server started on the agent's machine. The vendor's docs give the command and variables.
[[tool]]
name = "intel"
kind = "mcp"
category = "threat-intel"              # siem | edr | threat-intel | ticketing | scanner | cloud | identity | other
command = "<the server's command>"
args = ["<its>", "<arguments>"]
env = { API_KEY = "secret:intel-key" } # indra secret set intel-key  (scope it to tool:intel)
profiles = ["blue"]                    # agents of these profiles get it; "*" for every Claude Code agent

# An MCP server reached over HTTP.
[[tool]]
name = "edr"
kind = "mcp"
category = "edr"
url = "https://edr.example.com/mcp"
headers = { Authorization = "secret:edr-token" }   # the whole value, "Bearer ..." included

# Read-only data sources.
[[tool]]
name = "wazuh"
kind = "wazuh"                         # wazuh | elastic | opensearch | splunk | files
url = "https://wazuh-indexer:9200"     # the Wazuh indexer (OpenSearch), not the manager API on 55000
username = "indra-ro"
password = "secret:wazuh-ro"
ca_file = "/etc/indra/wazuh-root-ca.pem"
profiles = ["blue"]

[[tool]]
name = "elastic"
kind = "elastic"
url = "https://es.internal:9200"
api_key = "secret:elastic-ro"
index = ".alerts-security.alerts-*,logs-*"

[[tool]]
name = "splunk"
kind = "splunk"
url = "https://splunk.internal:8089"   # the management port
token = "secret:splunk-ro"

[[tool]]
name = "logs"
kind = "files"
paths = ["/var/log/auth.log", "/var/log/nginx/*.log"]
```

**How an agent gets its tools:**
- **By profile:** every tool whose `profiles` names the agent's profile, or `*`.
- **Per launch:** the tools ticked under **Run an agent > More options > Tools**.
- **MCP servers** are written to a config file for the agent:
  - readable by its owner only, in indra's data folder;
  - passed with `--mcp-config`, so their secrets are resolved at launch and never appear on a command
    line;
  - deleted when the agent exits.

  With **MCP servers: none**, the agent gets only these tools.
- **Data sources** reach agents through indra's own MCP server, with read-only `siem_sources`,
  `siem_search` and `siem_alerts` tools. This works for agents on the console's machine, where the
  sources are configured.
- **Only the console's configuration** sets an agent's MCP servers. A launch request can only pick tools
  from the list, by name.

**Secrets:**
- Every key, token and password is a secret reference (`secret:<name>`), never a value in the file.
- A secret scoped to `tool:<name>` is given to that tool only.
- For a data source, use an account that can only read.

**Certificates:** for a data source with its own certificate authority, Wazuh's default among them,
point `ca_file` at that authority's certificate. `insecure_tls = true` skips the check, but prefer
`ca_file`.

## Read-only by construction

| Source | What indra sends |
|---|---|
| Wazuh, Elastic, OpenSearch | Only `POST <index>/_search` |
| Splunk | Only `search "term" "term" \| head N`: every word is quoted, so a pipe or a command in a query is just text to look for |
| Log files | Only the configured paths, read and never written |

Requests go through `curl`, with the credentials on its standard input, never on a command line.
Results are labelled as untrusted data for the agents that read them.

## Search from the terminal

```bash
indra siem list
indra siem search wazuh 'rule.level:>=10 AND agent.name:web-01' --since 24h
indra siem search splunk '10.20.0.7 "failed password"' --since 7d --json
```

Times are Unix seconds or `15m`, `24h`, `7d` ago. Wazuh, Elastic and OpenSearch take Lucene query
syntax. Splunk and log files take terms and "quoted phrases", all of which must match.

## Outside indra

To give the data sources to a Claude Code you run yourself:

```bash
claude mcp add indra-siem -- indra siem mcp
```

## Check detections (purple team)

After an engagement's agents have run, open the engagement and click **Check detections**:

1. indra takes the engagement's steps: the tool calls the guard allowed that named a host.
2. It asks every SIEM source for its events over that time.
3. A step counts as **detected** when an event from a minute before it to 15 minutes after it names one
   of its hosts.

The result shows:
- detected and missed steps, with the alerts that matched;
- the ATT&CK techniques those alerts named;
- each source's status.

**Export CSV** downloads it. It is also saved as `engagements/<id>/detections.json` and recorded in the
engagement's audit log.

**Draft rules for the missed** starts an agent with `sh-detection-rules` and the missed steps. It writes
draft Sigma rules to `detections/<id>/` for a defender to review and deploy.

## Triage and hardening

Ask in plain words, or start them from **Run an agent**:

```
triage the high Wazuh alerts from the last 24 hours on web-01
hunt for password spraying against the VPN this week
make a hardening plan from the last review and its threat model
```

## API

| Call | What it does |
|---|---|
| `GET /api/integrations` | MCP & tools, with no secret values (only which keys are set) |
| `POST /api/integrations/{name}/test` (admins) | Search a data source, or start an MCP server and list its tools |
| `POST /api/procs` with `"tools": [names]` | Start an agent with these tools on top of its profile's |
| `POST /api/engagements/{id}/detections` `{sources?, slack?}` (operators) | Run the detection check |
