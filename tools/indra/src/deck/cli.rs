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
    // A console runs for weeks: look for a new release once a day here too.
    tokio::spawn(async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            crate::update::maybe_check();
        }
    });
    let loopback = addr.ip().is_loopback();
    let host = if loopback {
        if addr.is_ipv6() {
            format!("[::1]:{}", addr.port())
        } else {
            format!("127.0.0.1:{}", addr.port())
        }
    } else if addr.ip().is_unspecified() {
        // Listening on every address: show the one other machines use.
        match web::lan_ip() {
            Some(std::net::IpAddr::V6(ip)) => format!("[{}]:{}", ip, addr.port()),
            Some(ip) => format!("{}:{}", ip, addr.port()),
            None => addr.to_string(),
        }
    } else {
        addr.to_string()
    };
    let url = format!("http://{}/?token={}", host, token);
    let base_url = format!("http://{}/", host);

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
    let join_locked = if s.read_only {
        Some("indra is running read-only".to_string())
    } else if editing.lock {
        Some("indra serve was started with --lock-config".to_string())
    } else {
        None
    };
    let locked = if join_locked.is_some() {
        join_locked.clone()
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
    let mg = crate::deck::managed::view();
    if let Some(e) = mg["error"].as_str() {
        eprintln!(
            "indra: WARNING the organization's settings are not applied: {}",
            e
        );
    } else if mg["managed"] == true || mg["defaults"] == true {
        println!(
            "  org      {}  ({}{})",
            mg["dir"].as_str().unwrap_or(""),
            [
                ("defaults.toml", mg["defaults"] == true),
                ("managed.toml", mg["managed"] == true)
            ]
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(" and "),
            mg["enforced"]
                .as_array()
                .filter(|a| !a.is_empty())
                .map(|a| format!(
                    "; enforces {}",
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .unwrap_or_default()
        );
    }
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
    // With users, people sign in; the token stays a key for tools only.
    let has_users = crate::deck::users::Users::open(&s.data_dir).any();
    let url = if has_users {
        println!(
            "\nOpen: {}\n(sign in with your indra user; `indra user add` makes one)",
            base_url
        );
        base_url.clone()
    } else {
        println!(
            "\nOpen: {}\n(the token in this URL is the password; keep it private. `indra user add NAME` switches to sign-in with users and roles)",
            url
        );
        url
    };

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
            join_locked,
            saving: std::sync::Mutex::new(()),
        }),
    );
    // Workflows with a schedule run by themselves while the console runs.
    web::start_scheduler(app.clone());
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
    /// A workflow input, e.g. --input pr=123 (repeatable; `indra run NAME --help`
    /// lists them when you run it without the required ones)
    #[arg(long = "input", value_name = "NAME=VALUE")]
    inputs: Vec<String>,
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
            if !w.inputs.is_empty() {
                let ins: Vec<String> = w
                    .inputs
                    .iter()
                    .map(|i| format!("{}{}", i.name, if i.required { "*" } else { "" }))
                    .collect();
                println!(
                    "{:<20} --input {}=...  (* required)",
                    "",
                    ins.join("=... --input ")
                );
            }
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
        let shared = config::Shared::new((*s).clone());
        procs.set_accounts(std::sync::Arc::new(crate::deck::accounts::Accounts::new(
            shared.clone(),
            s.data_dir.clone(),
        )));
        let engine = workflows::Engine::new(procs.clone(), shared);
        let mut given = std::collections::BTreeMap::new();
        for kv in &a.inputs {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| format!("--input takes NAME=VALUE, not {:?}", kv))?;
            given.insert(k.trim().to_string(), v.to_string());
        }
        let id = engine.start_with_inputs(&name, &given)?;
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
    /// One run's text files as JSON (the hub opens a copy of a remote run)
    Runfiles,
    /// Run one agent job given as JSON on stdin; stops it when stdin closes
    Exec,
    /// Keep this machine's link to its hub open and run its jobs (after `indra join`, no options needed)
    Connect {
        /// The hub's console URL (default: the hub this machine joined)
        hub: Option<String>,
        /// This node's name (default: the name it joined as)
        #[arg(long)]
        name: Option<String>,
        /// One-time code from the hub; only to join (`indra join` is simpler)
        #[arg(long)]
        code: Option<String>,
        /// Folders agents may work in here (repeatable; remembered)
        #[arg(long = "target")]
        targets: Vec<String>,
        /// Never use the direct WebRTC path; relay everything through the hub
        #[arg(long)]
        no_webrtc: bool,
        /// Start in the background and return (what log-in autostart runs)
        #[arg(long, hide = true)]
        background: bool,
    },
    /// Which hub this machine joined, and whether its link runs by itself
    Status,
    /// Keep `node connect` running, restarting it if it fails (background links use this)
    #[command(hide = true)]
    Supervise,
    /// The folders agents may work in here: list them, or set them (restarts the link)
    Folders {
        /// New folders (replace the current ones)
        dirs: Vec<String>,
    },
    /// Stop the link and forget the hub (remove the machine in the hub's console too)
    Leave,
}

