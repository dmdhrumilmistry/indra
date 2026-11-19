//! Configuration: which harnesses to show, which runners may launch agents,
//! and saved prompt presets.
//!
//! A harness is described by globs, so any layout works: the security
//! harness, a plain Claude Code project (`.claude/agents`, `.claude/skills`),
//! or a custom harness with its own folders. Two profiles are built in and
//! detected automatically; a TOML file adds or overrides them.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    pub server: ServerCfg,
    /// Also load the auto-detected built-in profiles (default true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builtins: Option<bool>,
    pub harness: Vec<HarnessCfg>,
    pub runner: Vec<RunnerCfg>,
    pub preset: Vec<PresetCfg>,
    pub profile: Vec<ProfileCfg>,
    pub connector: Vec<ConnectorCfg>,
    /// Reusable workflow tasks.
    pub component: Vec<ComponentCfg>,
    /// This machine as an agent node.
    pub local: LocalCfg,
    /// Remote machines reached over SSH.
    pub node: Vec<NodeCfg>,
    /// Multi-step, multi-node agent workflows.
    pub workflow: Vec<WorkflowCfg>,
    /// Plan with one profile, do the steps with cheaper ones.
    pub team: Vec<TeamCfg>,
    /// Where plugins and skills come from (Claude Code plugin marketplaces).
    pub marketplace: Vec<MarketplaceCfg>,
    /// The browser terminal.
    pub terminal: TerminalCfg,
    /// Credentials agents run with, instead of each machine's own login.
    pub account: Vec<crate::deck::accounts::AccountCfg>,
    /// How a launch picks among the accounts.
    pub routing: crate::deck::accounts::RoutingCfg,
    /// SIEM and log sources for blue-team work (Wazuh, Elastic, OpenSearch,
    /// Splunk, log files), read-only.
    pub siem: Vec<crate::deck::siem::SiemCfg>,
    /// The hub `indra join` joins when given no hub and code (an
    /// organization's defaults.toml or managed.toml, deployed by MDM).
    #[serde(skip_serializing_if = "JoinCfg::is_empty")]
    pub join: JoinCfg,
}

/// `[join]`: which hub this machine joins, for `indra join` without arguments.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct JoinCfg {
    /// The hub's console URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub: Option<String>,
    /// An enrollment key from the hub (Machines > Add many machines).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Folders agents may work in (default: the user's home folder).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
}

impl JoinCfg {
    pub fn is_empty(&self) -> bool {
        self == &JoinCfg::default()
    }
}

/// A user's indra.toml text with the organization's defaults.toml under it
/// and managed.toml over it (see [`crate::deck::managed`]).
pub fn parse_layered(text: &str) -> Result<ConfigFile> {
    let user: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let layers = crate::deck::managed::layers()?;
    if layers.defaults.is_none() && layers.managed.is_none() {
        return toml::from_str(text).map_err(|e| e.to_string());
    }
    toml::Value::Table(crate::deck::managed::apply(user, &layers))
        .try_into()
        .map_err(|e: toml::de::Error| {
            format!(
                "with the organization's settings ({}): {}",
                crate::deck::managed::system_dir().display(),
                e
            )
        })
}

/// `[terminal]`: what the browser terminal starts.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalCfg {
    /// The shell to start (default: $SHELL, else /bin/sh; PowerShell on Windows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Its arguments, e.g. ["-l"] for a login shell.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// More shells to offer in the terminal's New form (besides /etc/shells).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shells: Vec<String>,
}

/// A plugin marketplace or a repository of skills.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceCfg {
    pub name: String,
    /// `owner/repo` on GitHub, an https git URL, or a local folder.
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Branch, tag or commit (default: the default branch).
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
}

