//! `indra claude`: Claude Code, interactive, in this terminal, on a model
//! served by Ollama, or with only its subagents there.
//!
//! It checks the server and the model first (and can pull it), then starts
//! `claude` with the environment the console would use, through the model
//! router in this process, which keeps every credential off Ollama. The
//! router lives as long as this command does, so the command waits for
//! Claude Code and passes its exit code on.

use crate::deck::{config, nodes, ollama};
use clap::Args;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Args, Debug)]
#[command(after_help = "Examples:\n  \
    indra claude -m qwen3-coder                 Claude Code on a local model\n  \
    indra claude -m gpt-oss:20b --pull          pull it first if the server lacks it\n  \
    indra claude --ollama http://gpu:11434 -m qwen3-coder\n  \
    indra claude --subagents qwen3-coder        Claude on Anthropic, its subagents local\n  \
    indra claude --profile local -- --resume    a profile from indra.toml, then Claude Code's own flags")]
pub struct ClaudeArgs {
    /// The Ollama model to run (default: the profile's model)
    #[arg(long, short)]
    model: Option<String>,
    /// The Ollama server (default: the profile's, else http://localhost:11434)
    #[arg(long, value_name = "URL")]
    ollama: Option<String>,
    /// Keep the main agent on Anthropic and run only its subagents on this
    /// Ollama model
    #[arg(long, value_name = "MODEL", conflicts_with = "model")]
    subagents: Option<String>,
    /// Start from a [[profile]] in indra.toml: its model, provider, permission
    /// mode, tool rules and environment (secrets included)
    #[arg(long)]
    profile: Option<String>,
    /// Config file (default: ./indra.toml, then ~/.config/indra/indra.toml)
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Pull the model first if the server does not have it
    #[arg(long)]
    pull: bool,
    /// Skip the checks (server, model, tool calling, context size)
    #[arg(long)]
    no_check: bool,
    /// The context window to tell Claude Code (default: what the server runs
    /// the model with, else the smaller of its maximum and 65536)
    #[arg(long, value_name = "TOKENS")]
    context: Option<u64>,
    /// Print what would run (secrets hidden) instead of running it
    #[arg(long)]
    dry_run: bool,
    /// Arguments for Claude Code itself, after `--`
    #[arg(last = true)]
    args: Vec<String>,
}

pub fn run(a: ClaudeArgs) -> Result<ExitCode, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(go(a))
}

/// The settings when there is a config, else the built-ins.
fn settings(config_file: Option<PathBuf>) -> Result<config::Settings, String> {
    config::load(config::LoadOptions {
        config: config_file,
        targets: vec![],
        bind: None,
        read_only: true,
        cwd: std::env::current_dir().map_err(|e| e.to_string())?,
    })
}

