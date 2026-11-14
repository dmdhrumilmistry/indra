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
    /// Harness whose name is recorded with each task (default: the first).
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default, rename = "task", skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskCfg>,
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
                for mut x in inner.clone() {
                    x.needs = if x.needs.is_empty() {
                        t.needs.clone()
                    } else {
                        x.needs.iter().map(|n| prefix(n)).collect()
                    };
                    x.id = prefix(&x.id);
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
            }
            if x.node.is_empty() {
                x.node = crate::deck::nodes::LOCAL.into();
            }
            if x.runner.is_empty() {
                x.runner = "claude".into();
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
}

impl ProfileCfg {
    pub fn opts(&self) -> crate::deck::nodes::AgentOpts {
        crate::deck::nodes::AgentOpts {
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            effort: self.effort.clone(),
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
        if t.prompt.trim().is_empty() {
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
    let global_memory = crate::mem::vault::locate(None);
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
    let file: ConfigFile = match &path {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read {}: {}", p.display(), e))?;
            toml::from_str(&text).map_err(|e| format!("{}: {}", p.display(), e))?
        }
        None => ConfigFile::default(),
    };
    build_settings(&opts, path, &file)
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
            Some(n) if !file.profile.iter().any(|x| &x.name == n) => Err(format!(
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
            known(
                &t.profile,
                &format!("workflow '{}' task '{}'", w.name, t.id),
            )?;
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
    let mut workflows = Vec::new();
    for w in &file.workflow {
        if workflows.iter().any(|x: &WorkflowCfg| x.name == w.name) {
            return Err(format!("workflow name '{}' is used twice", w.name));
        }
        // Ids are checked on the workflow as written, the rest once expanded.
        crate::deck::nodes::validate_name(&w.name).map_err(|e| format!("workflow name: {}", e))?;
        let mut w = expand_workflow(w, &file.workflow, &file.component)?;
        validate_workflow(&w, &node_names)?;
        for t in &w.tasks {
            crate::deck::nodes::AgentOpts {
                model: t.model.clone(),
                provider: t.provider.clone().filter(|p| !p.is_empty()),
                ..Default::default()
            }
            .validate()
            .map_err(|e| format!("workflow '{}' task '{}': {}", w.name, t.id, e))?;
        }
        // Local targets are relative to the config file; remote ones are the remote's business.
        for t in &mut w.tasks {
            if t.node == crate::deck::nodes::LOCAL {
                t.target = abs(expand(&base, &t.target)).to_string_lossy().into_owned();
            }
        }
        workflows.push(w);
    }

    Ok(Settings {
        harnesses,
        runners,
        presets,
        profiles: file.profile.clone(),
        connectors: file.connector.clone(),
        components: file.component.clone(),
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
        teams: file.team.clone(),
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
}
