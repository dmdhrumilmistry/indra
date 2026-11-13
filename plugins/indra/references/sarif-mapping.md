# Finding -> SARIF 2.1.0 mapping

The reporter builds `results.sarif` from `verified.jsonl`. Use `sarif-template.json` as the skeleton
(copy `runs[0].tool.driver`, replace `rules` and `results`). Emit one SARIF `result` per finding whose
`status` is `verified` or `needs-runtime` (never emit `false-positive`; optionally emit `candidate` when
the caller asked for an unverified scan).

| Finding field | SARIF location |
|---|---|
| `class` | `ruleId` = `SH-<CLASS-UPPER>`; add/reuse a matching entry in `tool.driver.rules[]` |
| `severity` | `level`: critical/high -> `error`, medium -> `warning`, low/info -> `note` |
| `cvss.score` (or mapped severity) | `properties["security-severity"]` (string, "0.0"-"10.0") - GitHub code-scanning reads this |
| `title` + `why_it_matters` | `message.text` |
| `file` / `line` | `locations[0].physicalLocation.artifactLocation.uri` / `region.startLine` |
| `data_flow[]` | `codeFlows[0].threadFlows[0].locations[]` (one threadFlowLocation per hop) |
| `id` | `partialFingerprints.shFindingId` (stable across runs) |
| `cwe` | `properties.cwe[]` AND `rules[].properties.tags` as `external/cwe/cwe-<n>` |
| `owasp` | `properties.owasp[]` |
| `status`, `confidence` | `properties.shStatus`, `properties.shConfidence` |
| `mitigation` | `rules[].help.text` or `result.properties.shMitigation` |

Rules dedupe by `id`: define each `SH-<CLASS>` rule once in `tool.driver.rules[]`, reference by `ruleId`.
Validate the output against the SARIF 2.1.0 schema before finishing (any SARIF validator / VS Code SARIF viewer).
