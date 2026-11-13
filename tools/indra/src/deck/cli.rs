//! The console commands: `indra serve`, `indra ls`, `indra check`, `indra init`.
//!
//! View a harness's agents, skills and prompts (and edit them), see how they
//! connect, browse memory graphs, monitor runs, and launch agents and watch
//! them work. The security-harness profile is the default; any harness
//! described by a few globs works too.

use crate::deck::{config, discover, procs, runs, web};
use clap::Args;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

/// Which harnesses to load. Shared by every console command.
#[derive(Args)]
pub struct DeckOpts {
    /// Config file (default: ./indra.toml, then ~/.config/indra/indra.toml)
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Codebase whose runs to monitor and where agents run (repeatable; default: .)
    #[arg(long = "target", value_name = "DIR")]
    targets: Vec<PathBuf>,
}

#[derive(Args)]
pub struct ServeArgs {
    #[command(flatten)]
    opts: DeckOpts,
    /// Address to listen on (default 127.0.0.1:7878; port 0 picks a free one)
    #[arg(long)]
    bind: Option<String>,
    /// Disable launching agents and editing prompts
    #[arg(long)]
    read_only: bool,
    /// Use this access token instead of a random one (or set INDRA_TOKEN)
    #[arg(long, env = "INDRA_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Open the console in the default browser
    #[arg(long)]
    open: bool,
}

#[derive(Args)]
pub struct LsArgs {
    #[command(flatten)]
    opts: DeckOpts,
    /// Also list every agent, skill and prompt
    #[arg(long)]
    defs: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub struct CheckArgs {
    #[command(flatten)]
    opts: DeckOpts,
}

#[derive(Args)]
pub struct InitArgs {
    #[arg(default_value = "indra.toml")]
    path: PathBuf,
    #[arg(long)]
    force: bool,
}

fn load(opts: DeckOpts, bind: Option<String>, read_only: bool) -> config::Result<config::Settings> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    config::load(config::LoadOptions {
        config: opts.config,
        targets: opts.targets,
        bind,
        read_only,
        cwd,
    })
}

pub fn init(a: InitArgs) -> Result<ExitCode, String> {
    if a.path.exists() && !a.force {
        return Err(format!(
            "{} exists; pass --force to overwrite",
            a.path.display()
        ));
    }
    std::fs::write(&a.path, config::SAMPLE)
        .map_err(|e| format!("cannot write {}: {}", a.path.display(), e))?;
    println!("wrote {}", a.path.display());
    Ok(ExitCode::SUCCESS)
}

pub fn run_ls(a: LsArgs) -> Result<ExitCode, String> {
    let s = load(a.opts, None, false)?;
    ls(&s, a.defs, a.json);
    Ok(ExitCode::SUCCESS)
}

pub fn run_check(a: CheckArgs) -> Result<ExitCode, String> {
    let s = load(a.opts, None, false)?;
    Ok(check(&s))
}

pub fn run_serve(a: ServeArgs) -> Result<ExitCode, String> {
    let s = load(a.opts, a.bind, a.read_only)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(serve(s, a.token, a.open))?;
    Ok(ExitCode::SUCCESS)
}

fn new_token() -> Result<String, String> {
    let mut b = [0u8; 24];
    getrandom::fill(&mut b).map_err(|e| format!("no OS randomness for the access token: {}", e))?;
    Ok(b.iter().map(|x| format!("{:02x}", x)).collect())
}