/// `indra join`: add this machine to a hub with the code from its console.
#[derive(Args)]
pub struct JoinArgs {
    /// The hub's console URL, e.g. http://192.168.1.20:7878 (default: [join] hub in the settings)
    hub: Option<String>,
    /// A code from the console (Machines > Add a machine), or an enrollment key (default: [join] key)
    code: Option<String>,
    /// Join again even if this machine already joined that hub
    #[arg(long)]
    force: bool,
    /// Name for this machine (default: its host name)
    #[arg(long)]
    name: Option<String>,
    /// Folders agents may work in here (repeatable; default: the current folder)
    #[arg(long = "target")]
    targets: Vec<String>,
    /// Do not keep the link running in the background; stay in this terminal
    #[arg(long)]
    foreground: bool,
    /// Never use the direct WebRTC path; relay everything through the hub
    #[arg(long)]
    no_webrtc: bool,
    /// Config file on this machine (its runners are the ones it will run)
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
}

fn abs_targets(targets: &[String]) -> Result<Vec<String>, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    Ok(targets
        .iter()
        .map(|t| {
            let p = config::expand(&cwd, t);
            std::fs::canonicalize(&p).unwrap_or(p).display().to_string()
        })
        .map(|t| t.trim_start_matches(r"\\?\").to_string())
        .collect())
}

pub fn run_join(a: JoinArgs) -> Result<ExitCode, String> {
    let opts = DeckOpts {
        config: a.config.clone(),
        targets: Vec::new(),
    };
    let lo = load_options(opts, None, false)?;
    // An organization's settings may name the hub and key ([join], from MDM).
    let preset = config::load_file(config::config_path(&lo).as_deref())
        .map(|f| f.join)
        .unwrap_or_default();
    let s = config::load(lo)?;
    let hub = a.hub.clone().or(preset.hub.clone()).ok_or(
        "which hub? `indra join <hub-url> <code>` (the console's Machines page gives you both)",
    )?;
    let code =
        a.code.clone().or(preset.key.clone()).ok_or(
            "no code: `indra join <hub-url> <code>` (Machines > Add a machine gives you one)",
        )?;
    let hub = hub.trim_end_matches('/').to_string();
    if !(hub.starts_with("http://") || hub.starts_with("https://")) {
        return Err(format!(
            "'{}' is not the hub's URL: copy it from the console, e.g. http://192.168.1.20:7878",
            hub
        ));
    }
    let targets = if !a.targets.is_empty() {
        abs_targets(&a.targets)?
    } else if !preset.targets.is_empty() {
        abs_targets(&preset.targets)?
    } else {
        // The folder this runs in; from an MDM script that is often `/`,
        // so then the home folder.
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let here = if cwd.parent().is_none() {
            crate::paths::home().unwrap_or(cwd)
        } else {
            cwd
        };
        abs_targets(&[here.display().to_string()])?
    };
    // Run again (MDM policies do): already joined, so just make sure the
    // link runs.
    if !a.force {
        if let Some(sv) = crate::deck::link::saved().filter(|sv| sv.hub == hub) {
            println!(
                "This machine already joined {} as '{}' (`--force` joins again).",
                hub, sv.name
            );
            if !a.foreground {
                if let Ok(how) = crate::deck::autostart::install(a.config.as_deref()) {
                    println!("Its link runs as {}.", how);
                }
            }
            return Ok(ExitCode::SUCCESS);
        }
    }
    let info = nodes::local_info(&s);
    if a.foreground {
        // One connection: join and stay on it.
        println!("Joining {} and staying connected in this terminal (Ctrl+C stops it; `indra node connect` starts it again) ...", hub);
        return run_link(
            s,
            crate::deck::link::ConnectOpts {
                hub: Some(hub),
                name: a.name.clone(),
                code: Some(code.clone()),
                targets,
                webrtc: !a.no_webrtc,
                once: false,
            },
        );
    }
    println!("Joining {} ...", hub);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let opts = crate::deck::link::ConnectOpts {
        hub: Some(hub.clone()),
        name: a.name.clone(),
        code: Some(code.clone()),
        targets: targets.clone(),
        webrtc: !a.no_webrtc,
        once: true,
    };
    let joined = rt.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            crate::deck::link::connect(s.clone(), opts),
        )
        .await
    });
    match joined {
        Err(_) => {
            return Err(format!(
                "no answer from {} in a minute: is the console running, and can this machine reach that address? (try opening it in a browser here)",
                hub
            ))
        }
        Ok(r) => r?,
    }
    let saved = crate::deck::link::saved().ok_or("joined, but the key was not saved")?;
    let runners: Vec<String> = info
        .runners
        .iter()
        .map(|r| {
            if r.available {
                r.name.clone()
            } else {
                format!("{} (not installed)", r.name)
            }
        })
        .collect();
    println!("\nJoined {} as '{}'.", hub, saved.name);
    println!(
        "  machine  {} ({} {}, {} CPUs)",
        info.hostname, info.os, info.arch, info.cpus
    );
    println!(
        "  agents   {}",
        if runners.is_empty() {
            "none found".into()
        } else {
            runners.join(", ")
        }
    );
    println!("  folders  {}", saved.targets.join(", "));
    match crate::deck::autostart::install(a.config.as_deref()) {
        Ok(how) => {
            println!("  runs as  {}", how);
            println!(
                "\nDone: it shows up on the hub's Machines page. You can close this terminal."
            );
            println!("Check it with `indra node status`; undo with `indra node leave`.");
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("\nCould not keep it running in the background ({}); staying connected here instead (Ctrl+C stops it).", e);
            run_link(
                s,
                crate::deck::link::ConnectOpts {
                    webrtc: !a.no_webrtc,
                    ..Default::default()
                },
            )
        }
    }
}