/// A planner, workers and an optional reviewer: plan with a strong model,
/// carry the plan out with cheaper ones (see `deck::delegate`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TeamCfg {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Profile that reads the code and writes the plan (changes nothing).
    pub planner: String,
    /// Profile that carries out each step.
    pub worker: String,
    /// Profile for steps the planner marks light (mechanical edits); default: `worker`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_worker: Option<String>,
    /// Profile that checks the result at the end; none: no review step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// Run the plan as soon as it is written, without waiting for approval.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_run: bool,
    /// Let independent steps run at the same time (they share the directory).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parallel: bool,
    /// Most steps a plan may have (default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalCfg {
    /// Agents allowed to run at once on this machine (default 4).
    pub max_parallel: Option<usize>,
    /// Run workflow shell steps on this machine (default true). A node with
    /// `false` refuses the hub's shell steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_steps: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeCfg {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// SSH destination: host, user@host, or a Host alias from ~/.ssh/config.
    /// Optional: a node that dials in (`indra node connect`) needs none, and
    /// SSH is then only the fallback when its link is down.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ssh: String,
    #[serde(default)]
    pub port: Option<u16>,
    /// Private key file (default: whatever ssh would use).
    #[serde(default)]
    pub identity: Option<String>,
    /// indra binary on the remote (default "indra").
    #[serde(default)]
    pub indra: Option<String>,
    /// Extra arguments for remote `indra node ...`, e.g. ["--config", "/etc/indra.toml"].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Agents allowed to run at once on this node (default 2).
    #[serde(default)]
    pub max_parallel: Option<usize>,
    /// Codebases on the remote, as remote paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// ssh client to run (default "ssh").
    #[serde(default)]
    pub ssh_command: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowCfg {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Run it by itself on this cron schedule (`minute hour day month
    /// weekday`, the hub's local time), e.g. "0 9 * * 1-5". `indra serve`
    /// runs it; its inputs take their defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Harness whose name is recorded with each task (default: the first).
    #[serde(default)]
    pub harness: Option<String>,
    /// What the person running it fills in; tasks use them as `{input.NAME}`.
    #[serde(default, rename = "input", skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputCfg>,
    #[serde(default, rename = "task", skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskCfg>,
    /// One of indra's own (not in the config file).
    #[serde(skip)]
    pub builtin: bool,
    /// The repository file it came from (`<repo>/.indra/workflows/*.toml`),
    /// when it is not in the config file.
    #[serde(skip)]
    pub source: Option<PathBuf>,
    /// The inputs' values for this run (filled in by `apply_inputs`).
    #[serde(skip)]
    pub values: std::collections::BTreeMap<String, String>,
}

/// One input of a workflow (`[[workflow.input]]`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputCfg {
    /// `{input.NAME}` in a task's prompt, target, profile or model.
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub help: String,
    /// text (default) | choice | bool | target (one of the harness's
    /// targets) | profile (a configured profile)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// text: a regular expression the whole value must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// choice: the values to pick from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ChoiceCfg>,
    /// bool: what `{input.NAME}` becomes when ticked (empty when not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_true: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceCfg {
    /// What `{input.NAME}` becomes (may be empty).
    #[serde(default)]
    pub value: String,
    pub label: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

pub const INPUT_KINDS: [&str; 5] = ["text", "choice", "bool", "target", "profile"];

/// The workflows indra brings for its own harness: a pull request review, a
/// full security review and a threat model. A configured workflow with the same name wins.
pub fn builtin_workflows() -> Vec<WorkflowCfg> {
    #[derive(Deserialize)]
    struct W {
        workflow: Vec<WorkflowCfg>,
    }
    let mut list = toml::from_str::<W>(BUILTIN_WORKFLOWS)
        .map(|w| w.workflow)
        .unwrap_or_default();
    for w in &mut list {
        w.builtin = true;
    }
    // What a configured task gets by default.
    for t in list.iter_mut().flat_map(|w| w.tasks.iter_mut()) {
        if t.node.is_empty() {
            t.node = crate::deck::nodes::LOCAL.into();
        }
        if t.runner.is_empty() {
            t.runner = "claude".into();
        }
    }
    list
}

const BUILTIN_WORKFLOWS: &str = r##"
[[workflow]]
name = "pr-review"
description = "Security-review a pull request: inline comments and a pass/fail status (a dry run unless you untick it)"
harness = "indra"
[[workflow.input]]
name = "pr"
label = "Pull request"
kind = "text"
required = true
placeholder = "https://github.com/owner/repo/pull/123, or 123"
pattern = '(https://github\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+/pull/[0-9]{1,9}|[0-9]{1,9})'
help = "A pull request URL, or a number in the checkout's own repository."
[[workflow.input]]
name = "target"
label = "Checkout"
kind = "target"
required = true
help = "A clone of the pull request's repository."
[[workflow.input]]
name = "profile"
label = "Profile"
kind = "profile"
help = "Any runner and model, a local one through Ollama included."
[[workflow.input]]
name = "depth"
label = "Depth"
kind = "choice"
options = [
  { value = "", label = "from the diff" },
  { value = "--tier=0", label = "0: triage only" },
  { value = "--tier=1", label = "1: targeted" },
  { value = "--tier=2", label = "2: standard" },
  { value = "--tier=3", label = "3: full" },
]
[[workflow.input]]
name = "fail_on"
label = "Fail on"
kind = "choice"
options = [
  { value = "", label = "default" },
  { value = "--fail-on=critical", label = "critical" },
  { value = "--fail-on=high", label = "high" },
  { value = "--fail-on=medium", label = "medium" },
  { value = "--fail-on=low", label = "low" },
]
[[workflow.input]]
name = "deep"
label = "Deep: every changed file, not just the risky ones"
kind = "bool"
when_true = "--deep"
[[workflow.input]]
name = "dry_run"
label = "Dry run: post nothing; post from the run's page when you have read it"
kind = "bool"
default = "true"
when_true = "--dry-run"
[[workflow.task]]
id = "review"
target = "{input.target}"
profile = "{input.profile}"
prompt = "/sh-pr-review {input.pr} {input.depth} {input.fail_on} {input.deep} {input.dry_run}"

[[workflow]]
name = "security-review"
description = "A full security review of a codebase: recon, hunt, chain, verify, report"
harness = "indra"
[[workflow.input]]
name = "target"
label = "Codebase"
kind = "target"
required = true
[[workflow.input]]
name = "focus"
label = "Focus"
kind = "text"
placeholder = "optional: classes:sqli,ssrf, stage:recon or since:main"
pattern = '[A-Za-z0-9 :,._/~^-]{0,200}'
help = "Leave empty for everything. since:<git ref> reviews only what changed, reusing the earlier map and threat model."
[[workflow.input]]
name = "profile"
label = "Profile"
kind = "profile"
[[workflow.task]]
id = "review"
target = "{input.target}"
profile = "{input.profile}"
prompt = "/sh-security-review {target} {input.focus}"

[[workflow]]
name = "threat-model"
description = "A threat model: data flows, trust boundaries and the controls at each, STRIDE threats ranked by risk, and where to hunt"
harness = "indra"
[[workflow.input]]
name = "target"
label = "Codebase"
kind = "target"
required = true
[[workflow.input]]
name = "focus"
label = "Focus"
kind = "text"
placeholder = "optional: since:main or design:docs/design.md"
pattern = '[A-Za-z0-9 :,._/~^-]{0,200}'
help = "Leave empty for the whole codebase. since:<git ref> models only what changed; design:<file> models a design document."
[[workflow.input]]
name = "profile"
label = "Profile"
kind = "profile"
[[workflow.task]]
id = "model"
target = "{input.target}"
profile = "{input.profile}"
prompt = "/sh-threat-model {target} {input.focus}"
"##;

/// `{input.NAME}` references in a task.
fn input_refs(t: &TaskCfg) -> Vec<String> {
    let mut out = Vec::new();
    let each = t.foreach.as_ref().map(|f| f.list().join("\n"));
    let fields = [
        Some(t.prompt.as_str()),
        Some(t.target.as_str()),
        t.profile.as_deref(),
        t.model.as_deref(),
        each.as_deref(),
    ];
    for f in fields.into_iter().flatten() {
        let mut rest = f;
        while let Some(i) = rest.find("{input.") {
            let after = &rest[i + 7..];
            match after.find('}') {
                Some(j) => {
                    out.push(after[..j].to_string());
                    rest = &after[j + 1..];
                }
                None => break,
            }
        }
    }
    out
}

/// A workflow's inputs are well formed and every reference names one.
pub fn validate_inputs(w: &WorkflowCfg) -> Result<()> {
    let at = |m: String| format!("workflow '{}': {}", w.name, m);
    let mut names: Vec<&str> = Vec::new();
    for i in &w.inputs {
        let ok = !i.name.is_empty()
            && i.name.len() <= 40
            && i.name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok || names.contains(&i.name.as_str()) {
            return Err(at(format!(
                "input name {:?} is invalid or used twice",
                i.name
            )));
        }
        names.push(&i.name);
        let kind = if i.kind.is_empty() {
            "text"
        } else {
            i.kind.as_str()
        };
        if !INPUT_KINDS.contains(&kind) {
            return Err(at(format!(
                "input '{}': kind must be one of {}",
                i.name,
                INPUT_KINDS.join(", ")
            )));
        }
        if kind == "choice" && i.options.is_empty() {
            return Err(at(format!(
                "input '{}' is a choice with no options",
                i.name
            )));
        }
        if let Some(p) = &i.pattern {
            regex::Regex::new(&format!("^(?:{})$", p))
                .map_err(|e| at(format!("input '{}': bad pattern: {}", i.name, e)))?;
        }
    }
    for t in &w.tasks {
        for r in input_refs(t) {
            if !names.contains(&r.as_str()) {
                return Err(at(format!(
                    "task '{}' uses {{input.{}}}, which is not an input",
                    t.id, r
                )));
            }
        }
    }
    Ok(())
}

/// The workflow with the inputs filled in, each checked: required ones
/// given, text matching its pattern (and free of control characters),
/// choices among their options, targets among the harness's, profiles
/// configured. Empty profile or model fields fall back to none.
pub fn apply_inputs(
    w: &WorkflowCfg,
    given: &std::collections::BTreeMap<String, String>,
    targets: &[PathBuf],
    profiles: &[String],
) -> Result<WorkflowCfg> {
    let at = |m: String| format!("{}: {}", w.name, m);
    for k in given.keys() {
        if !w.inputs.iter().any(|i| &i.name == k) {
            return Err(at(format!("there is no input '{}'", k)));
        }
    }
    let mut values: Vec<(String, String)> = Vec::new();
    let mut resolved = std::collections::BTreeMap::new();
    for i in &w.inputs {
        let label = if i.label.is_empty() {
            &i.name
        } else {
            &i.label
        };
        let raw = given
            .get(&i.name)
            .cloned()
            .or_else(|| i.default.clone())
            .unwrap_or_default();
        let raw = raw.trim().to_string();
        if raw.chars().any(|c| c.is_control()) || raw.len() > 2000 {
            return Err(at(format!("{}: not a single line of text", label)));
        }
        let kind = if i.kind.is_empty() {
            "text"
        } else {
            i.kind.as_str()
        };
        if raw.is_empty() && i.required && kind != "bool" {
            return Err(at(format!("{} is required", label)));
        }
        let v = match kind {
            "bool" => {
                if matches!(raw.as_str(), "true" | "1" | "yes" | "on") {
                    i.when_true.clone().unwrap_or_else(|| "true".into())
                } else {
                    String::new()
                }
            }
            "choice" => {
                if !i.options.iter().any(|o| o.value == raw) {
                    return Err(at(format!("{}: not one of the choices", label)));
                }
                raw
            }
            "target" if !raw.is_empty() => {
                let p = PathBuf::from(&raw);
                if !targets.contains(&p) {
                    return Err(at(format!("{}: not one of the harness's targets", label)));
                }
                raw
            }
            "profile" if !raw.is_empty() => {
                if !profiles.contains(&raw) {
                    return Err(at(format!("{}: no profile named '{}'", label, raw)));
                }
                raw
            }
            _ => {
                if let (Some(p), false) = (&i.pattern, raw.is_empty()) {
                    let re = regex::Regex::new(&format!("^(?:{})$", p))
                        .map_err(|e| at(e.to_string()))?;
                    if !re.is_match(&raw) {
                        return Err(at(format!(
                            "{}: {:?} is not in the expected form",
                            label, raw
                        )));
                    }
                }
                raw
            }
        };
        resolved.insert(i.name.clone(), v.clone());
        values.push((format!("{{input.{}}}", i.name), v));
    }
    let fill = |s: &str| {
        let mut s = s.to_string();
        for (k, v) in &values {
            s = s.replace(k, v);
        }
        s
    };
    // Empty inputs leave double spaces in a command line; tidy those.
    let tidy = |s: String| {
        s.lines()
            .map(|l| {
                l.split([' ', '\t'])
                    .filter(|x| !x.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut out = w.clone();
    out.values = resolved;
    for t in &mut out.tasks {
        t.prompt = tidy(fill(&t.prompt));
        if let Some(f) = &t.foreach {
            t.foreach = Some(Items::Many(f.list().iter().map(|x| fill(x)).collect()));
        }
        t.target = fill(&t.target);
        t.profile = t
            .profile
            .as_deref()
            .map(fill)
            .filter(|p| !p.trim().is_empty());
        t.model = t
            .model
            .as_deref()
            .map(fill)
            .filter(|m| !m.trim().is_empty());
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCfg {
    pub id: String,
    /// "local" (the default) or a [[node]] name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub node: String,
    /// Directory on that node the agent runs in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    /// Runner (default: the profile's or the component's, else "claude").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runner: String,
    /// Profile to run with; its runner, if it names one, replaces `runner`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// This task's model, over the profile's (with `provider = "ollama"`,
    /// an Ollama model name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// "ollama" or "anthropic", over the profile's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// `{target}` and `{node}` are filled in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    /// Task ids that must succeed first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    /// Extra attempts after a failure.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retries: u32,
    /// A [[component]] to start from; the task's own fields win.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// Run another [[workflow]] as this one step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// Run this skill (`/<skill> <prompt>`); the prompt is then its
    /// arguments (default `{target}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// A shell command to run instead of an agent (bash, else sh; PowerShell
    /// on Windows), in the target. Inputs, the loop item and earlier steps'
    /// results reach it as environment variables, never as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Run only when this condition holds (see `deck::flow`); else skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// When to start: "success" (the default: everything it needs
    /// succeeded), "failure" (something it needs failed), or "always" (once
    /// everything it needs has finished, however it went).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Its failure does not fail the workflow, and what follows it on
    /// success still runs (`<step>.failed` still tells).
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_failure: bool,
    /// One copy of the step per item, run in parallel: `{item}` in the
    /// prompt and target, `$INDRA_ITEM` in a command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreach: Option<Items>,
    /// Run it again until this condition holds (`self.output contains
    /// "PASS"`), at most `max_runs` times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// The most runs of an `until` loop (default 3, at most 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs: Option<u32>,
    /// A stop step: when it is reached (its needs and `when` allowing),
    /// the workflow ends at once as "success" or "failure"; steps not yet
    /// started are skipped and running ones stopped. `prompt` is its message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
    /// An If step: it runs nothing, checks this condition (like `when`) and
    /// answers "yes" or "no"; the steps after it say which answer they
    /// follow with `branch`.
    #[serde(default, rename = "if", skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// After an If step: run only when it answered this ("yes" or "no").
    /// The other side is skipped, and a step both sides lead to runs after
    /// whichever side ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The loop item this copy runs for (set when a `foreach` is expanded).
    #[serde(skip)]
    pub item: Option<String>,
    /// Where the graph editor draws it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<[i32; 2]>,
}

/// A reusable task: what a workflow task can start from.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentCfg {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runner: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub node: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    /// A skill to run, as on a task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// A shell command to run, as on a task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

/// `foreach`: a list, or one string of items separated by new lines or
/// commas (an input that holds a list, say).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Items {
    One(String),
    Many(Vec<String>),
}

impl Items {
    pub fn list(&self) -> Vec<String> {
        match self {
            Items::One(s) => vec![s.clone()],
            Items::Many(v) => v.clone(),
        }
    }
}

/// The reserved runner shell steps run with, on any node.
pub const SHELL_RUNNER: &str = "indra-shell";

/// The runner for shell steps: bash, else sh (PowerShell on Windows), with
/// the command as one argument, never parsed by indra.
pub fn shell_runner() -> Runner {
    let (command, args): (&str, Vec<&str>) = if cfg!(windows) {
        (
            "powershell",
            vec!["-NoProfile", "-NonInteractive", "-Command", "{prompt}"],
        )
    } else if which("bash").is_some() {
        ("bash", vec!["-c", "{prompt}"])
    } else {
        ("sh", vec!["-c", "{prompt}"])
    };
    Runner {
        name: SHELL_RUNNER.into(),
        program: which(command),
        command: command.into(),
        args: args.into_iter().map(String::from).collect(),
        format: "text".into(),
        resume_args: vec![],
        fork_args: vec![],
        model_args: vec![],
    }
}

/// The prompt that runs `skill` with `args`: Claude Code's slash command
/// for it, or plain words for other runners.
pub fn skill_prompt(skill: &str, args: &str, runner: &str) -> String {
    let args = if args.trim().is_empty() {
        "{target}"
    } else {
        args.trim()
    };
    if runner.is_empty() || runner == "claude" || runner.starts_with("claude") {
        format!("/{} {}", skill, args)
    } else {
        format!("Use the \"{}\" skill. Its input: {}", skill, args)
    }
}

/// A skill name: letters, digits, `-`, `_`, and `:` for a plugin's skills.
pub fn validate_skill(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 100
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
    {
        return Err(format!(
            "skill '{}': letters, digits, -, _ and : only",
            name
        ));
    }
    Ok(())
}

/// A workflow with its components filled in and its sub-workflows
/// inlined, ready to run. A sub-workflow step `s` becomes tasks `s__<id>`:
/// its first tasks wait for what `s` needs, and whatever needs `s` waits for
/// its last tasks.
pub fn expand_workflow(
    w: &WorkflowCfg,
    all: &[WorkflowCfg],
    components: &[ComponentCfg],
) -> Result<WorkflowCfg> {
    fn go(
        w: &WorkflowCfg,
        all: &[WorkflowCfg],
        comps: &[ComponentCfg],
        stack: &mut Vec<String>,
    ) -> Result<Vec<TaskCfg>> {
        let at = |m: String| format!("workflow '{}': {}", w.name, m);
        let mut out: Vec<TaskCfg> = Vec::new();
        // A sub-workflow step's id -> the ids of its last tasks.
        let mut sinks: std::collections::HashMap<String, Vec<String>> = Default::default();
        for t in &w.tasks {
            if let Some(sub) = &t.workflow {
                if t.component.is_some() {
                    return Err(at(format!(
                        "task '{}' cannot have both a component and a workflow",
                        t.id
                    )));
                }
                if stack.contains(sub) || stack.len() > 8 {
                    return Err(at(format!(
                        "workflow '{}' includes itself (through task '{}')",
                        sub, t.id
                    )));
                }
                let sw = all.iter().find(|x| &x.name == sub).ok_or_else(|| {
                    at(format!("task '{}' runs unknown workflow '{}'", t.id, sub))
                })?;
                stack.push(sub.clone());
                let inner = go(sw, all, comps, stack)?;
                stack.pop();
                let prefix = |id: &str| format!("{}__{}", t.id, id);
                let needed: std::collections::HashSet<&String> =
                    inner.iter().flat_map(|x| x.needs.iter()).collect();
                sinks.insert(
                    t.id.clone(),
                    inner
                        .iter()
                        .filter(|x| !needed.contains(&x.id))
                        .map(|x| prefix(&x.id))
                        .collect(),
                );
                let names: std::collections::HashMap<String, String> = inner
                    .iter()
                    .map(|x| (x.id.clone(), prefix(&x.id)))
                    .collect();
                for mut x in inner.clone() {
                    x.needs = if x.needs.is_empty() {
                        t.needs.clone()
                    } else {
                        x.needs.iter().map(|n| prefix(n)).collect()
                    };
                    x.id = prefix(&x.id);
                    // Its conditions name its own steps, now prefixed.
                    x.when = x
                        .when
                        .as_deref()
                        .map(|c| crate::deck::flow::rename(c, &names));
                    x.until = x
                        .until
                        .as_deref()
                        .map(|c| crate::deck::flow::rename(c, &names));
                    x.check = x
                        .check
                        .as_deref()
                        .map(|c| crate::deck::flow::rename(c, &names));
                    out.push(x);
                }
                continue;
            }
            let mut x = t.clone();
            if let Some(cn) = &t.component {
                let c = comps.iter().find(|c| &c.name == cn).ok_or_else(|| {
                    at(format!("task '{}' uses unknown component '{}'", t.id, cn))
                })?;
                let fill = |v: &mut String, from: &str| {
                    if v.is_empty() {
                        *v = from.to_string();
                    }
                };
                fill(&mut x.prompt, &c.prompt);
                fill(&mut x.target, &c.target);
                fill(&mut x.runner, &c.runner);
                fill(&mut x.node, &c.node);
                if x.profile.is_none() {
                    x.profile = c.profile.clone();
                }
                if x.retries == 0 {
                    x.retries = c.retries.unwrap_or(0);
                }
                if x.skill.is_none() {
                    x.skill = c.skill.clone();
                }
                if x.run.is_none() {
                    x.run = c.run.clone();
                }
            }
            if x.node.is_empty() {
                x.node = crate::deck::nodes::LOCAL.into();
            }
            if x.runner.is_empty() {
                x.runner = "claude".into();
            }
            if let Some(sk) = x.skill.take() {
                validate_skill(&sk).map_err(|e| at(format!("task '{}': {}", t.id, e)))?;
                x.prompt = skill_prompt(&sk, &x.prompt, &x.runner);
            }
            x.component = None;
            out.push(x);
        }
        // Whatever needed a sub-workflow step now needs its last tasks.
        for x in &mut out {
            x.needs = x
                .needs
                .iter()
                .flat_map(|n| sinks.get(n).cloned().unwrap_or_else(|| vec![n.clone()]))
                .collect();
        }
        Ok(out)
    }
    let mut stack = vec![w.name.clone()];
    let tasks = go(w, all, components, &mut stack)?;
    Ok(WorkflowCfg { tasks, ..w.clone() })
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerCfg {
    pub bind: Option<String>,
    pub read_only: Option<bool>,
    /// Let nodes that dial in move to a direct WebRTC data channel (default true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webrtc: Option<bool>,
    /// STUN/TURN servers for that, e.g. ["stun:stun.example.org:3478"]. Without
    /// any, direct paths work on the same network; elsewhere traffic is relayed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice_servers: Vec<String>,
    /// STUN/TURN servers with credentials: `[[server.ice]]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice: Vec<IceCfg>,
}

/// One `[[server.ice]]`: a TURN server's URLs and its credentials (the
/// credential may be a `secret:<name>` reference).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IceCfg {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessCfg {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Base directory for the globs below (relative to the config file).
    #[serde(default = "dot")]
    pub root: String,
    /// Globs for agent definitions (Markdown with frontmatter).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// Globs for skills (`SKILL.md` files or any Markdown).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// Globs for other prompt files: system prompts, references, templates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<String>,
    /// Memory vaults (`indra mem`) to show as graphs. `{target}` expands per target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory: Vec<String>,
    /// Codebases whose runs are monitored (relative to the config file).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// Glob, relative to each target, matching one directory per run.
    #[serde(default)]
    pub runs: Option<String>,
    /// JSONL files inside a run holding findings or results; first that exists wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<String>,
    /// Field of a findings record that names the agent or class producing it.
    #[serde(default)]
    pub group_by: Option<String>,
    /// Pipeline stages, in order, for progress and the graph map.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<StageCfg>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StageCfg {
    pub name: String,
    /// Agent (definition name) that runs this stage.
    #[serde(default)]
    pub agent: Option<String>,
    /// A file (glob) inside the run directory whose presence marks the stage done.
    #[serde(default)]
    pub done_when: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerCfg {
    pub name: String,
    /// Program to execute. Run directly, never through a shell.
    pub command: String,
    /// Arguments. `{prompt}` and `{target}` are substituted as whole values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// `claude-stream-json` (Claude Code `--output-format stream-json`) or `text`.
    #[serde(default)]
    pub format: Option<String>,
    /// Extra arguments that continue an earlier session (`{session}` is its id).
    /// Enables "continue" pivots, e.g. ["--resume", "{session}"] for Claude Code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resume_args: Vec<String>,
    /// Added after `resume_args` to branch the session instead of continuing it
    /// in place, used when the original agent keeps running.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fork_args: Vec<String>,
    /// For runners other than Claude Code: arguments that choose a model,
    /// with `{model}`, e.g. ["--model", "{model}"]. Lets profiles pick one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetCfg {
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub runner: Option<String>,
    /// Profile to launch with (model, permission mode, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// A named way to run an agent: a runner with a model, permission mode,
/// effort, tool rules and environment. Launches, presets, workflow tasks and
/// board items pick one, so "plan with Opus, build with Sonnet, try it on a
/// local model" is three profiles.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCfg {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Runner (default: the one chosen at launch, else "claude").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// manual | acceptEdits | auto | plan | dontAsk | bypassPermissions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// low | medium | high | xhigh | max
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// MCP servers: "none" for an agent without any (a much smaller
    /// context), or left out for Claude Code's own configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    /// Environment for the agent, e.g. ANTHROPIC_BASE_URL for a local model.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// Start with the memory notes most relevant to the prompt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub memory: bool,
    /// Before finishing, propose learnings for the inbox (Claude Code runners).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reflect: bool,
    /// Model for Claude Code's own subagents (CLAUDE_CODE_SUBAGENT_MODEL),
    /// e.g. "haiku" under an Opus main agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_model: Option<String>,
    /// "ollama" runs the agent on a model served by Ollama; the model is
    /// `model`. Claude Code and Codex are supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// "ollama" hands Claude Code's subagents (`subagent_model`) to Ollama
    /// while the main agent stays on Anthropic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_provider: Option<String>,
    /// The Ollama server, if not http://localhost:11434.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_url: Option<String>,
    /// Run with this account (or "login": the machine's own login) instead of
    /// letting routing choose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

impl ProfileCfg {
    pub fn opts(&self) -> crate::deck::nodes::AgentOpts {
        crate::deck::nodes::AgentOpts {
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            effort: self.effort.clone(),
            mcp: self.mcp.clone(),
            engagement: None,
            allowed_tools: self.allowed_tools.clone(),
            disallowed_tools: self.disallowed_tools.clone(),
            env: {
                let mut env = self.env.clone();
                if let Some(m) = self
                    .subagent_model
                    .as_ref()
                    .filter(|m| !m.trim().is_empty())
                {
                    env.entry("CLAUDE_CODE_SUBAGENT_MODEL".into())
                        .or_insert_with(|| m.trim().to_string());
                }
                env
            },
            provider: none_if_empty(&self.provider),
            subagent_provider: none_if_empty(&self.subagent_provider),
            ollama_url: none_if_empty(&self.ollama_url),
        }
    }
}

fn none_if_empty(v: &Option<String>) -> Option<String> {
    v.as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A tracker the board imports work from and reports back to.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorCfg {
    pub name: String,
    /// jira | github | clickup | linear
    pub kind: String,
    /// Base URL: the Jira site (https://acme.atlassian.net); GitHub and
    /// ClickUp default to their public APIs (set it for GitHub Enterprise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Jira project key, GitHub "owner/repo", or ClickUp list id.
    pub project: String,
    /// Filter: extra JQL (Jira), a label (GitHub), or statuses (ClickUp, comma-separated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Jira account email (Jira Cloud uses email + API token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// The API token, as a secret reference: "secret:<name>".
    pub token: String,
    /// Profile, node and target to run imported items with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Instructions for the agent; {title}, {key}, {url} and {body} are filled in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Import new items every N minutes (default: only when asked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_minutes: Option<u64>,
    /// Start an agent on each imported item, within the node's capacity.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_run: bool,
    /// Board column -> tracker state: running, review, done, failed.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub states: std::collections::BTreeMap<String, String>,
}

fn dot() -> String {
    ".".into()
}

// ---- resolved settings ---------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Harness {
    pub name: String,
    pub description: String,
    pub root: PathBuf,
    pub agents: Vec<String>,
    pub skills: Vec<String>,
    pub prompts: Vec<String>,
    pub memory: Vec<String>,
    pub targets: Vec<PathBuf>,
    pub runs: Option<String>,
    pub findings: Vec<String>,
    pub group_by: Option<String>,
    pub stages: Vec<StageCfg>,
    pub builtin: bool,
}

impl Harness {
    /// Memory vault paths, with `{target}` expanded for every target.
    pub fn memory_paths(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for m in &self.memory {
            if m.contains("{target}") {
                for t in &self.targets {
                    let p = PathBuf::from(m.replace("{target}", &t.to_string_lossy()));
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            } else {
                let p = expand(&self.root, m);
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        // The global vault, last, for every harness: what one agent learns
        // is there for all of them.
        let g = crate::mem::vault::global();
        if !out.contains(&g) {
            out.push(g);
        }
        out
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Runner {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub format: String,
    pub resume_args: Vec<String>,
    pub fork_args: Vec<String>,
    pub model_args: Vec<String>,
    /// Where the program was found on PATH; None means it is not installed.
    pub program: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub harnesses: Vec<Harness>,
    pub runners: Vec<Runner>,
    pub presets: Vec<PresetCfg>,
    pub profiles: Vec<ProfileCfg>,
    pub connectors: Vec<ConnectorCfg>,
    pub components: Vec<ComponentCfg>,
    pub bind: String,
    pub read_only: bool,
    pub config_path: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub local_max_parallel: usize,
    /// WebRTC for linked nodes, and its ICE servers.
    pub webrtc: bool,
    pub ice_servers: Vec<crate::deck::link::IceServer>,
    pub nodes: Vec<crate::deck::nodes::Node>,
    pub workflows: Vec<WorkflowCfg>,
    pub teams: Vec<TeamCfg>,
    pub marketplaces: Vec<MarketplaceCfg>,
    pub terminal: TerminalCfg,
    pub accounts: Vec<crate::deck::accounts::AccountCfg>,
    pub routing: crate::deck::accounts::RoutingCfg,
    /// SIEM and log sources (read-only).
    pub siem: Vec<crate::deck::siem::SiemCfg>,
    /// Workflow shell steps may run on this machine.
    pub shell_steps: bool,
    /// Workflow files in repositories that could not be used, and why.
    pub workflow_errors: Vec<String>,
    /// Where `.indra/workflows/` is looked for: harness roots and targets.
    pub repo_dirs: Vec<PathBuf>,
}

/// Settings that the console can replace while it runs (after a config
/// edit). Readers take a snapshot; work already started keeps its own.
#[derive(Clone)]
pub struct Shared(std::sync::Arc<std::sync::RwLock<std::sync::Arc<Settings>>>);

impl Shared {
    pub fn new(s: Settings) -> Shared {
        Shared(std::sync::Arc::new(std::sync::RwLock::new(
            std::sync::Arc::new(s),
        )))
    }
    pub fn get(&self) -> std::sync::Arc<Settings> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn set(&self, s: Settings) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = std::sync::Arc::new(s);
    }
}

impl Settings {
    pub fn connector(&self, name: &str) -> Option<&ConnectorCfg> {
        self.connectors.iter().find(|c| c.name == name)
    }

    #[cfg(test)]
    pub fn workflow(&self, name: &str) -> Option<&WorkflowCfg> {
        self.workflows.iter().find(|w| w.name == name)
    }

    pub fn profile(&self, name: &str) -> Option<&ProfileCfg> {
        self.profiles.iter().find(|p| p.name == name)
    }

    pub fn team(&self, name: &str) -> Option<&TeamCfg> {
        self.teams.iter().find(|t| t.name == name)
    }

    pub fn node(&self, name: &str) -> Option<&crate::deck::nodes::Node> {
        self.nodes.iter().find(|n| n.name == name)
    }

    /// Agents allowed at once on a node.
    pub fn capacity(&self, node: &str) -> usize {
        if node == crate::deck::nodes::LOCAL {
            self.local_max_parallel
        } else {
            self.node(node).map(|n| n.max_parallel).unwrap_or(1)
        }
    }
}

/// Check a workflow: tasks exist, ids are unique and simple, nodes are
/// known, every `needs` names a task, and the dependencies have no cycle.
pub fn validate_workflow(w: &WorkflowCfg, node_names: &[String]) -> Result<()> {
    let at = |m: String| format!("workflow '{}': {}", w.name, m);
    if w.tasks.is_empty() {
        return Err(at("has no [[workflow.task]]".into()));
    }
    let mut ids: Vec<&str> = Vec::new();
    for t in &w.tasks {
        crate::deck::nodes::validate_name(&t.id).map_err(|e| at(format!("task id: {}", e)))?;
        if ids.contains(&t.id.as_str()) {
            return Err(at(format!("task id '{}' is used twice", t.id)));
        }
        ids.push(&t.id);
        if t.node != crate::deck::nodes::LOCAL && !node_names.contains(&t.node) {
            return Err(at(format!(
                "task '{}' runs on unknown node '{}'",
                t.id, t.node
            )));
        }
        if t.prompt.trim().is_empty() && t.run.is_none() && t.stop.is_none() && t.check.is_none() {
            return Err(at(format!("task '{}' has an empty prompt", t.id)));
        }
        if t.target.trim().is_empty() {
            return Err(at(format!("task '{}' has no target", t.id)));
        }
    }
    for t in &w.tasks {
        for n in &t.needs {
            if !ids.contains(&n.as_str()) {
                return Err(at(format!("task '{}' needs unknown task '{}'", t.id, n)));
            }
        }
    }
    validate_flow(w).map_err(at)?;
    if let Some(sc) = &w.schedule {
        crate::deck::cron::Cron::parse(sc).map_err(|e| at(format!("schedule: {}", e)))?;
    }
    // Kahn's algorithm: whatever cannot be ordered is on a cycle.
    let mut done: Vec<&str> = Vec::new();
    loop {
        let ready: Vec<&str> = w
            .tasks
            .iter()
            .filter(|t| {
                !done.contains(&t.id.as_str()) && t.needs.iter().all(|n| done.contains(&n.as_str()))
            })
            .map(|t| t.id.as_str())
            .collect();
        if ready.is_empty() {
            break;
        }
        done.extend(ready);
    }
    if done.len() != w.tasks.len() {
        let stuck: Vec<&str> = ids.iter().copied().filter(|i| !done.contains(i)).collect();
        return Err(at(format!("dependency cycle among: {}", stuck.join(", "))));
    }
    Ok(())
}

/// Conditions, loops and commands: each condition parses and names steps
/// that run before it (so their result is known), a command takes no text
/// from inputs, and loops stay within their limits.
fn validate_flow(w: &WorkflowCfg) -> std::result::Result<(), String> {
    use crate::deck::flow;
    let ids: Vec<&str> = w.tasks.iter().map(|t| t.id.as_str()).collect();
    let inputs: Vec<&str> = w.inputs.iter().map(|i| i.name.as_str()).collect();
    let looped: Vec<&str> = w
        .tasks
        .iter()
        .filter(|t| t.foreach.is_some())
        .map(|t| t.id.as_str())
        .collect();
    // Everything that runs before a step, through its needs.
    let before = |id: &str| -> std::collections::HashSet<String> {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![id.to_string()];
        while let Some(x) = stack.pop() {
            if let Some(t) = w.tasks.iter().find(|t| t.id == x) {
                for n in &t.needs {
                    if seen.insert(n.clone()) {
                        stack.push(n.clone());
                    }
                }
            }
        }
        seen
    };
    for t in &w.tasks {
        let at = |m: String| format!("task '{}': {}", t.id, m);
        let earlier = before(&t.id);
        let known = |used: Vec<String>| -> std::result::Result<(), String> {
            for u in used {
                if u == t.id {
                    return Err(at(
                        "a condition cannot name its own step (use self.<field> in until)".into(),
                    ));
                }
                if !earlier.contains(&u) {
                    return Err(at(format!(
                        "it reads step '{}', which does not run before it (add '{}' to its needs)",
                        u, u
                    )));
                }
                if looped.contains(&u.as_str()) {
                    return Err(at(format!(
                        "it reads step '{}', which runs once per item; follow it with on = \"failure\" or \"always\" instead",
                        u
                    )));
                }
            }
            Ok(())
        };
        if let Some(r) = &t.run {
            if r.trim().is_empty() || r.len() > 20_000 {
                return Err(at("run must be a command (up to 20000 characters)".into()));
            }
            if t.skill.is_some() || t.workflow.is_some() {
                return Err(at(
                    "a step runs a command, a skill or a workflow, not several".into(),
                ));
            }
            for bad in ["{input.", "{item}", "{steps."] {
                if r.contains(bad) {
                    return Err(at(format!(
                        "a command cannot contain {}...}}: use $INDRA_INPUT_<NAME>, $INDRA_ITEM or $INDRA_OUTPUT_<STEP> (environment variables, so no value can change the command)",
                        bad
                    )));
                }
            }
        }
        if let Some(st) = &t.stop {
            if !["success", "failure"].contains(&st.as_str()) {
                return Err(at(format!("stop must be success or failure, not {:?}", st)));
            }
            if t.run.is_some()
                || t.skill.is_some()
                || t.workflow.is_some()
                || t.foreach.is_some()
                || t.until.is_some()
            {
                return Err(at(
                    "a stop step only ends the workflow: no run, skill, workflow, foreach or until"
                        .into(),
                ));
            }
        }
        if let Some(o) = &t.on {
            if !["success", "failure", "always"].contains(&o.as_str()) {
                return Err(at(format!(
                    "on must be success, failure or always, not {:?}",
                    o
                )));
            }
        }
        if let Some(c) = &t.when {
            known(flow::check(c, &ids, &inputs, false).map_err(at)?)?;
        }
        if let Some(c) = &t.check {
            known(flow::check(c, &ids, &inputs, false).map_err(at)?)?;
            if t.run.is_some()
                || t.skill.is_some()
                || t.workflow.is_some()
                || t.foreach.is_some()
                || t.until.is_some()
                || t.stop.is_some()
            {
                return Err(at(
                    "an If step only checks its condition: no run, skill, workflow, loop or stop"
                        .into(),
                ));
            }
        }
        if let Some(b) = &t.branch {
            if !["yes", "no"].contains(&b.as_str()) {
                return Err(at(format!("branch must be yes or no, not {:?}", b)));
            }
            let after_if = t
                .needs
                .iter()
                .any(|n| w.tasks.iter().any(|x| &x.id == n && x.check.is_some()));
            if !after_if {
                return Err(at(
                    "branch needs an If step among the steps it follows".into()
                ));
            }
        }
        if let Some(c) = &t.until {
            known(flow::check(c, &ids, &inputs, true).map_err(at)?)?;
        }
        if let Some(n) = t.max_runs {
            if !(1..=flow::MAX_RUNS).contains(&n) {
                return Err(at(format!("max_runs must be 1 to {}", flow::MAX_RUNS)));
            }
            if t.until.is_none() {
                return Err(at("max_runs needs an until condition".into()));
            }
        }
        if let Some(f) = &t.foreach {
            let list = f.list();
            if !list.iter().any(|x| x.contains("{input."))
                && flow::items(&list).len() > flow::MAX_ITEMS
            {
                return Err(at(format!(
                    "foreach has more than {} items",
                    flow::MAX_ITEMS
                )));
            }
        }
        // `{steps.<id>.<field>}` in a prompt: an earlier step's result.
        let mut rest = t.prompt.as_str();
        while let Some(i) = rest.find("{steps.") {
            let after = &rest[i + 7..];
            let Some(j) = after.find('}') else { break };
            let (sid, field) = after[..j].rsplit_once('.').unwrap_or((&after[..j], ""));
            if !["output", "exit_code", "status"].contains(&field) {
                return Err(at(format!(
                    "{{steps.{}}}: write {{steps.<step>.output}}, .exit_code or .status",
                    &after[..j]
                )));
            }
            if !ids.contains(&sid) {
                return Err(at(format!(
                    "{{steps.{}.{}}}: there is no step '{}'",
                    sid, field, sid
                )));
            }
            known(vec![sid.to_string()])?;
            rest = &after[j + 1..];
        }
    }
    Ok(())
}

pub const DEFAULT_BIND: &str = "127.0.0.1:7878";

pub use crate::paths::home;

/// `~/x` to the home directory, relative paths against `base`.
pub fn expand(base: &Path, p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(h) = home() {
            return h.join(rest);
        }
    }
    let pb = PathBuf::from(p);
    if pb.is_absolute() {
        pb
    } else {
        base.join(pb)
    }
}

/// Logs and other state indra writes, per user.
pub fn data_dir() -> PathBuf {
    crate::paths::data_dir()
}

/// Where a config file is looked for when `--config` is not given.
pub fn default_config_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut v = vec![cwd.join("indra.toml")];
    v.extend(
        crate::paths::config_dirs()
            .into_iter()
            .map(|d| d.join("indra.toml")),
    );
    v
}

/// Find a program on PATH the way a shell would (PATHEXT on Windows).
pub fn which(cmd: &str) -> Option<PathBuf> {
    let p = Path::new(cmd);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = dir.join(format!("{}{}", cmd, ext));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

// ---- built-in profiles ----------------------------------------------------

/// The indra plugin directory (`plugins/indra`, or `security-harness` from
/// before the rename): in this repository (walking up
/// from `cwd`), or as installed by Claude Code under `~/.claude/plugins`.
/// Folder names the plugin has been shipped under.
const PLUGIN_DIRS: [&str; 2] = ["indra", "security-harness"];

pub fn find_security_harness(cwd: &Path) -> Option<PathBuf> {
    let looks_right = |d: &Path| d.join("agents").is_dir() && d.join("skills").is_dir();
    let mut dir = cwd.to_path_buf();
    loop {
        for name in PLUGIN_DIRS {
            let cand = dir.join("plugins").join(name);
            if looks_right(&cand) {
                return Some(cand);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    let plugins = home()?.join(".claude").join("plugins");
    let mut stack = vec![(plugins, 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            if PLUGIN_DIRS.iter().any(|n| e.file_name() == *n) && looks_right(&p) {
                return Some(p);
            }
            if depth < 5 {
                stack.push((p, depth + 1));
            }
        }
    }
    None
}

pub fn security_harness_profile(plugin: &Path, targets: &[PathBuf]) -> Harness {
    let stage = |name: &str, agent: Option<&str>, done: &str| StageCfg {
        name: name.into(),
        agent: agent.map(String::from),
        done_when: Some(done.into()),
    };
    let global_memory = crate::mem::vault::global();
    Harness {
        name: "indra".into(),
        description: "Multi-agent application-security review: recon, parallel hunters, chain, verify, report.".into(),
        root: plugin.to_path_buf(),
        agents: vec!["agents/*.md".into()],
        skills: vec!["skills/*/SKILL.md".into()],
        prompts: vec!["references/*.md".into(), "references/*.json".into()],
        memory: vec![
            "{target}/.security-harness/memory".into(),
            global_memory.to_string_lossy().into_owned(),
        ],
        targets: targets.to_vec(),
        runs: Some(".security-harness/{run,pr}-*".into()),
        findings: vec!["verified.jsonl".into(), "findings.jsonl".into()],
        group_by: Some("class".into()),
        stages: vec![
            stage("recon", Some("sh-recon"), "recon.md"),
            stage("threat model", Some("sh-threat-modeler"), "threat-model.md"),
            stage("hunt", Some("sh-hunter"), "findings.jsonl"),
            stage("chain", Some("sh-chainer"), "chains.md"),
            stage("verify", Some("sh-verifier"), "verified.jsonl"),
            stage("report", Some("sh-reporter"), "reports/README.md"),
        ],
        builtin: true,
    }
}

/// Any Claude Code project: `.claude/agents`, `.claude/skills`, commands
/// and memory files, plus the user-level ones under `~/.claude`.
pub fn claude_code_profile(root: &Path, targets: &[PathBuf]) -> Harness {
    Harness {
        name: "claude-code".into(),
        description: "Claude Code subagents, skills and commands in this project and in ~/.claude."
            .into(),
        root: root.to_path_buf(),
        agents: vec![".claude/agents/*.md".into(), "~/.claude/agents/*.md".into()],
        skills: vec![
            ".claude/skills/*/SKILL.md".into(),
            "~/.claude/skills/*/SKILL.md".into(),
        ],
        prompts: vec![
            "CLAUDE.md".into(),
            "AGENTS.md".into(),
            ".claude/commands/*.md".into(),
            "~/.claude/CLAUDE.md".into(),
            "~/.claude/commands/*.md".into(),
        ],
        memory: Vec::new(),
        targets: targets.to_vec(),
        runs: None,
        findings: Vec::new(),
        group_by: None,
        stages: Vec::new(),
        builtin: true,
    }
}

pub fn default_runners() -> Vec<RunnerCfg> {
    let r = |name: &str, command: &str, args: &[&str], format: &str| RunnerCfg {
        name: name.into(),
        command: command.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        format: Some(format.into()),
        resume_args: Vec::new(),
        fork_args: Vec::new(),
        model_args: Vec::new(),
    };
    let mut claude = r(
        "claude",
        "claude",
        &[
            "-p",
            "{prompt}",
            "--output-format",
            "stream-json",
            "--verbose",
        ],
        "claude-stream-json",
    );
    claude.resume_args = vec!["--resume".into(), "{session}".into()];
    claude.fork_args = vec!["--fork-session".into()];
    vec![
        claude,
        {
            let mut c = r("codex", "codex", &["exec", "{prompt}"], "text");
            c.model_args = vec!["-m".into(), "{model}".into()];
            c
        },
        {
            let mut g = r("gemini", "gemini", &["-p", "{prompt}"], "text");
            g.model_args = vec!["-m".into(), "{model}".into()];
            g
        },
    ]
}

/// Profiles that exist without any configuration, so teams, workflows and
/// Launch work out of the box. A configured profile with the same name wins.
pub fn default_profiles() -> Vec<ProfileCfg> {
    let tools = |t: &[&str]| t.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let edit = tools(&[
        "Read",
        "Grep",
        "Glob",
        "Edit",
        "Write",
        "Bash(git diff:*)",
        "Bash(git status:*)",
    ]);
    vec![
        ProfileCfg {
            name: "planner".into(),
            description: "Reads and plans with Opus; changes nothing".into(),
            runner: Some("claude".into()),
            model: Some("opus".into()),
            permission_mode: Some("dontAsk".into()),
            effort: Some("high".into()),
            allowed_tools: tools(&[
                "Read",
                "Grep",
                "Glob",
                "Bash(git log:*)",
                "Bash(git diff:*)",
                "Bash(ls:*)",
            ]),
            ..Default::default()
        },
        ProfileCfg {
            name: "worker".into(),
            description: "Does the work with Sonnet, accepting its own edits".into(),
            runner: Some("claude".into()),
            model: Some("sonnet".into()),
            permission_mode: Some("acceptEdits".into()),
            allowed_tools: edit.clone(),
            ..Default::default()
        },
        ProfileCfg {
            name: "builder".into(),
            description: "Small, well-specified steps with Haiku, cheaply".into(),
            runner: Some("claude".into()),
            model: Some("haiku".into()),
            permission_mode: Some("acceptEdits".into()),
            allowed_tools: edit,
            ..Default::default()
        },
        ProfileCfg {
            name: "reviewer".into(),
            description: "Reviews with Opus and fixes small mistakes".into(),
            runner: Some("claude".into()),
            model: Some("opus".into()),
            permission_mode: Some("acceptEdits".into()),
            ..Default::default()
        },
    ]
}

/// A team made of the default profiles.
pub fn default_teams() -> Vec<TeamCfg> {
    vec![TeamCfg {
        name: "plan-and-build".into(),
        description: "Opus plans, Sonnet builds, Haiku does the light steps, Opus reviews".into(),
        planner: "planner".into(),
        worker: "worker".into(),
        light_worker: Some("builder".into()),
        reviewer: Some("reviewer".into()),
        ..Default::default()
    }]
}

pub fn default_presets() -> Vec<PresetCfg> {
    let p = |name: &str, prompt: &str| PresetCfg {
        name: name.into(),
        prompt: prompt.into(),
        harness: Some("indra".into()),
        runner: Some("claude".into()),
        profile: None,
    };
    vec![
        p("Full security review", "/sh-security-review {target}"),
        p(
            "Recon only (map the codebase)",
            "/sh-security-review {target} stage:recon",
        ),
        p(
            "Threat model (trust boundaries, STRIDE)",
            "/sh-threat-model {target}",
        ),
        p(
            "Review what changed since main",
            "/sh-security-review {target} since:main",
        ),
        p(
            "Hunt SQLi and access control",
            "/sh-security-review {target} classes:sqli,access-control",
        ),
        p(
            "What does memory know?",
            "/sh-memory recall what do we know about this codebase",
        ),
    ]
}

#[derive(Debug, Clone)]
pub struct LoadOptions {
    pub config: Option<PathBuf>,
    pub targets: Vec<PathBuf>,
    pub bind: Option<String>,
    pub read_only: bool,
    pub cwd: PathBuf,
}

/// `ice_servers` (plain URLs) and `[[server.ice]]` (with credentials), checked.
fn ice_servers(s: &ServerCfg) -> Result<Vec<crate::deck::link::IceServer>> {
    use crate::deck::link::IceServer;
    let ok = |u: &str| {
        ["stun:", "stuns:", "turn:", "turns:"]
            .iter()
            .any(|p| u.starts_with(p))
            && u.len() <= 512
            && !u.chars().any(|c| c.is_control() || c.is_whitespace())
    };
    let mut out: Vec<IceServer> = Vec::new();
    for u in &s.ice_servers {
        if !ok(u) {
            return Err(format!(
                "server.ice_servers: not a stun:/turn: URL: {:?}",
                u
            ));
        }
        out.push(IceServer {
            urls: vec![u.clone()],
            ..Default::default()
        });
    }
    for (i, c) in s.ice.iter().enumerate() {
        if c.urls.is_empty() || c.urls.iter().any(|u| !ok(u)) {
            return Err(format!("server.ice[{}]: urls must be stun:/turn: URLs", i));
        }
        if let Some(name) = crate::deck::secrets::reference(&c.credential) {
            crate::deck::secrets::validate_name(name)
                .map_err(|e| format!("server.ice[{}] credential: {}", i, e))?;
        }
        out.push(IceServer {
            urls: c.urls.clone(),
            username: c.username.clone(),
            credential: c.credential.clone(),
        });
    }
    Ok(out)
}

pub fn load(opts: LoadOptions) -> Result<Settings> {
    let path = match &opts.config {
        Some(p) => {
            if !p.is_file() {
                return Err(format!("config file not found: {}", p.display()));
            }
            Some(p.clone())
        }
        None => default_config_paths(&opts.cwd)
            .into_iter()
            .find(|p| p.is_file()),
    };
    let file = load_file(path.as_deref())?;
    build_settings(&opts, path, &file)
}

/// A config file (or none) with the organization's settings applied.
pub fn load_file(path: Option<&Path>) -> Result<ConfigFile> {
    let text = match path {
        Some(p) => {
            std::fs::read_to_string(p).map_err(|e| format!("cannot read {}: {}", p.display(), e))?
        }
        None => String::new(),
    };
    parse_layered(&text).map_err(|e| match path {
        Some(p) => format!("{}: {}", p.display(), e),
        None => e,
    })
}

/// The config file `load` would read for these options, if any.
pub fn config_path(opts: &LoadOptions) -> Option<PathBuf> {
    match &opts.config {
        Some(p) => p.is_file().then(|| p.clone()),
        None => default_config_paths(&opts.cwd)
            .into_iter()
            .find(|p| p.is_file()),
    }
}

/// What a repository's `.indra/workflows/*.toml` may hold.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoFile {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workflow: Vec<WorkflowCfg>,
    /// Saved tasks the workflows can start from.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub component: Vec<ComponentCfg>,
}

/// The folder in a repository that holds its workflows.
pub fn repo_workflow_dir(repo: &Path) -> PathBuf {
    repo.join(".indra").join("workflows")
}

/// The workflow files of a repository, in name order.
pub fn repo_workflow_files(repo: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(repo_workflow_dir(repo))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn read_repo_file(f: &Path) -> Result<RepoFile> {
    let meta = std::fs::metadata(f).map_err(|e| e.to_string())?;
    if meta.len() > 1 << 20 {
        return Err("larger than 1 MB".into());
    }
    let text = std::fs::read_to_string(f).map_err(|e| e.to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

/// Write a repository's workflow file (or delete it when it holds nothing),
/// with a header saying what it is.
pub fn write_repo_file(f: &Path, rf: &RepoFile) -> Result<()> {
    if rf.workflow.is_empty() && rf.component.is_empty() {
        return match std::fs::remove_file(f) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    let body = toml::to_string(rf).map_err(|e| e.to_string())?;
    let names: Vec<&str> = rf.workflow.iter().map(|w| w.name.as_str()).collect();
    let text = format!(
        "# indra workflow{}: {}. Commit this file to share it: everyone whose\n\
         # indra console or `indra run` has this repository as a target gets it.\n\
         # Edit it in the console's workflow editor or by hand.\n\n{}",
        if names.len() == 1 { "" } else { "s" },
        names.join(", "),
        body
    );
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    crate::deck::confedit::write_atomic(f, &text)
}

/// Names pick things out (on Launch, in workflows), so each must be unique.
fn unique<'a>(what: &str, names: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen: Vec<&str> = Vec::new();
    for n in names {
        if n.trim().is_empty() {
            return Err(format!("every [[{}]] needs a name", what));
        }
        if seen.contains(&n) {
            return Err(format!("{} name '{}' is used twice", what, n));
        }
        seen.push(n);
    }
    Ok(())
}

/// The file the console saves settings to: the one that was loaded, or
/// `--config`, or the per-user `indra.toml` when there is none yet.
pub fn edit_path(opts: &LoadOptions, loaded: Option<&Path>) -> PathBuf {
    if let Some(p) = loaded {
        return p.to_path_buf();
    }
    if let Some(p) = &opts.config {
        return p.clone();
    }
    crate::paths::config_dirs()
        .into_iter()
        .next()
        .map(|d| d.join("indra.toml"))
        .unwrap_or_else(|| opts.cwd.join("indra.toml"))
}

/// Resolve a parsed config file into settings: paths, built-in profiles,
/// runners, nodes and workflows, every one of them checked.
pub fn build_settings(
    opts: &LoadOptions,
    path: Option<PathBuf>,
    file: &ConfigFile,
) -> Result<Settings> {
    let base = path
        .as_ref()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| opts.cwd.clone());
    let abs = |p: PathBuf| -> PathBuf { std::fs::canonicalize(&p).unwrap_or(p) };
    unique("harness", file.harness.iter().map(|h| h.name.as_str()))?;
    unique("runner", file.runner.iter().map(|r| r.name.as_str()))?;
    unique("preset", file.preset.iter().map(|p| p.name.as_str()))?;

    let cli_targets: Vec<PathBuf> = opts.targets.iter().map(|t| abs(opts.cwd.join(t))).collect();
    let default_targets = if cli_targets.is_empty() {
        vec![abs(opts.cwd.clone())]
    } else {
        cli_targets.clone()
    };

    let mut harnesses: Vec<Harness> = Vec::new();
    for h in &file.harness {
        if h.name.trim().is_empty() {
            return Err("every [[harness]] needs a name".into());
        }
        let root = abs(expand(&base, &h.root));
        let targets = if !cli_targets.is_empty() {
            cli_targets.clone()
        } else if h.targets.is_empty() {
            default_targets.clone()
        } else {
            h.targets.iter().map(|t| abs(expand(&base, t))).collect()
        };
        harnesses.push(Harness {
            name: h.name.clone(),
            description: h.description.clone(),
            root,
            agents: h.agents.clone(),
            skills: h.skills.clone(),
            prompts: h.prompts.clone(),
            memory: h.memory.clone(),
            targets,
            runs: h.runs.clone(),
            findings: h.findings.clone(),
            group_by: h.group_by.clone(),
            stages: h.stages.clone(),
            builtin: false,
        });
    }
    if file.builtins.unwrap_or(true) {
        let mut builtins = Vec::new();
        if let Some(plugin) = find_security_harness(&opts.cwd) {
            builtins.push(security_harness_profile(&abs(plugin), &default_targets));
        }
        let cc = claude_code_profile(&abs(opts.cwd.clone()), &default_targets);
        if !crate::deck::discover::discover(&cc).is_empty() {
            builtins.push(cc);
        }
        for b in builtins {
            if !harnesses.iter().any(|h| h.name == b.name) {
                harnesses.push(b);
            }
        }
    }
    // The security harness is the default view when it is present.
    harnesses.sort_by_key(|h| (h.name != "indra", h.builtin));

    let mut runner_cfgs = file.runner.clone();
    for d in default_runners() {
        if !runner_cfgs.iter().any(|r| r.name == d.name) {
            runner_cfgs.push(d);
        }
    }
    let mut runners = Vec::new();
    for r in runner_cfgs {
        let format = r.format.clone().unwrap_or_else(|| "text".into());
        if format != "text" && format != "claude-stream-json" {
            return Err(format!(
                "runner '{}': format must be 'text' or 'claude-stream-json'",
                r.name
            ));
        }
        if !r.args.iter().any(|a| a.contains("{prompt}")) {
            return Err(format!("runner '{}': args must contain {{prompt}}", r.name));
        }
        // Claude Code's stream-json runners can always be continued.
        let (resume_args, fork_args) = if format == "claude-stream-json" && r.resume_args.is_empty()
        {
            (
                vec!["--resume".to_string(), "{session}".to_string()],
                vec!["--fork-session".to_string()],
            )
        } else {
            (r.resume_args.clone(), r.fork_args.clone())
        };
        if !r.model_args.is_empty() && !r.model_args.iter().any(|a| a.contains("{model}")) {
            return Err(format!(
                "runner '{}': model_args must contain {{model}}",
                r.name
            ));
        }
        if !resume_args.is_empty() && !resume_args.iter().any(|a| a.contains("{session}")) {
            return Err(format!(
                "runner '{}': resume_args must contain {{session}}",
                r.name
            ));
        }
        runners.push(Runner {
            program: which(&r.command),
            name: r.name,
            command: r.command,
            args: r.args,
            format,
            resume_args,
            fork_args,
            model_args: r.model_args,
        });
    }

    unique("profile", file.profile.iter().map(|p| p.name.as_str()))?;
    // The default profiles, under any configured with the same name.
    let mut profiles = file.profile.clone();
    if file.builtins.unwrap_or(true) {
        for d in default_profiles() {
            if !profiles.iter().any(|p| p.name == d.name) {
                profiles.push(d);
            }
        }
    }
    for p in &file.profile {
        p.opts()
            .validate()
            .map_err(|e| format!("profile '{}': {}", p.name, e))?;
        if let Some(r) = &p.runner {
            crate::deck::nodes::validate_name(r)
                .map_err(|e| format!("profile '{}': {}", p.name, e))?;
        }
        for (k, v) in &p.env {
            if let Some(name) = crate::deck::secrets::reference(v) {
                crate::deck::secrets::validate_name(name)
                    .map_err(|e| format!("profile '{}' env {}: {}", p.name, k, e))?;
            }
        }
    }
    let known = |p: &Option<String>, what: &str| -> Result<()> {
        match p {
            Some(n) if !profiles.iter().any(|x| &x.name == n) => Err(format!(
                "{} uses profile '{}', which is not defined",
                what, n
            )),
            _ => Ok(()),
        }
    };
    for p in &file.preset {
        known(&p.profile, &format!("preset '{}'", p.name))?;
    }
    unique(
        "marketplace",
        file.marketplace.iter().map(|m| m.name.as_str()),
    )?;
    for m in &file.marketplace {
        let what = format!("marketplace '{}'", m.name);
        crate::deck::nodes::validate_name(&m.name).map_err(|e| format!("{}: {}", what, e))?;
        if m.source.trim().is_empty() {
            return Err(format!(
                "{}: source is required (owner/repo, a git URL or a folder)",
                what
            ));
        }
        if let Some(r) = &m.git_ref {
            if r.starts_with('-') || r.chars().any(|c| c.is_whitespace()) {
                return Err(format!("{}: bad ref", what));
            }
        }
    }
    crate::deck::accounts::check(&file.account, &file.routing)?;
    for p in &file.profile {
        if let Some(a) = p.account.as_deref().filter(|a| *a != "login") {
            if !file.account.iter().any(|x| x.name == a) {
                return Err(format!(
                    "profile '{}' uses account '{}', which is not defined (or use \"login\" for the machine's own)",
                    p.name, a
                ));
            }
        }
    }
    unique("team", file.team.iter().map(|t| t.name.as_str()))?;
    for t in &file.team {
        let what = format!("team '{}'", t.name);
        crate::deck::nodes::validate_name(&t.name).map_err(|e| format!("{}: {}", what, e))?;
        for p in [
            Some(&t.planner),
            Some(&t.worker),
            t.light_worker.as_ref(),
            t.reviewer.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            known(&Some(p.clone()), &what)?;
        }
        if let Some(n) = t.max_steps {
            if !(1..=30).contains(&n) {
                return Err(format!("{}: max_steps must be 1 to 30", what));
            }
        }
    }
    unique("connector", file.connector.iter().map(|c| c.name.as_str()))?;
    unique("siem", file.siem.iter().map(|c| c.name.as_str()))?;
    for c in &file.siem {
        c.validate()?;
    }
    for c in &file.connector {
        let at = |m: String| format!("connector '{}': {}", c.name, m);
        crate::deck::nodes::validate_name(&c.name).map_err(at)?;
        if !crate::deck::trackers::KINDS.contains(&c.kind.as_str()) {
            return Err(at(format!(
                "kind must be one of {}",
                crate::deck::trackers::KINDS.join(", ")
            )));
        }
        match crate::deck::secrets::reference(&c.token) {
            Some(n) => crate::deck::secrets::validate_name(n).map_err(at)?,
            None => return Err(at("token must be a secret reference, \"secret:<name>\" (set it in Settings > Secrets), never the token itself".into())),
        }
        if c.kind == "jira" && (c.url.is_none() || c.email.is_none()) {
            return Err(at(
                "Jira needs url (https://<site>.atlassian.net) and email".into(),
            ));
        }
        if let Some(u) = &c.url {
            if !(u.starts_with("https://") || u.starts_with("http://")) {
                return Err(at("url must start with https://".into()));
            }
        }
        if c.project.trim().is_empty() {
            return Err(at(
                "project is required (Jira key, GitHub owner/repo, or ClickUp list id)".into(),
            ));
        }
        for k in c.states.keys() {
            if !["running", "review", "done", "failed"].contains(&k.as_str()) {
                return Err(at(format!(
                    "states: '{}' is not a board column (running, review, done, failed)",
                    k
                )));
            }
        }
        known(&c.profile, &format!("connector '{}'", c.name))?;
    }
    for w in &file.workflow {
        for t in &w.tasks {
            // `{input.profile}` is checked when the workflow runs.
            if t.profile.as_deref().is_some_and(|p| p.contains("{input.")) {
                continue;
            }
            known(
                &t.profile,
                &format!("workflow '{}' task '{}'", w.name, t.id),
            )?;
        }
    }
    let mut teams = file.team.clone();
    if file.builtins.unwrap_or(true) {
        for d in default_teams() {
            if !teams.iter().any(|t| t.name == d.name) {
                teams.push(d);
            }
        }
    }
    let mut presets = file.preset.clone();
    for d in default_presets() {
        if !presets.iter().any(|p| p.name == d.name) {
            presets.push(d);
        }
    }

    let mut nodes = Vec::new();
    for n in &file.node {
        crate::deck::nodes::validate_name(&n.name)?;
        if n.name == crate::deck::nodes::LOCAL
            || nodes
                .iter()
                .any(|x: &crate::deck::nodes::Node| x.name == n.name)
        {
            return Err(format!("node name '{}' is reserved or used twice", n.name));
        }
        if !n.ssh.is_empty() {
            crate::deck::nodes::validate_destination(&n.ssh)?;
        }
        nodes.push(crate::deck::nodes::Node {
            name: n.name.clone(),
            description: n.description.clone(),
            ssh: n.ssh.clone(),
            port: n.port,
            identity: n.identity.as_ref().map(|i| expand(&base, i)),
            indra: n.indra.clone().unwrap_or_else(|| "indra".into()),
            args: n.args.clone(),
            max_parallel: n.max_parallel.unwrap_or(2).max(1),
            targets: n.targets.clone(),
            ssh_command: n.ssh_command.clone().unwrap_or_else(|| "ssh".into()),
        });
    }
    let node_names: Vec<String> = nodes.iter().map(|n| n.name.clone()).collect();
    unique("component", file.component.iter().map(|c| c.name.as_str()))?;
    for c in &file.component {
        known(&c.profile, &format!("component '{}'", c.name))?;
    }
    for w in &file.workflow {
        for t in &w.tasks {
            if t.workflow.is_none() {
                known(
                    &t.profile,
                    &format!("workflow '{}' task '{}'", w.name, t.id),
                )?;
            }
        }
    }
    // Workflows (and saved tasks) kept in repositories, to commit and share:
    // `<repo>/.indra/workflows/*.toml` under every harness root and target.
    // Also the targets given on the command line (or the current folder),
    // so `indra run` inside a repository finds its workflows with no harness.
    let mut repo_dirs: Vec<PathBuf> = Vec::new();
    for h in harnesses
        .iter()
        .map(|h| (Some(&h.root), &h.targets))
        .chain(std::iter::once((None, &default_targets)))
    {
        for d in h.0.into_iter().chain(h.1.iter()) {
            if !repo_dirs.contains(d) {
                repo_dirs.push(d.clone());
            }
        }
    }
    let mut workflow_errors: Vec<String> = Vec::new();
    let mut all_comps = file.component.clone();
    let mut repo_raw: Vec<(PathBuf, PathBuf, WorkflowCfg)> = Vec::new();
    for d in &repo_dirs {
        for f in repo_workflow_files(d) {
            match read_repo_file(&f) {
                Ok(rf) => {
                    for c in rf.component {
                        if !all_comps.iter().any(|x| x.name == c.name) {
                            all_comps.push(c);
                        }
                    }
                    for w in rf.workflow {
                        repo_raw.push((d.clone(), f.clone(), w));
                    }
                }
                Err(e) => workflow_errors.push(format!("{}: {}", f.display(), e)),
            }
        }
    }
    for c in &all_comps {
        if let Some(sk) = &c.skill {
            validate_skill(sk).map_err(|e| format!("component '{}': {}", c.name, e))?;
        }
    }
    let mut all_raw = file.workflow.clone();
    for (_, _, w) in &repo_raw {
        if !all_raw.iter().any(|x| x.name == w.name) {
            all_raw.push(w.clone());
        }
    }
    // Everything a runnable workflow must pass, once expanded.
    let finish = |w: &WorkflowCfg| -> Result<()> {
        validate_workflow(w, &node_names)?;
        validate_inputs(w)?;
        for t in &w.tasks {
            if t.model.as_deref().is_some_and(|m| m.contains("{input.")) {
                continue;
            }
            crate::deck::nodes::AgentOpts {
                model: t.model.clone(),
                provider: t.provider.clone().filter(|p| !p.is_empty()),
                ..Default::default()
            }
            .validate()
            .map_err(|e| format!("workflow '{}' task '{}': {}", w.name, t.id, e))?;
        }
        Ok(())
    };
    let mut workflows = Vec::new();
    for w in &file.workflow {
        if workflows.iter().any(|x: &WorkflowCfg| x.name == w.name) {
            return Err(format!("workflow name '{}' is used twice", w.name));
        }
        // Ids are checked on the workflow as written, the rest once expanded.
        crate::deck::nodes::validate_name(&w.name).map_err(|e| format!("workflow name: {}", e))?;
        let mut w = expand_workflow(w, &all_raw, &all_comps)?;
        // A local step without a target runs in the harness's first target
        // (the workflow's harness, else the first one), like a Launch does.
        let home = w
            .harness
            .as_deref()
            .and_then(|n| harnesses.iter().find(|h| h.name == n))
            .or_else(|| harnesses.first())
            .and_then(|h| h.targets.first().cloned().or_else(|| Some(h.root.clone())))
            .or_else(|| default_targets.first().cloned())
            .unwrap_or_else(|| base.clone());
        for t in &mut w.tasks {
            if t.node == crate::deck::nodes::LOCAL && t.target.trim().is_empty() {
                t.target = home.to_string_lossy().into_owned();
            }
        }
        finish(&w)?;
        // Local targets are relative to the config file; remote ones are the
        // remote's business. One from an input is resolved when it runs.
        for t in &mut w.tasks {
            if t.node == crate::deck::nodes::LOCAL && !t.target.contains("{input.") {
                t.target = abs(expand(&base, &t.target)).to_string_lossy().into_owned();
            }
        }
        workflows.push(w);
    }
    // A repository's workflow runs in that repository unless it says
    // otherwise; one that cannot be used is listed, never fatal (it may name
    // a profile or node only its author has).
    for (repo, f, w) in &repo_raw {
        if workflows.iter().any(|x: &WorkflowCfg| x.name == w.name) {
            workflow_errors.push(format!(
                "{}: workflow '{}' is not used: one with that name is already defined{}",
                f.display(),
                w.name,
                if file.workflow.iter().any(|x| x.name == w.name) {
                    " in your indra.toml"
                } else {
                    ""
                }
            ));
            continue;
        }
        let built = (|| -> Result<WorkflowCfg> {
            crate::deck::nodes::validate_name(&w.name)
                .map_err(|e| format!("workflow name: {}", e))?;
            for t in &w.tasks {
                if t.workflow.is_none()
                    && !t.profile.as_deref().is_some_and(|p| p.contains("{input."))
                {
                    known(
                        &t.profile,
                        &format!("workflow '{}' task '{}'", w.name, t.id),
                    )?;
                }
            }
            let mut x = expand_workflow(w, &all_raw, &all_comps)?;
            for t in &mut x.tasks {
                if t.node == crate::deck::nodes::LOCAL && !t.target.contains("{input.") {
                    t.target = if t.target.trim().is_empty() {
                        repo.to_string_lossy().into_owned()
                    } else {
                        abs(expand(repo, &t.target)).to_string_lossy().into_owned()
                    };
                }
            }
            finish(&x)?;
            x.source = Some(f.clone());
            Ok(x)
        })();
        match built {
            Ok(x) => workflows.push(x),
            Err(e) => workflow_errors.push(format!("{}: {}", f.display(), e)),
        }
    }
    // indra's own workflows, for its harness, unless configured otherwise.
    if file.builtins.unwrap_or(true) && harnesses.iter().any(|h| h.name == "indra") {
        for b in builtin_workflows() {
            if !workflows.iter().any(|w| w.name == b.name) {
                workflows.push(b);
            }
        }
    }

    Ok(Settings {
        harnesses,
        runners,
        presets,
        profiles: profiles.clone(),
        connectors: file.connector.clone(),
        components: all_comps.clone(),
        local_max_parallel: file.local.max_parallel.unwrap_or(4).max(1),
        webrtc: file.server.webrtc.unwrap_or(true),
        ice_servers: ice_servers(&file.server)?,
        nodes,
        workflows,
        bind: opts
            .bind
            .clone()
            .or(file.server.bind.clone())
            .unwrap_or_else(|| DEFAULT_BIND.into()),
        read_only: opts.read_only || file.server.read_only.unwrap_or(false),
        config_path: path.map(abs),
        data_dir: data_dir(),
        marketplaces: file.marketplace.clone(),
        teams,
        terminal: file.terminal.clone(),
        accounts: file.account.clone(),
        routing: file.routing.clone(),
        siem: file.siem.clone(),
        shell_steps: file.local.shell_steps.unwrap_or(true),
        workflow_errors,
        repo_dirs,
    })
}

/// The commented starter file `indra init` writes.
pub const SAMPLE: &str = r#"# indra configuration.
#
# Two harness profiles are built in and detected automatically:
#   indra             - the indra security-review plugin (this repository, or installed)
#   claude-code       - .claude/agents, .claude/skills and commands, here and in ~/.claude
# Add your own harness below; a [[harness]] with a built-in name replaces it.

# builtins = false          # only show the harnesses defined in this file

[server]
bind = "127.0.0.1:7878"     # keep it on loopback; a token is required either way
# read_only = true          # disable launching agents and editing prompts

# [[harness]]
# name        = "my-harness"
# description = "Research agents"
# root        = "."                                  # base for the globs below
# agents      = ["agents/*.md"]                      # Markdown with frontmatter (name, description, model, tools)
# skills      = ["skills/*/SKILL.md"]
# prompts     = ["prompts/**/*.md", "system.md"]
# memory      = ["{target}/.memory", "~/notes"]      # memory vaults to draw as graphs
# targets     = ["../project-a", "../project-b"]     # where runs happen
# runs        = ".runs/*"                            # one directory per run, under each target
# findings    = ["results.jsonl"]                    # JSONL results inside a run
# group_by    = "agent"                              # field naming who produced a result
# stages      = [                                    # pipeline order, for progress and the graph
#   { name = "plan", agent = "planner", done_when = "plan.md" },
#   { name = "execute", agent = "worker", done_when = "results.jsonl" },
# ]

# Runners launch agents. The program is executed directly, never through a
# shell; {prompt} and {target} are substituted as whole arguments.
# Built-in runners: claude, codex, gemini (shown only when installed).
#
# [[runner]]
# name    = "claude-edits"
# command = "claude"
# args    = ["-p", "{prompt}", "--output-format", "stream-json", "--verbose", "--permission-mode", "acceptEdits"]
# format  = "claude-stream-json"

# This machine as an agent node.
# [local]
# max_parallel = 4                                   # agents at once here

# Remote machines, reached over SSH (keys, agent and ~/.ssh/config as usual).
# The remote needs `indra` on its PATH; it runs only its own runners, by name.
# [[node]]
# name         = "build-box"
# ssh          = "me@build-box.internal"             # or a Host alias from ~/.ssh/config
# max_parallel = 2
# targets      = ["/srv/api", "/srv/web"]            # codebases on that machine
# indra        = "indra"                             # path of indra on the remote
# args         = ["--config", "/etc/indra.toml"]     # extra args for the remote indra

# Workflows: tasks on any node, ordered by `needs`, run with `indra run <name>`
# or from the console. A task whose dependency failed is skipped.
# [[workflow]]
# name = "nightly"
# [[workflow.task]]
# id = "api"
# node = "build-box"
# target = "/srv/api"
# prompt = "/indra full security review of {target}"
# retries = 1
# [[workflow.task]]
# id = "web"
# node = "local"
# target = "../web"
# prompt = "/indra full security review of {target}"
# [[workflow.task]]
# id = "summary"
# target = "."
# prompt = "Summarise today's review runs and list new critical findings."
# needs = ["api", "web"]

# Presets are saved prompts on the Launch page.
# [[preset]]
# name    = "Nightly review"
# harness = "indra"
# runner  = "claude"
# prompt  = "/sh-security-review {target} depth:quick"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_parses_and_is_all_comments_by_default() {
        let c: ConfigFile = toml::from_str(SAMPLE).unwrap();
        assert!(c.harness.is_empty() && c.runner.is_empty());
        assert_eq!(c.server.bind.as_deref(), Some("127.0.0.1:7878"));
        let uncommented: String = SAMPLE
            .lines()
            .map(|l| {
                l.strip_prefix("# ")
                    .filter(|r| {
                        r.starts_with('[')
                            || r.contains(" = ")
                            || r.trim_start().starts_with('{')
                            || r.trim() == "]"
                    })
                    .unwrap_or(l)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let c: ConfigFile = toml::from_str(&uncommented).unwrap();
        assert_eq!(c.harness[0].stages.len(), 2);
        assert_eq!(c.runner[0].name, "claude-edits");
        assert_eq!(c.local.max_parallel, Some(4));
        assert_eq!(c.node[0].name, "build-box");
        assert_eq!(c.workflow[0].tasks.len(), 3);
        let w = expand_workflow(&c.workflow[0], &c.workflow, &c.component).unwrap();
        assert!(validate_workflow(&w, &["build-box".into()]).is_ok());
    }

    #[test]
    fn components_and_sub_workflows_expand() {
        let c: ConfigFile = toml::from_str(
            r#"
[[component]]
name = "review"
prompt = "review {target}"
profile = "p"
retries = 2

[[workflow]]
name = "inner"
[[workflow.task]]
id = "a"
target = "/a"
prompt = "x"
[[workflow.task]]
id = "b"
target = "/b"
prompt = "y"
needs = ["a"]

[[workflow]]
name = "outer"
[[workflow.task]]
id = "first"
component = "review"
target = "/srv"
[[workflow.task]]
id = "sub"
workflow = "inner"
needs = ["first"]
pos = [120, 40]
[[workflow.task]]
id = "last"
component = "review"
target = "/t"
prompt = "my own prompt"
needs = ["sub"]
"#,
        )
        .unwrap();
        let w = expand_workflow(&c.workflow[1], &c.workflow, &c.component).unwrap();
        let ids: Vec<&str> = w.tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["first", "sub__a", "sub__b", "last"]);
        let t = |id: &str| w.tasks.iter().find(|t| t.id == id).unwrap();
        assert_eq!(t("first").prompt, "review {target}");
        assert_eq!(t("first").profile.as_deref(), Some("p"));
        assert_eq!(t("first").retries, 2);
        assert_eq!(t("first").node, "local");
        assert_eq!(t("first").runner, "claude");
        assert_eq!(
            t("last").prompt,
            "my own prompt",
            "the task's own fields win"
        );
        assert_eq!(
            t("sub__a").needs,
            ["first"],
            "the sub-workflow starts after its step's needs"
        );
        assert_eq!(t("sub__b").needs, ["sub__a"]);
        assert_eq!(
            t("last").needs,
            ["sub__b"],
            "what needed the step waits for its last task"
        );
        assert!(validate_workflow(&w, &[]).is_ok());

        // A workflow that includes itself is refused.
        let mut looped = c.workflow.clone();
        looped[0].tasks.push(TaskCfg {
            id: "again".into(),
            workflow: Some("outer".into()),
            ..Default::default()
        });
        let e = expand_workflow(&looped[1], &looped, &c.component).unwrap_err();
        assert!(e.contains("includes itself"), "{}", e);
        let mut bad = c.workflow[1].clone();
        bad.tasks[0].component = Some("nope".into());
        assert!(expand_workflow(&bad, &c.workflow, &c.component)
            .unwrap_err()
            .contains("unknown component"));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<ConfigFile>("[[harness]]\nname='x'\nagnets=['a']\n").is_err());
    }

    #[test]
    fn memory_paths_expand_targets() {
        let h =
            security_harness_profile(Path::new("/p"), &[PathBuf::from("/a"), PathBuf::from("/b")]);
        let m = h.memory_paths();
        assert_eq!(m[0], PathBuf::from("/a/.security-harness/memory"));
        assert_eq!(m[1], PathBuf::from("/b/.security-harness/memory"));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn teams_name_real_profiles_and_subagents_get_their_model() {
        let opts = LoadOptions {
            config: None,
            targets: vec![],
            bind: None,
            read_only: false,
            cwd: std::env::temp_dir(),
        };
        let base = "builtins = false\n[[profile]]\nname = \"planner\"\nmodel = \"opus\"\nsubagent_model = \"haiku\"\n\
[[profile]]\nname = \"builder\"\nmodel = \"haiku\"\n";
        let ok: ConfigFile = toml::from_str(&format!(
            "{}[[team]]\nname = \"t\"\nplanner = \"planner\"\nworker = \"builder\"\nreviewer = \"planner\"\n",
            base
        ))
        .unwrap();
        let s = build_settings(&opts, None, &ok).unwrap();
        assert_eq!(s.team("t").unwrap().worker, "builder");
        assert_eq!(
            s.profile("planner")
                .unwrap()
                .opts()
                .env
                .get("CLAUDE_CODE_SUBAGENT_MODEL")
                .map(String::as_str),
            Some("haiku")
        );
        for bad in [
            "[[team]]\nname = \"t\"\nplanner = \"planner\"\nworker = \"nobody\"\n",
            "[[team]]\nname = \"t\"\nplanner = \"planner\"\nworker = \"builder\"\nmax_steps = 99\n",
            "[[team]]\nname = \"t\"\nplanner = \"planner\"\nworker = \"builder\"\n[[team]]\nname = \"t\"\nplanner = \"planner\"\nworker = \"builder\"\n",
        ] {
            let f: ConfigFile = toml::from_str(&format!("{}{}", base, bad)).unwrap();
            assert!(build_settings(&opts, None, &f).is_err(), "{}", bad);
        }
    }

    #[test]
    fn workflow_inputs() {
        let b = builtin_workflows();
        assert_eq!(
            b.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec!["pr-review", "security-review", "threat-model"]
        );
        for w in &b {
            validate_inputs(w).unwrap();
            validate_workflow(w, &[]).unwrap();
        }
        let pr = &b[0];
        let targets = vec![PathBuf::from("/code/shop")];
        let profiles = vec!["local".to_string()];
        let mut g = std::collections::BTreeMap::new();
        g.insert(
            "pr".to_string(),
            "https://github.com/acme/shop/pull/7".to_string(),
        );
        g.insert("target".to_string(), "/code/shop".to_string());
        g.insert("depth".to_string(), "--tier=2".to_string());
        g.insert("deep".to_string(), "true".to_string());
        let w = apply_inputs(pr, &g, &targets, &profiles).unwrap();
        let t = &w.tasks[0];
        // Unset inputs leave no gaps; dry run is on by default.
        assert_eq!(
            t.prompt,
            "/sh-pr-review https://github.com/acme/shop/pull/7 --tier=2 --deep --dry-run"
        );
        assert_eq!(t.target, "/code/shop");
        assert_eq!(t.profile, None);
        g.insert("dry_run".to_string(), "false".to_string());
        g.insert("profile".to_string(), "local".to_string());
        let w = apply_inputs(pr, &g, &targets, &profiles).unwrap();
        assert_eq!(
            w.tasks[0].prompt,
            "/sh-pr-review https://github.com/acme/shop/pull/7 --tier=2 --deep"
        );
        assert_eq!(w.tasks[0].profile.as_deref(), Some("local"));
        // Refusals: required, pattern (no injection), choices, targets, profiles, unknown names.
        for (k, v) in [
            ("pr", ""),
            ("pr", "7; rm -rf /"),
            ("pr", "https://evil.example/a/b/pull/1"),
            ("depth", "--tier=9"),
            ("target", "/etc"),
            ("profile", "nobody"),
            ("nope", "x"),
            ("pr", "12\n/other"),
        ] {
            let mut g2 = g.clone();
            g2.insert(k.to_string(), v.to_string());
            assert!(
                apply_inputs(pr, &g2, &targets, &profiles).is_err(),
                "{}={:?}",
                k,
                v
            );
        }
        // A task may only use inputs the workflow declares.
        let mut bad = pr.clone();
        bad.tasks[0].prompt = "/x {input.missing}".into();
        assert!(validate_inputs(&bad).is_err());
        let mut bad = pr.clone();
        bad.inputs[0].kind = "file".into();
        assert!(validate_inputs(&bad).is_err());
    }

    #[test]
    fn ice_servers_with_turn_credentials() {
        use crate::deck::link::{ice_frame, parse_ice};
        let opts = LoadOptions {
            config: None,
            targets: vec![],
            bind: None,
            read_only: false,
            cwd: std::env::temp_dir(),
        };
        let f: ConfigFile = toml::from_str(
            "builtins = false\n[server]\nice_servers = [\"stun:stun.example.org:3478\"]\n\
[[server.ice]]\nurls = [\"turn:turn.example.org:3478\", \"turns:turn.example.org:5349\"]\nusername = \"indra\"\ncredential = \"secret:turn\"\n",
        )
        .unwrap();
        let s = build_settings(&opts, None, &f).unwrap();
        assert_eq!(s.ice_servers.len(), 2);
        assert_eq!(s.ice_servers[1].username, "indra");
        assert_eq!(s.ice_servers[1].credential, "secret:turn");
        // Plain URLs stay plain on the wire (older nodes read them); TURN
        // servers go as objects, and both come back the same.
        let frame = ice_frame(&s.ice_servers);
        assert_eq!(frame[0], "stun:stun.example.org:3478");
        assert_eq!(frame[1]["username"], "indra");
        assert_eq!(parse_ice(Some(&frame)), s.ice_servers);
        let one =
            serde_json::json!([{ "urls": "turn:x:3478", "username": "u", "credential": "c" }, 7]);
        assert_eq!(parse_ice(Some(&one))[0].urls, vec!["turn:x:3478"]);
        for bad in [
            "[server]\nice_servers = [\"http://x\"]\n",
            "[[server.ice]]\nurls = []\n",
            "[[server.ice]]\nurls = [\"turn:x\"]\ncredential = \"secret:bad name\"\n",
        ] {
            let f: ConfigFile = toml::from_str(&format!("builtins = false\n{}", bad)).unwrap();
            assert!(build_settings(&opts, None, &f).is_err(), "{}", bad);
        }
    }

    #[test]
    fn ollama_profiles_and_tasks() {
        let opts = LoadOptions {
            config: None,
            targets: vec![],
            bind: None,
            read_only: false,
            cwd: std::env::temp_dir(),
        };
        let good = "builtins = false\n\
[[profile]]\nname = \"local\"\nrunner = \"claude\"\nmodel = \"qwen3-coder\"\nprovider = \"ollama\"\nollama_url = \"http://gpu:11434\"\n\
[[profile]]\nname = \"lead\"\nmodel = \"opus\"\nsubagent_model = \"qwen3-coder\"\nsubagent_provider = \"ollama\"\n\
[[workflow]]\nname = \"w\"\n[[workflow.task]]\nid = \"a\"\ntarget = \".\"\nprompt = \"p\"\nprofile = \"lead\"\nmodel = \"llama3\"\nprovider = \"ollama\"\n";
        let f: ConfigFile = toml::from_str(good).unwrap();
        let s = build_settings(&opts, None, &f).unwrap();
        let o = s.profile("local").unwrap().opts();
        assert_eq!(o.provider.as_deref(), Some("ollama"));
        assert_eq!(o.ollama_url.as_deref(), Some("http://gpu:11434"));
        let o = s.profile("lead").unwrap().opts();
        assert_eq!(o.subagent_provider.as_deref(), Some("ollama"));
        assert_eq!(o.env["CLAUDE_CODE_SUBAGENT_MODEL"], "qwen3-coder");
        assert_eq!(s.workflows[0].tasks[0].model.as_deref(), Some("llama3"));
        for bad in [
            "[[profile]]\nname = \"x\"\nprovider = \"openai\"\n",
            "[[profile]]\nname = \"x\"\nprovider = \"ollama\"\nollama_url = \"file:///x\"\n",
            "[[workflow]]\nname = \"v\"\n[[workflow.task]]\nid = \"a\"\ntarget = \".\"\nprompt = \"p\"\nprovider = \"bedrock\"\n",
        ] {
            let f: ConfigFile = toml::from_str(&format!("builtins = false\n{}", bad)).unwrap();
            assert!(build_settings(&opts, None, &f).is_err(), "{}", bad);
        }
    }

    #[test]
    fn defaults_workflows_in_repositories_and_skill_steps() {
        let base = std::env::temp_dir().join(format!("indra-repo-wf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("api");
        let wfdir = repo.join(".indra/workflows");
        std::fs::create_dir_all(&wfdir).unwrap();
        // A shared workflow: no target (the repository), a default profile,
        // a skill step, and a saved task kept next to it.
        std::fs::write(
            wfdir.join("triage.toml"),
            "[[component]]\nname = \"lint\"\nprompt = \"lint {target}\"\n\n\
[[workflow]]\nname = \"triage\"\n\
[[workflow.task]]\nid = \"scan\"\nskill = \"sh-security-review\"\nprompt = \"{target} stage:recon\"\nprofile = \"planner\"\n\
[[workflow.task]]\nid = \"lint\"\ncomponent = \"lint\"\ntarget = \"sub\"\nneeds = [\"scan\"]\n",
        )
        .unwrap();
        // One naming a profile only its author has: listed, not fatal.
        std::fs::write(
            wfdir.join("theirs.toml"),
            "[[workflow]]\nname = \"theirs\"\n[[workflow.task]]\nid = \"a\"\nprompt = \"p\"\nprofile = \"only-on-my-laptop\"\n",
        )
        .unwrap();
        std::fs::write(wfdir.join("broken.toml"), "[[workflow]\n").unwrap();
        // Same name as one in the config file: the config file wins.
        std::fs::write(
            wfdir.join("mine.toml"),
            "[[workflow]]\nname = \"mine\"\n[[workflow.task]]\nid = \"a\"\nprompt = \"from the repo\"\n",
        )
        .unwrap();
        let opts = LoadOptions {
            config: None,
            targets: vec![repo.clone()],
            bind: None,
            read_only: false,
            cwd: base.clone(),
        };
        let f: ConfigFile = toml::from_str(
            "[[workflow]]\nname = \"mine\"\n[[workflow.task]]\nid = \"a\"\ntarget = \".\"\nprompt = \"from the config\"\n",
        )
        .unwrap();
        let s = build_settings(&opts, None, &f).unwrap();
        // Defaults: profiles and a team, with no configuration at all.
        for p in ["planner", "worker", "builder", "reviewer"] {
            assert!(s.profile(p).is_some(), "{}", p);
        }
        assert!(s.teams.iter().any(|t| t.name == "plan-and-build"));
        let t = s
            .workflows
            .iter()
            .find(|w| w.name == "triage")
            .expect("loaded");
        let repo_abs = std::fs::canonicalize(&repo).unwrap();
        assert!(t.source.as_ref().unwrap().ends_with("triage.toml"));
        assert_eq!(
            t.tasks[0].prompt,
            "/sh-security-review {target} stage:recon"
        );
        assert_eq!(t.tasks[0].target, repo_abs.to_string_lossy());
        assert_eq!(t.tasks[1].prompt, "lint {target}");
        assert!(t.tasks[1].target.ends_with("sub"));
        assert!(s.components.iter().any(|c| c.name == "lint"));
        let mine = s.workflows.iter().find(|w| w.name == "mine").unwrap();
        assert_eq!(mine.tasks[0].prompt, "from the config");
        assert!(mine.source.is_none());
        let errs = s.workflow_errors.join("\n");
        assert!(
            errs.contains("theirs.toml") && errs.contains("only-on-my-laptop"),
            "{}",
            errs
        );
        assert!(errs.contains("broken.toml"), "{}", errs);
        assert!(
            errs.contains("mine.toml") && errs.contains("your indra.toml"),
            "{}",
            errs
        );
        assert!(!s.workflows.iter().any(|w| w.name == "theirs"));
        // A configured profile replaces the default one; builtins = false drops them.
        let f: ConfigFile =
            toml::from_str("[[profile]]\nname = \"planner\"\nmodel = \"sonnet\"\n").unwrap();
        let s = build_settings(&opts, None, &f).unwrap();
        assert_eq!(
            s.profile("planner").unwrap().model.as_deref(),
            Some("sonnet")
        );
        let f: ConfigFile = toml::from_str("builtins = false\n").unwrap();
        let s = build_settings(&opts, None, &f).unwrap();
        assert!(s.profile("planner").is_none() && s.teams.is_empty());
        // Skills: other runners get words, names are checked.
        assert_eq!(
            skill_prompt("triage", "", "codex"),
            "Use the \"triage\" skill. Its input: {target}"
        );
        assert!(validate_skill("plugin:skill-1").is_ok());
        assert!(validate_skill("x; rm -rf /").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
