# Blue team: SIEM sources, detection checks, rules, triage and hardening

indra connects to your SIEM **read-only** and gives its agents three defensive skills.

| Piece | What it does |
|---|---|
| **SIEM sources** (`[[siem]]`) | Wazuh, Elastic, OpenSearch, Splunk, or plain log files. Searched by `indra siem`, by the `indra siem mcp` tools, and by detection checks |
| **Check detections** on an engagement | Which of the engagement's steps your SIEM alerted on: detected, missed, and the ATT&CK techniques its rules named |
| **`sh-detection-rules`** | Drafts Sigma rules for what was missed, with the log source each needs, noise counted against your real logs, and a test for each. Never deploys them |
| **`sh-log-triage`** | Triages alerts and hunts in logs: incidents, timelines, indicators, verdicts with evidence. Takes no response action |
| **`sh-hardening`** | Turns findings, a threat model and detection gaps into an ordered plan of fixes, each with how to verify it |

## Add a source

**Settings > SIEM > Add SIEM source**, or in `indra.toml`:

```toml
[[siem]]
name = "wazuh"
kind = "wazuh"                         # wazuh | elastic | opensearch | splunk | files
url = "https://wazuh-indexer:9200"     # the Wazuh indexer (OpenSearch), not the manager API on 55000
username = "indra-ro"
password = "secret:wazuh-ro"           # indra secret set wazuh-ro  (scope it to siem:wazuh)
ca_file = "/etc/indra/wazuh-root-ca.pem"
# index = "wazuh-alerts-*"             # the default for Wazuh
# time_field = "timestamp"             # the default for Wazuh

[[siem]]
name = "elastic"
kind = "elastic"
url = "https://es.internal:9200"
api_key = "secret:elastic-ro"
index = ".alerts-security.alerts-*,logs-*"

[[siem]]
name = "splunk"
kind = "splunk"
url = "https://splunk.internal:8089"   # the management port
token = "secret:splunk-ro"

[[siem]]
name = "logs"
kind = "files"
paths = ["/var/log/auth.log", "/var/log/nginx/*.log"]
```

**Credentials** are always secret references, never values in the file. A secret scoped to
`siem:<name>` is given to that source only. Use an account that can only read.

**Certificates:** for a SIEM with its own certificate authority, Wazuh's default among them, point
`ca_file` at that authority's certificate. `insecure_tls = true` skips the check, but prefer `ca_file`.

Check a source with **Test**, or `indra siem test wazuh`.

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

## Give agents the SIEM

```bash
claude mcp add indra-siem -- indra siem mcp
```

This adds three read-only tools: `siem_sources`, `siem_search` and `siem_alerts`. Agents can also run
`indra siem search` in Bash.

Other security tools with their own MCP servers (an official Wazuh, Elastic or Splunk MCP server, a
threat-intel lookup, an EDR's) work the same way: add them to Claude Code, or install them from
**Marketplace**, and agents started by indra have them.

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
| `GET /api/siem` | The configured sources, without credentials |
| `POST /api/siem/{name}/test` | Can it be reached with its credentials |
| `POST /api/engagements/{id}/detections` `{sources?, slack?}` (operators) | Run the detection check |
