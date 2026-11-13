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
    pub builtins: Option<bool>,
    pub harness: Vec<HarnessCfg>,
    pub runner: Vec<RunnerCfg>,
    pub preset: Vec<PresetCfg>,
    /// This machine as an agent node.
    pub local: LocalCfg,
    /// Remote machines reached over SSH.
    pub node: Vec<NodeCfg>,
    /// Multi-step, multi-node agent workflows.
    pub workflow: Vec<WorkflowCfg>,
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
    #[serde(default)]
    pub description: String,
    /// SSH destination: host, user@host, or a Host alias from ~/.ssh/config.
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
    #[serde(default)]
    pub args: Vec<String>,
    /// Agents allowed to run at once on this node (default 2).
    #[serde(default)]
    pub max_parallel: Option<usize>,
    /// Codebases on the remote, as remote paths.
    #[serde(default)]
    pub targets: Vec<String>,
    /// ssh client to run (default "ssh").
    #[serde(default)]
    pub ssh_command: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowCfg {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Harness whose name is recorded with each task (default: the first).
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default, rename = "task")]
    pub tasks: Vec<TaskCfg>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCfg {
    pub id: String,
    /// "local" or a [[node]] name.
    #[serde(default = "local_node")]
    pub node: String,
    /// Directory on that node the agent runs in.
    pub target: String,
    #[serde(default = "claude_runner")]
    pub runner: String,
    /// `{target}` and `{node}` are filled in.
    pub prompt: String,
    /// Task ids that must succeed first.
    #[serde(default)]
    pub needs: Vec<String>,
    /// Extra attempts after a failure.
    #[serde(default)]
    pub retries: u32,
}

fn local_node() -> String {
    crate::deck::nodes::LOCAL.into()
}
fn claude_runner() -> String {
    "claude".into()
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerCfg {
    pub bind: Option<String>,
    pub read_only: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessCfg {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Base directory for the globs below (relative to the config file).
    #[serde(default = "dot")]
    pub root: String,
    /// Globs for agent definitions (Markdown with frontmatter).
    #[serde(default)]
    pub agents: Vec<String>,
    /// Globs for skills (`SKILL.md` files or any Markdown).
    #[serde(default)]
    pub skills: Vec<String>,
    /// Globs for other prompt files: system prompts, references, templates.
    #[serde(default)]
    pub prompts: Vec<String>,
    /// Memory vaults (`indra mem`) to show as graphs. `{target}` expands per target.
    #[serde(default)]
    pub memory: Vec<String>,
    /// Codebases whose runs are monitored (relative to the config file).
    #[serde(default)]
    pub targets: Vec<String>,
    /// Glob, relative to each target, matching one directory per run.
    #[serde(default)]
    pub runs: Option<String>,
    /// JSONL files inside a run holding findings or results; first that exists wins.
    #[serde(default)]
    pub findings: Vec<String>,
    /// Field of a findings record that names the agent or class producing it.
    #[serde(default)]
    pub group_by: Option<String>,
    /// Pipeline stages, in order, for progress and the graph map.
    #[serde(default)]
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
    #[serde(default)]
    pub args: Vec<String>,
    /// `claude-stream-json` (Claude Code `--output-format stream-json`) or `text`.
    #[serde(default)]
    pub format: Option<String>,
    /// Extra arguments that continue an earlier session (`{session}` is its id).
    /// Enables "continue" pivots, e.g. ["--resume", "{session}"] for Claude Code.
    #[serde(default)]
    pub resume_args: Vec<String>,
    /// Added after `resume_args` to branch the session instead of continuing it
    /// in place, used when the original agent keeps running.
    #[serde(default)]
    pub fork_args: Vec<String>,
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
    /// Where the program was found on PATH; None means it is not installed.
    pub program: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub harnesses: Vec<Harness>,
    pub runners: Vec<Runner>,
    pub presets: Vec<PresetCfg>,
    pub bind: String,
    pub read_only: bool,
    pub config_path: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub local_max_parallel: usize,
    pub nodes: Vec<crate::deck::nodes::Node>,
    pub workflows: Vec<WorkflowCfg>,
}

impl Settings {
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
        runs: Some(".security-harness/run-*".into()),
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

fn default_runners() -> Vec<RunnerCfg> {
    let r = |name: &str, command: &str, args: &[&str], format: &str| RunnerCfg {
        name: name.into(),
        command: command.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        format: Some(format.into()),
        resume_args: Vec::new(),
        fork_args: Vec::new(),
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
        r("codex", "codex", &["exec", "{prompt}"], "text"),
        r("gemini", "gemini", &["-p", "{prompt}"], "text"),
    ]
}

fn default_presets() -> Vec<PresetCfg> {
    let p = |name: &str, prompt: &str| PresetCfg {
        name: name.into(),
        prompt: prompt.into(),
        harness: Some("indra".into()),
        runner: Some("claude".into()),
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

pub struct LoadOptions {
    pub config: Option<PathBuf>,
    pub targets: Vec<PathBuf>,
    pub bind: Option<String>,
    pub read_only: bool,
    pub cwd: PathBuf,
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
    let base = path
        .as_ref()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| opts.cwd.clone());
    let abs = |p: PathBuf| -> PathBuf { std::fs::canonicalize(&p).unwrap_or(p) };

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
        });
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
        crate::deck::nodes::validate_destination(&n.ssh)?;
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
    let mut workflows = Vec::new();
    for w in &file.workflow {
        validate_workflow(w, &node_names)?;
        if workflows.iter().any(|x: &WorkflowCfg| x.name == w.name) {
            return Err(format!("workflow name '{}' is used twice", w.name));
        }
        let mut w = w.clone();
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
        local_max_parallel: file.local.max_parallel.unwrap_or(4).max(1),
        nodes,
        workflows,
        bind: opts
            .bind
            .or(file.server.bind.clone())
            .unwrap_or_else(|| DEFAULT_BIND.into()),
        read_only: opts.read_only || file.server.read_only.unwrap_or(false),
        config_path: path.map(abs),
        data_dir: data_dir(),
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
        assert!(validate_workflow(&c.workflow[0], &["build-box".into()]).is_ok());
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
}
