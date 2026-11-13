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
    /// memex vaults to show as memory graphs. `{target}` expands per target.
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
}

pub const DEFAULT_BIND: &str = "127.0.0.1:7878";

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

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

/// Logs and other state agentdeck writes, per user.
pub fn data_dir() -> PathBuf {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let h = home().unwrap_or_else(|| PathBuf::from("."));
    if cfg!(windows) {
        return env("LOCALAPPDATA").unwrap_or(h).join("agentdeck");
    }
    if cfg!(target_os = "macos") {
        return h
            .join("Library")
            .join("Application Support")
            .join("agentdeck");
    }
    env("XDG_DATA_HOME")
        .unwrap_or_else(|| h.join(".local").join("share"))
        .join("agentdeck")
}

/// Where a config file is looked for when `--config` is not given.
pub fn default_config_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut v = vec![cwd.join("agentdeck.toml")];
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(x) = env("XDG_CONFIG_HOME").or_else(|| home().map(|h| h.join(".config"))) {
        v.push(x.join("agentdeck").join("agentdeck.toml"));
    }
    if cfg!(windows) {
        if let Some(a) = env("APPDATA") {
            v.push(a.join("agentdeck").join("agentdeck.toml"));
        }
    }
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

/// The security-harness plugin directory: in this repository (walking up
/// from `cwd`), or as installed by Claude Code under `~/.claude/plugins`.
pub fn find_security_harness(cwd: &Path) -> Option<PathBuf> {
    let looks_right = |d: &Path| d.join("agents").is_dir() && d.join("skills").is_dir();
    let mut dir = cwd.to_path_buf();
    loop {
        let cand = dir.join("plugins").join("security-harness");
        if looks_right(&cand) {
            return Some(cand);
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
            if e.file_name() == "security-harness" && looks_right(&p) {
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
    let global_memory = memex::vault::locate(None);
    Harness {
        name: "security-harness".into(),
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
    };
    vec![
        r(
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
        ),
        r("codex", "codex", &["exec", "{prompt}"], "text"),
        r("gemini", "gemini", &["-p", "{prompt}"], "text"),
    ]
}

fn default_presets() -> Vec<PresetCfg> {
    let p = |name: &str, prompt: &str| PresetCfg {
        name: name.into(),
        prompt: prompt.into(),
        harness: Some("security-harness".into()),
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
        if !crate::discover::discover(&cc).is_empty() {
            builtins.push(cc);
        }
        for b in builtins {
            if !harnesses.iter().any(|h| h.name == b.name) {
                harnesses.push(b);
            }
        }
    }
    // The security harness is the default view when it is present.
    harnesses.sort_by_key(|h| (h.name != "security-harness", h.builtin));

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
        runners.push(Runner {
            program: which(&r.command),
            name: r.name,
            command: r.command,
            args: r.args,
            format,
        });
    }

    let mut presets = file.preset.clone();
    for d in default_presets() {
        if !presets.iter().any(|p| p.name == d.name) {
            presets.push(d);
        }
    }

    Ok(Settings {
        harnesses,
        runners,
        presets,
        bind: opts
            .bind
            .or(file.server.bind.clone())
            .unwrap_or_else(|| DEFAULT_BIND.into()),
        read_only: opts.read_only || file.server.read_only.unwrap_or(false),
        config_path: path.map(abs),
        data_dir: data_dir(),
    })
}

/// The commented starter file `agentdeck init` writes.
pub const SAMPLE: &str = r#"# agentdeck configuration.
#
# Two harness profiles are built in and detected automatically:
#   security-harness  - this repository's plugin (or the installed plugin)
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
# memory      = ["{target}/.memory", "~/notes"]      # memex vaults to draw as graphs
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

# Presets are saved prompts on the Launch page.
# [[preset]]
# name    = "Nightly review"
# harness = "security-harness"
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