async fn serve(s: config::Settings, token: Option<String>, open: bool) -> Result<(), String> {
    let token = match token.filter(|t| !t.is_empty()) {
        Some(t) if t.len() < 16 => return Err("--token must be at least 16 characters".into()),
        Some(t) => t,
        None => new_token()?,
    };
    let listener = tokio::net::TcpListener::bind(&s.bind)
        .await
        .map_err(|e| format!("cannot listen on {}: {}", s.bind, e))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let loopback = addr.ip().is_loopback();
    let host = if loopback {
        if addr.is_ipv6() {
            format!("[::1]:{}", addr.port())
        } else {
            format!("127.0.0.1:{}", addr.port())
        }
    } else {
        addr.to_string()
    };
    let url = format!("http://{}/?token={}", host, token);

    println!("indra {}", env!("CARGO_PKG_VERSION"));
    for h in &s.harnesses {
        println!("  harness  {}  ({})", h.name, h.root.display());
    }
    if s.harnesses.is_empty() {
        println!("  no harness found: run inside a project, pass --target, or write one with `indra init`");
    }
    let available: Vec<&str> = s
        .runners
        .iter()
        .filter(|r| r.program.is_some())
        .map(|r| r.name.as_str())
        .collect();
    println!(
        "  runners  {}",
        if s.read_only {
            "disabled (read-only)".to_string()
        } else if available.is_empty() {
            "none installed".to_string()
        } else {
            available.join(", ")
        }
    );
    if !loopback {
        eprintln!("indra: WARNING listening on {} - anyone who can reach it and has the token can run agents on this machine", addr);
    }
    println!(
        "\nOpen: {}\n(the token in this URL is the password; keep it private)",
        url
    );

    if open {
        open_browser(&url);
    }
    let app = Arc::new(web::App {
        procs: procs::Procs::new(s.data_dir.join("logs")),
        settings: s,
        token,
        loopback,
    });
    axum::serve(listener, web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}

fn open_browser(url: &str) {
    let mut c = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = c
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn ls(s: &config::Settings, with_defs: bool, json: bool) {
    let mut out = Vec::new();
    for h in &s.harnesses {
        let defs = discover::discover(h);
        let runs = runs::list(h, 20);
        if json {
            out.push(serde_json::json!({
                "name": h.name, "root": h.root.display().to_string(), "builtin": h.builtin,
                "definitions": if with_defs { serde_json::json!(defs) } else { serde_json::json!(defs.len()) },
                "runs": runs,
            }));
            continue;
        }
        let n = |k: discover::Kind| defs.iter().filter(|d| d.kind == k).count();
        println!("{}{}", h.name, if h.builtin { "  (built-in)" } else { "" });
        println!("  root     {}", h.root.display());
        println!(
            "  defs     {} agents, {} skills, {} prompts",
            n(discover::Kind::Agent),
            n(discover::Kind::Skill),
            n(discover::Kind::Prompt)
        );
        if with_defs {
            for d in &defs {
                let model = d
                    .model
                    .as_deref()
                    .map(|m| format!(" [{}]", m))
                    .unwrap_or_default();
                println!(
                    "    {:<7} {:<26}{}  {}",
                    d.kind.as_str(),
                    d.name,
                    model,
                    crate::mem::search::truncate(&d.description, 70)
                );
            }
        }
        if h.runs.is_some() {
            println!(
                "  runs     {}",
                if runs.is_empty() {
                    "none yet".to_string()
                } else {
                    format!("{} (newest first)", runs.len())
                }
            );
            for r in runs.iter().take(10) {
                let stages: String = r
                    .stages
                    .iter()
                    .map(|st| match st.state {
                        "done" => '#',
                        "running" => '>',
                        _ => '.',
                    })
                    .collect();
                println!(
                    "    {:<24} [{}] {:>3} results{}",
                    r.name,
                    stages,
                    r.counts.total,
                    if r.active { "  LIVE" } else { "" }
                );
            }
        }
        println!();
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    }
}

fn check(s: &config::Settings) -> ExitCode {
    let mut problems = 0;
    match &s.config_path {
        Some(p) => println!("config    {}", p.display()),
        None => println!("config    none (built-in profiles only)"),
    }
    for h in &s.harnesses {
        println!("harness   {}", h.name);
        for (label, globs) in [
            ("agents", &h.agents),
            ("skills", &h.skills),
            ("prompts", &h.prompts),
        ] {
            for g in globs.iter() {
                let n = discover::glob(&h.root, g).len();
                println!(
                    "  {:<8} {:<40} {} match{}",
                    label,
                    g,
                    n,
                    if n == 1 { "" } else { "es" }
                );
            }
        }
        for t in &h.targets {
            if !t.is_dir() {
                println!("  PROBLEM  target is not a directory: {}", t.display());
                problems += 1;
            }
        }
        let names: Vec<String> = discover::discover(h).into_iter().map(|d| d.name).collect();
        for st in &h.stages {
            if let Some(a) = &st.agent {
                if !names.contains(a) {
                    println!(
                        "  PROBLEM  stage '{}' names agent '{}', which was not found",
                        st.name, a
                    );
                    problems += 1;
                }
            }
        }
    }
    for r in &s.runners {
        match &r.program {
            Some(p) => println!("runner    {:<8} {}", r.name, p.display()),
            None => println!(
                "runner    {:<8} not installed ('{}' is not on PATH)",
                r.name, r.command
            ),
        }
    }
    if problems > 0 {
        println!("{} problem(s)", problems);
        ExitCode::FAILURE
    } else {
        println!("ok");
        ExitCode::SUCCESS
    }
}