fn run_link(s: config::Settings, o: crate::deck::link::ConnectOpts) -> Result<ExitCode, String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    match rt.block_on(crate::deck::link::connect(s, o)) {
        // Removed on the hub: nothing to do, and no restart (see autostart).
        Err(e) if e.contains("was removed from") => {
            eprintln!("indra: {}", e);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => Err(e),
        Ok(()) => Ok(ExitCode::SUCCESS),
    }
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

/// `indra accounts`: what each account (and each machine's own login) used.
#[derive(Args)]
pub struct AccountsArgs {
    #[command(flatten)]
    opts: DeckOpts,
    /// Print JSON
    #[arg(long)]
    json: bool,
}

pub fn run_accounts(a: AccountsArgs) -> Result<ExitCode, String> {
    let s = load(a.opts, None, false)?;
    let data = s.data_dir.clone();
    let acc = crate::deck::accounts::Accounts::new(config::Shared::new(s), data);
    let v = acc.view(crate::deck::runs::now());
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(ExitCode::SUCCESS);
    }
    let rows = v["accounts"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("No accounts and no runs yet: agents use each machine's own CLI login.");
        println!("Add [[account]] in indra.toml (or Settings > Accounts) to run them with credentials from here.");
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{:<24} {:<8} {:>6} {:>10} {:>10}  STATUS",
        "ACCOUNT", "AGENT", "RUNS", "TODAY", "MONTH"
    );
    for r in rows {
        let name = if r["login"] == true {
            format!(
                "login ({})",
                r["key"].as_str().unwrap_or("").trim_start_matches("login:")
            )
        } else {
            r["name"].as_str().unwrap_or("").to_string()
        };
        let lim = |k: &str| {
            r["limits"][k]
                .as_f64()
                .map(|x| format!("/{:.0}", x))
                .unwrap_or_default()
        };
        println!(
            "{:<24} {:<8} {:>6} {:>10} {:>10}  {}{}",
            name,
            r["agent"].as_str().unwrap_or(""),
            r["today"]["runs"].as_u64().unwrap_or(0),
            format!(
                "${:.2}{}",
                r["today"]["usd"].as_f64().unwrap_or(0.0),
                lim("daily_usd")
            ),
            format!(
                "${:.2}{}",
                r["month_usd"].as_f64().unwrap_or(0.0),
                lim("monthly_usd")
            ),
            r["status"].as_str().unwrap_or(""),
            r["why"]
                .as_str()
                .map(|w| format!(" ({})", w))
                .unwrap_or_default()
        );
    }
    println!(
        "\nrouting: {}",
        v["routing"]["strategy"].as_str().unwrap_or("")
    );
    Ok(ExitCode::SUCCESS)
}

