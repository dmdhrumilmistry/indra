//! The console commands: `indra serve`, `indra ls`, `indra check`, `indra init`.
//!
//! View a harness's agents, skills and prompts (and edit them), see how they
//! connect, browse memory graphs, monitor runs, and launch agents and watch
//! them work. The indra security-review profile is the default; any harness
//! described by a few globs works too.

use crate::deck::{config, discover, nodes, runs, web, workflows};
use clap::{Args, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

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
    /// Do not let the console edit the config file (Settings page)
    #[arg(long)]
    lock_config: bool,
    /// Let the console edit the config file even when listening beyond this
    /// machine (editing runners is equivalent to running any command)
    #[arg(long)]
    allow_remote_config: bool,
    /// Offer a terminal in the console: your shell or Claude Code in a target,
    /// or an agent's own session. Loopback only; it is a shell on this machine.
    #[arg(long)]
    shell: bool,
}

#[derive(Args)]
pub struct ConsoleArgs {
    /// The console's address (default: the one `indra serve` runs on this machine)
    #[arg(long, env = "INDRA_URL")]
    url: Option<String>,
    /// Its access token (default: read from the running console's console.json)
    #[arg(long, env = "INDRA_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

#[derive(Args)]
pub struct AgentsArgs {
    #[command(flatten)]
    console: ConsoleArgs,
    /// Include agents that have finished
    #[arg(long, short)]
    all: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub struct AttachArgs {
    /// Process id (default: one waiting for an answer, else the newest running)
    id: Option<u64>,
    #[command(flatten)]
    console: ConsoleArgs,
}

fn client_runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
}

pub fn run_agents(a: AgentsArgs) -> Result<ExitCode, String> {
    let c = crate::deck::attach::find(a.console.url, a.console.token)?;
    client_runtime()?.block_on(crate::deck::attach::list(&c, a.all, a.json))?;
    Ok(ExitCode::SUCCESS)
}

pub fn run_attach(a: AttachArgs) -> Result<ExitCode, String> {
    let c = crate::deck::attach::find(a.console.url, a.console.token)?;
    let rt = client_runtime()?;
    rt.block_on(crate::deck::attach::attach(c, a.id))?;
    // Do not wait for the blocking stdin reader.
    rt.shutdown_background();
    Ok(ExitCode::SUCCESS)
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

fn load_options(
    opts: DeckOpts,
    bind: Option<String>,
    read_only: bool,
) -> config::Result<config::LoadOptions> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    Ok(config::LoadOptions {
        config: opts.config,
        targets: opts.targets,
        bind,
        read_only,
        cwd,
    })
}

fn load(opts: DeckOpts, bind: Option<String>, read_only: bool) -> config::Result<config::Settings> {
    config::load(load_options(opts, bind, read_only)?)
}

/// How the console may edit the config file.
struct Editing {
    opts: config::LoadOptions,
    lock: bool,
    allow_remote: bool,
    shell: bool,
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
    let opts = load_options(a.opts, a.bind, a.read_only)?;
    let s = config::load(opts.clone())?;
    let editing = Editing {
        opts,
        lock: a.lock_config,
        allow_remote: a.allow_remote_config,
        shell: a.shell,
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(serve(s, a.token, a.open, editing))?;
    Ok(ExitCode::SUCCESS)
}

fn new_token() -> Result<String, String> {
    let mut b = [0u8; 24];
    getrandom::fill(&mut b).map_err(|e| format!("no OS randomness for the access token: {}", e))?;
    Ok(b.iter().map(|x| format!("{:02x}", x)).collect())
}

async fn serve(
    s: config::Settings,
    token: Option<String>,
    open: bool,
    editing: Editing,
) -> Result<(), String> {
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
    let config_path = config::edit_path(&editing.opts, s.config_path.as_deref());
    let locked = if s.read_only {
        Some("indra is running read-only".to_string())
    } else if editing.lock {
        Some("indra serve was started with --lock-config".to_string())
    } else if !loopback && !editing.allow_remote {
        Some("the console listens beyond this machine; start it with --allow-remote-config to edit settings from a browser".to_string())
    } else {
        None
    };
    println!(
        "  settings {}  ({})",
        config_path.display(),
        match &locked {
            None if config_path.is_file() => "editable in the console",
            None => "created when you first save in the console",
            Some(_) => "locked",
        }
    );
    if editing.shell {
        if !loopback {
            return Err(
                "--shell is offered on a loopback address only (it is a shell on this machine)"
                    .into(),
            );
        }
        if s.read_only {
            return Err("--shell cannot be used with --read-only".into());
        }
        println!(
            "  terminal on (shell, Claude Code and agent sessions; logged to logs/terminal.log)"
        );
    }
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
    let app = web::App::with_config(
        s,
        token,
        loopback,
        Some(web::ConfigAccess {
            opts: editing.opts,
            path: config_path,
            locked,
            saving: std::sync::Mutex::new(()),
        }),
    );
    app.shell
        .store(editing.shell, std::sync::atomic::Ordering::SeqCst);
    // Let `indra agents` / `indra attach` on this machine find the console.
    let discovery = crate::deck::attach::advertise(
        &app.settings().data_dir,
        &format!("http://{}", host),
        &app.token,
    );
    let (procs, engine) = (app.procs.clone(), app.engine.clone());
    // Keep the board moving: finished agents, scheduled imports, auto runs.
    let ticker = app.clone();
    tokio::spawn(async move {
        loop {
            crate::deck::board::tick(&ticker).await;
            crate::deck::knowledge::harvest(&ticker);
            crate::deck::delegate::tick(&ticker);
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    });
    tokio::select! {
        r = axum::serve(listener, web::router(app)) => r.map_err(|e| e.to_string()),
        _ = shutdown_signal() => {
            // Agents run in their own process groups, so a terminal's Ctrl-C
            // does not reach them: stop them here rather than orphan them.
            for r in engine.list().iter().filter(|r| r.status == "running") {
                engine.cancel(r.id);
            }
            let n = procs.running();
            if n > 0 {
                eprintln!("indra: stopping {} running agent(s)...", n);
            }
            procs.stop_all().await;
            if let Some(p) = discovery {
                crate::deck::attach::withdraw(&p);
            }
            Ok(())
        }
    }
}

/// Ctrl-C, or SIGTERM / SIGHUP on Unix (a service manager, a closed terminal).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).ok();
        let mut hup = signal(SignalKind::hangup()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async { match term.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => {}
            _ = async { match hup.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
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

// ---- nodes and orchestration ------------------------------------------------------

#[derive(Args)]
pub struct NodesArgs {
    #[command(flatten)]
    opts: DeckOpts,
    #[arg(long)]
    json: bool,
}

/// Check every node: reachable, indra version, CPUs, runners installed.
pub fn run_nodes(a: NodesArgs) -> Result<ExitCode, String> {
    let s = load(a.opts, None, false)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let mut rows: Vec<(String, String, Result<nodes::NodeInfo, String>, usize)> = vec![(
        nodes::LOCAL.into(),
        "this machine".into(),
        Ok(nodes::local_info(&s)),
        s.local_max_parallel,
    )];
    rt.block_on(async {
        let mut set = tokio::task::JoinSet::new();
        // A node that dials in has no SSH to probe; its state is the hub's.
        for (i, n) in s
            .nodes
            .iter()
            .cloned()
            .enumerate()
            .filter(|(_, n)| !n.ssh.is_empty())
        {
            set.spawn(async move { (i, nodes::remote_info(&n).await) });
        }
        let mut res: Vec<(usize, Result<nodes::NodeInfo, String>)> = Vec::new();
        while let Some(Ok(r)) = set.join_next().await {
            res.push(r);
        }
        res.sort_by_key(|r| r.0);
        for (i, r) in res {
            let n = &s.nodes[i];
            rows.push((n.name.clone(), n.ssh.clone(), r, n.max_parallel));
        }
    });
    let failed = rows.iter().filter(|r| r.2.is_err()).count();
    let dial_in: Vec<&nodes::Node> = s.nodes.iter().filter(|n| n.ssh.is_empty()).collect();
    if a.json {
        let mut v: Vec<_> = rows
            .iter()
            .map(|(n, d, r, m)| match r {
                Ok(i) => serde_json::json!({ "name": n, "at": d, "max_parallel": m, "info": i }),
                Err(e) => serde_json::json!({ "name": n, "at": d, "max_parallel": m, "error": e }),
            })
            .collect();
        v.extend(dial_in.iter().map(|n| serde_json::json!({ "name": n.name, "dials_in": true, "max_parallel": n.max_parallel })));
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        for (n, d, r, m) in &rows {
            match r {
                Ok(i) => {
                    let runners: Vec<&str> = i
                        .runners
                        .iter()
                        .filter(|x| x.available)
                        .map(|x| x.name.as_str())
                        .collect();
                    println!(
                        "{:<14} ok    {}  ({} {}, {} cpus, indra {}, max {} agents)  runners: {}",
                        n,
                        if i.hostname.is_empty() {
                            d.as_str()
                        } else {
                            i.hostname.as_str()
                        },
                        i.os,
                        i.arch,
                        i.cpus,
                        i.version,
                        m,
                        if runners.is_empty() {
                            "none".to_string()
                        } else {
                            runners.join(", ")
                        }
                    );
                }
                Err(e) => println!("{:<14} DOWN  {}  {}", n, d, e),
            }
        }
        for n in &dial_in {
            println!(
                "{:<14} link  dials in to the hub (no SSH): see Nodes in the console, or `indra node connect` on it",
                n.name
            );
        }
    }
    Ok(if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

#[derive(Args)]
pub struct RunArgs {
    #[command(flatten)]
    opts: DeckOpts,
    /// Workflow name from the config; omit to list them
    name: Option<String>,
}

fn clock() -> String {
    crate::mem::time::format_unix(runs::now())[11..19].to_string()
}

/// Run a workflow and follow it in the terminal. Exit code 0 only when every
/// task succeeded; Ctrl-C cancels the run (stopping its agents) and waits.
pub fn run_workflow(a: RunArgs) -> Result<ExitCode, String> {
    let s = load(a.opts, None, false)?;
    let Some(name) = a.name else {
        if s.workflows.is_empty() {
            println!("no workflows configured; add [[workflow]] to indra.toml (see `indra init`)");
        }
        for w in &s.workflows {
            println!(
                "{:<20} {} task(s)  {}",
                w.name,
                w.tasks.len(),
                w.description
            );
        }
        return Ok(ExitCode::SUCCESS);
    };
    let s = std::sync::Arc::new(s);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let procs = crate::deck::procs::Procs::new(s.data_dir.join("logs"));
        let engine = workflows::Engine::new(procs.clone(), config::Shared::new((*s).clone()));
        let id = engine.start_named(&name)?;
        println!("{} workflow '{}' started", clock(), name);
        let mut seen: std::collections::HashMap<String, (&'static str, u32)> =
            std::collections::HashMap::new();
        let mut cancelled = false;
        let run = loop {
            let r = engine.get(id).ok_or("workflow run vanished")?;
            for t in &r.tasks {
                let now = (t.state, t.attempts);
                if seen.get(&t.id) != Some(&now) {
                    seen.insert(t.id.clone(), now);
                    if t.state == "waiting" {
                        continue;
                    }
                    let mut line = format!("{} {:<18} {:<12} {}", clock(), t.id, t.node, t.state);
                    if let Some(pid) = t.proc_id.filter(|_| t.state == "running") {
                        line.push_str(&format!("  (process #{}, attempt {})", pid, t.attempts));
                    }
                    if let Some(e) = t.error.as_ref().filter(|_| t.state != "running") {
                        line.push_str(&format!("  - {}", e));
                    }
                    println!("{}", line);
                }
            }
            if r.status != "running" {
                break r;
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(workflows::TICK_MS)) => {}
                _ = tokio::signal::ctrl_c(), if !cancelled => {
                    cancelled = true;
                    eprintln!("{} cancelling: stopping running agents...", clock());
                    engine.cancel(id);
                }
            }
        };
        println!("\n{} workflow '{}' {}", clock(), name, run.status);
        for t in &run.tasks {
            let log = t
                .proc_id
                .and_then(|p| procs.get(p))
                .map(|p| p.view().log)
                .unwrap_or_default();
            println!("  {:<18} {:<12} {:<10} {}", t.id, t.node, t.state, log);
        }
        Ok::<ExitCode, String>(if run.status == "succeeded" {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    })
}

#[derive(Subcommand)]
pub enum NodeCmd {
    /// Print this machine's facts as JSON
    Info,
    /// List run directories described by a JSON spec on stdin
    Runs,
    /// Run one agent job given as JSON on stdin; stops it when stdin closes
    Exec,
    /// Dial in to a hub and run its jobs here, reconnecting as needed (no SSH)
    Connect {
        /// The hub's console URL, e.g. https://hub.example:7878
        hub: String,
        /// This node's name in the hub's config
        #[arg(long)]
        name: String,
        /// One-time enrolment code from the hub (Nodes > Add a machine); only the first time
        #[arg(long)]
        code: Option<String>,
        /// Codebases on this machine the hub may run agents in (repeatable)
        #[arg(long = "target")]
        targets: Vec<String>,
        /// Never use the direct WebRTC path; relay everything through the hub
        #[arg(long)]
        no_webrtc: bool,
    },
}

/// `indra node ...`: the remote side of a node. The hub runs these over SSH.
#[derive(Args)]
pub struct NodeArgs {
    #[command(subcommand)]
    cmd: NodeCmd,
    /// Config file on this machine (its runners are the only ones it will run)
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
}

pub fn run_node(a: NodeArgs) -> Result<ExitCode, String> {
    let opts = DeckOpts {
        config: a.config,
        targets: Vec::new(),
    };
    match a.cmd {
        NodeCmd::Info => nodes::cmd_info(&load(opts, None, false)?),
        NodeCmd::Runs => nodes::cmd_runs(),
        NodeCmd::Exec => nodes::cmd_exec(&load(opts, None, false)?),
        NodeCmd::Connect {
            hub,
            name,
            code,
            targets,
            no_webrtc,
        } => {
            let s = load(opts, None, false)?;
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let targets = targets
                .iter()
                .map(|t| {
                    let p = config::expand(&cwd, t);
                    std::fs::canonicalize(&p).unwrap_or(p).display().to_string()
                })
                .collect();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            rt.block_on(crate::deck::link::connect(
                s,
                crate::deck::link::ConnectOpts {
                    hub,
                    name,
                    code,
                    targets,
                    webrtc: !no_webrtc,
                },
            ))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[derive(Args)]
pub struct SecretArgs {
    #[command(subcommand)]
    cmd: SecretCmd,
}

#[derive(Subcommand)]
pub enum SecretCmd {
    /// Set a secret; the value is read from stdin (never from an argument)
    Set {
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// Only these profiles may receive it (repeatable; default: any)
        #[arg(long = "profile")]
        profiles: Vec<String>,
        /// Only these nodes may receive it (repeatable; default: any)
        #[arg(long = "node")]
        nodes: Vec<String>,
    },
    /// List secrets: names, scopes and use, never values
    List {
        #[arg(long)]
        json: bool,
    },
    /// Delete a secret
    Rm { name: String },
}

pub fn run_secret(a: SecretArgs) -> Result<ExitCode, String> {
    let vault = crate::deck::secrets::Vault::open(config::data_dir().join("secrets"));
    match a.cmd {
        SecretCmd::Set {
            name,
            description,
            profiles,
            nodes,
        } => {
            use std::io::Read;
            let mut value = String::new();
            std::io::stdin()
                .read_to_string(&mut value)
                .map_err(|e| e.to_string())?;
            let value = value.trim_end_matches(['\n', '\r']).to_string();
            vault.set(
                &name,
                crate::deck::secrets::Change {
                    value: (!value.is_empty()).then_some(value),
                    description,
                    profiles: (!profiles.is_empty()).then_some(profiles),
                    nodes: (!nodes.is_empty()).then_some(nodes),
                },
            )?;
            println!(
                "secret '{}' saved; reference it as secret:{} in a profile's env",
                name, name
            );
        }
        SecretCmd::List { json } => {
            let list = vault.list()?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?
                );
            } else if list.is_empty() {
                println!("no secrets");
            } else {
                for s in list {
                    let scope = |v: &Vec<String>| {
                        if v.is_empty() {
                            "any".to_string()
                        } else {
                            v.join(",")
                        }
                    };
                    println!(
                        "{:<24} profiles: {:<16} nodes: {:<16} used {} times  {}",
                        s.name,
                        scope(&s.profiles),
                        scope(&s.nodes),
                        s.uses,
                        s.description
                    );
                }
            }
        }
        SecretCmd::Rm { name } => {
            if !vault.remove(&name)? {
                return Err(format!("no secret named '{}'", name));
            }
            println!("deleted '{}'", name);
        }
    }
    Ok(ExitCode::SUCCESS)
}