async fn go(a: ClaudeArgs) -> Result<ExitCode, String> {
    let s = settings(a.config.clone())?;
    // The profile's options, then the flags on top.
    let mut o = match a.profile.as_deref() {
        Some(p) => s
            .profile(p)
            .ok_or_else(|| format!("no profile named '{}' in the config", p))?
            .opts(),
        None => nodes::AgentOpts::default(),
    };
    if let Some(u) = &a.ollama {
        o.ollama_url = Some(u.clone());
    }
    if let Some(m) = &a.model {
        o.model = Some(m.clone());
        o.provider = Some("ollama".into());
    }
    if let Some(m) = &a.subagents {
        o.subagent_provider = Some("ollama".into());
        o.env.insert("CLAUDE_CODE_SUBAGENT_MODEL".into(), m.clone());
    }
    let local = o.provider.as_deref() == Some("ollama");
    let sub = !local && o.subagent_provider.as_deref() == Some("ollama");
    if !local && !sub {
        return Err("name the Ollama model: `indra claude -m qwen3-coder` (or --subagents MODEL, or a --profile that runs on Ollama)".into());
    }
    o.validate()?;
    let url = o.ollama();
    let model = if local {
        o.model.clone()
    } else {
        o.env.get("CLAUDE_CODE_SUBAGENT_MODEL").cloned()
    }
    .filter(|m| !m.trim().is_empty())
    .ok_or("name the Ollama model with -m (or set the profile's model)")?;
    if !ollama::valid_model(&model) {
        return Err(format!("not a model name: {:?}", model));
    }

    let runner = s
        .runners
        .iter()
        .find(|r| r.format == "claude-stream-json")
        .ok_or("no Claude Code runner is configured")?;
    let program = runner
        .program
        .clone()
        .ok_or("Claude Code is not installed: `npm install -g @anthropic-ai/claude-code`")?;

    let mut context = a.context;
    if !a.no_check {
        let mut c = ollama::check(&url, &model).await;
        if c["reachable"] == true && c["present"] == false && a.pull {
            eprintln!("indra: pulling {} on {}...", model, url);
            let mut shown = String::new();
            ollama::pull(&url, &model, |v| {
                let st = v["status"].as_str().unwrap_or("");
                let line = match (v["completed"].as_u64(), v["total"].as_u64()) {
                    (Some(c), Some(t)) if t > 0 => format!("{} {}%", st, c * 100 / t),
                    _ => st.to_string(),
                };
                if line != shown {
                    eprint!("\r\x1b[2K  {}", line);
                    shown = line;
                }
            })
            .await?;
            eprintln!();
            c = ollama::check(&url, &model).await;
        }
        if c["ok"] != true {
            let mut e = c["error"]
                .as_str()
                .unwrap_or("Ollama check failed")
                .to_string();
            if c["present"] == false {
                e.push_str(" (or add --pull)");
            }
            return Err(e);
        }
        for w in c["warnings"].as_array().into_iter().flatten() {
            eprintln!("indra: warning: {}", w.as_str().unwrap_or(""));
        }
        context = context.or_else(|| c["context"].as_u64());
    }
    if local {
        if let Some(n) = context {
            o.env
                .entry("CLAUDE_CODE_MAX_CONTEXT_TOKENS".into())
                .or_insert_with(|| n.to_string());
        }
    }

    // The environment the console would give this agent, then its secrets.
    let mut args: Vec<String> = Vec::new();
    let mut flag = |f: &str, v: &Option<String>| {
        if let Some(v) = v {
            args.push(f.into());
            args.push(v.clone());
        }
    };
    flag("--model", &o.model);
    flag("--permission-mode", &o.permission_mode);
    flag("--effort", &o.effort);
    if !o.allowed_tools.is_empty() {
        args.push("--allowed-tools".into());
        args.push(o.allowed_tools.join(","));
    }
    if !o.disallowed_tools.is_empty() {
        args.push("--disallowed-tools".into());
        args.push(o.disallowed_tools.join(","));
    }
    let env: BTreeMap<String, String> = nodes::provider_env(runner, true, &o, &mut args)?;
    let vault = crate::deck::secrets::Vault::open(s.data_dir.join("secrets"));
    let (env, redactions) = vault.resolve(&env, a.profile.as_deref(), nodes::LOCAL)?;
    args.extend(a.args.iter().cloned());

    if a.dry_run {
        let shown: Vec<String> = env
            .iter()
            .map(|(k, v)| {
                let v = crate::deck::secrets::redact(v, &redactions);
                format!("{}={}", k, if v.is_empty() { "''".into() } else { v })
            })
            .collect();
        println!("{}", shown.join("\n"));
        println!("{} {}", program.display(), args.join(" "));
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!(
        "indra: Claude Code {} on {} at {} (credentials stay off it)",
        if local {
            "running"
        } else {
            "with its subagents"
        },
        model,
        url
    );
    // Ctrl-C belongs to Claude Code; this process must outlive it, because
    // the router it talks to lives here.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
    let status = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(&program);
        cmd.args(&args).envs(&env);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Claude Code gets Ctrl-C back.
            unsafe {
                cmd.pre_exec(|| {
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    Ok(())
                });
            }
        }
        cmd.status()
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("cannot start Claude Code: {}", e))?;
    Ok(ExitCode::from(
        status.code().unwrap_or(1).clamp(0, 255) as u8
    ))
}