/// `indra mcp`: the MCP server for designing workflows (stdio).
#[derive(Args)]
pub struct McpArgs {
    #[command(flatten)]
    opts: DeckOpts,
}

pub fn run_mcp(a: McpArgs) -> Result<ExitCode, String> {
    crate::deck::mcp::serve(load_options(a.opts, None, false)?)
}

pub fn run_node(a: NodeArgs) -> Result<ExitCode, String> {
    let opts = DeckOpts {
        config: a.config,
        targets: Vec::new(),
    };
    match a.cmd {
        NodeCmd::Info => nodes::cmd_info(&load(opts, None, false)?),
        NodeCmd::Runs => nodes::cmd_runs(),
        NodeCmd::Runfiles => nodes::cmd_run_files(),
        NodeCmd::Exec => nodes::cmd_exec(&load(opts, None, false)?),
        NodeCmd::Connect {
            hub,
            name,
            code,
            targets,
            no_webrtc,
            background,
        } => {
            if background {
                let pid = crate::deck::autostart::spawn_background(opts.config.as_deref())?;
                println!(
                    "indra node connect runs in the background (process {})",
                    pid
                );
                return Ok(ExitCode::SUCCESS);
            }
            let s = load(opts, None, false)?;
            run_link(
                s,
                crate::deck::link::ConnectOpts {
                    hub,
                    name,
                    code,
                    targets: abs_targets(&targets)?,
                    webrtc: !no_webrtc,
                    once: false,
                },
            )
        }
        NodeCmd::Supervise => crate::deck::autostart::supervise(opts.config.as_deref()),
        NodeCmd::Status => {
            let Some(sv) = crate::deck::link::saved() else {
                println!("This machine has not joined a hub. In the hub's console: Machines > Add a machine, then run the command it shows here.");
                return Ok(ExitCode::SUCCESS);
            };
            println!("hub      {}", sv.hub);
            println!("name     {}", sv.name);
            println!(
                "folders  {}",
                if sv.targets.is_empty() {
                    "none".to_string()
                } else {
                    sv.targets.join(", ")
                }
            );
            match crate::deck::autostart::status() {
                Some(how) => println!("runs as  {}", how),
                None => println!("runs as  nothing keeps it running: `indra node connect` (or `indra join` again for the background)"),
            }
            println!("logs     {}", crate::deck::autostart::log_path().display());
            println!("key      {}", crate::deck::link::saved_path().display());
            Ok(ExitCode::SUCCESS)
        }
        NodeCmd::Folders { dirs } => {
            let mut sv = crate::deck::link::saved()
                .ok_or("this machine has not joined a hub: `indra join <hub-url> <code>`")?;
            if dirs.is_empty() {
                if sv.targets.is_empty() {
                    println!("none: agents have no folder here yet (`indra node folders <dir>`)");
                }
                for t in &sv.targets {
                    println!("{}", t);
                }
                return Ok(ExitCode::SUCCESS);
            }
            sv.targets = abs_targets(&dirs)?;
            for t in &sv.targets {
                if !std::path::Path::new(t).is_dir() {
                    return Err(format!("{} is not a folder here", t));
                }
            }
            crate::deck::link::save(&sv)?;
            println!("Agents may work in: {}", sv.targets.join(", "));
            if crate::deck::autostart::status().is_some() {
                let how = crate::deck::autostart::install(opts.config.as_deref())?;
                println!(
                    "Restarted the link ({}); the hub shows the new folders.",
                    how
                );
            } else {
                println!("The next `indra node connect` uses them.");
            }
            Ok(ExitCode::SUCCESS)
        }
        NodeCmd::Leave => {
            let sv = crate::deck::link::saved();
            for d in crate::deck::autostart::uninstall() {
                println!("{}", d);
            }
            crate::deck::link::forget().map_err(|e| e.to_string())?;
            match sv {
                Some(sv) => println!(
                    "Left {} (this machine was '{}'). Remove it there too: Machines > Remove.",
                    sv.hub, sv.name
                ),
                None => println!("This machine had not joined a hub."),
            }
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
