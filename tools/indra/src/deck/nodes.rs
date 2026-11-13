//! Nodes: the machines agents run on.
//!
//! `local` is always present. A remote node is reached over SSH, so there is
//! no daemon to install, no port to open, and authentication and encryption
//! are SSH's (keys, agents, `~/.ssh/config`, known_hosts). The remote needs
//! `indra` on its PATH; the hub only ever runs `indra node <info|runs|exec>`
//! there and passes everything else as JSON on stdin, so no prompt or path
//! ever goes through the remote shell.
//!
//! The remote runs only runners from its *own* configuration, by name, in a
//! directory that must exist on it, and kills the whole process tree when the
//! SSH session ends (stdin EOF).
//!
//! Protocol of `indra node exec` (one JSON object per line on stdout):
//!   {"t":"start","runner":..,"format":..,"pid":..,"host":..}
//!   {"t":"out","l":"<a stdout line of the agent>"}
//!   {"t":"err","l":"<a stderr line>"}
//!   {"t":"error","msg":".."}     the job could not start
//!   {"t":"exit","code":N}
//!
//! After the job line, the hub may send more lines on stdin for agents that
//! are driven interactively (Claude Code's control channel):
//!   {"t":"in","l":"<a line for the agent's stdin>"}
//!   {"t":"close"}                  close the agent's stdin (its work is done)
//! EOF on stdin still means "stop the agent".

use crate::deck::config::{self, Harness, Runner, Settings, StageCfg};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const LOCAL: &str = "local";

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub name: String,
    pub description: String,
    /// SSH destination: `host`, `user@host`, or a `Host` alias from ~/.ssh/config.
    pub ssh: String,
    pub port: Option<u16>,
    pub identity: Option<PathBuf>,
    /// The indra binary on the remote (default `indra`).
    pub indra: String,
    /// Extra arguments for every remote `indra node ...` call, e.g. `--config`.
    pub args: Vec<String>,
    pub max_parallel: usize,
    /// Codebases on the remote (remote paths).
    pub targets: Vec<String>,
    /// The ssh client to run (default `ssh`).
    pub ssh_command: String,
}

/// A destination is a host, `user@host` or an ssh_config alias. Refusing a
/// leading `-` and anything outside a conservative set means a config value
/// can never smuggle in an ssh option such as `-oProxyCommand=...`.
pub fn validate_destination(s: &str) -> Result<(), String> {
    let ok = !s.is_empty()
        && !s.starts_with('-')
        && s.len() <= 255
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._-:[]%".contains(c));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid ssh destination '{}': use host, user@host or an ssh_config alias",
            s
        ))
    }
}

/// Node names appear in URLs, logs and the UI.
pub fn validate_name(s: &str) -> Result<(), String> {
    let ok = !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid node name '{}': use letters, digits, '-' and '_'",
            s
        ))
    }
}

/// POSIX shell quoting for one word of the remote command. ssh joins its
/// arguments into a single string that the remote shell parses, so every
/// word is quoted unless it is plainly safe.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl Node {
    /// `(program, args)` that run `indra node <sub>` on this node over SSH.
    pub fn ssh_command_for(&self, sub: &str) -> Result<(PathBuf, Vec<String>), String> {
        if self.ssh.is_empty() {
            return Err(format!(
                "node '{}' has no ssh destination; it is reached when it connects (`indra node connect`)",
                self.name
            ));
        }
        let program = config::which(&self.ssh_command).ok_or_else(|| {
            format!(
                "node '{}': ssh client '{}' is not installed",
                self.name, self.ssh_command
            )
        })?;
        let mut args: Vec<String> = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=10".into(),
            "-o".into(),
            "ServerAliveInterval=30".into(),
            "-T".into(),
        ];
        if let Some(p) = self.port {
            args.push("-p".into());
            args.push(p.to_string());
        }
        if let Some(i) = &self.identity {
            args.push("-i".into());
            args.push(i.display().to_string());
        }
        args.push(self.ssh.clone());
        args.push("--".into());
        let words = std::iter::once(self.indra.as_str())
            .chain(["node", sub])
            .chain(self.args.iter().map(String::as_str));
        args.extend(words.map(shell_quote));
        Ok((program, args))
    }

    /// Run `indra node <sub>` remotely with `input` on stdin; its stdout.
    pub async fn call(&self, sub: &str, input: &str, timeout_secs: u64) -> Result<String, String> {
        let (program, args) = self.ssh_command_for(sub)?;
        let mut child = tokio::process::Command::new(program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("cannot run ssh: {}", e))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(input.as_bytes()).await;
            let _ = si.write_all(b"\n").await;
        }
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| {
            format!(
                "node '{}' did not answer within {}s",
                self.name, timeout_secs
            )
        })?
        .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let msg = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            return Err(if msg.is_empty() {
                format!("ssh to '{}' failed ({})", self.name, out.status)
            } else {
                msg
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

// ---- node facts ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerInfo {
    pub name: String,
    pub available: bool,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpus: usize,
    pub version: String,
    pub runners: Vec<RunnerInfo>,
}

fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname NUL-terminates or truncates.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..end]).into_owned();
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

pub fn local_info(s: &Settings) -> NodeInfo {
    NodeInfo {
        hostname: hostname(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        cpus: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        version: env!("CARGO_PKG_VERSION").into(),
        runners: s
            .runners
            .iter()
            .map(|r| RunnerInfo {
                name: r.name.clone(),
                available: r.program.is_some(),
                format: r.format.clone(),
            })
            .collect(),
    }
}

pub async fn remote_info(n: &Node) -> Result<NodeInfo, String> {
    let out = n.call("info", "{}", 20).await?;
    let line = out
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or("");
    serde_json::from_str(line).map_err(|_| {
        format!(
            "node '{}' did not return indra node info (is indra installed there?)",
            n.name
        )
    })
}

/// What `indra node runs` needs to list run directories on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunsSpec {
    pub targets: Vec<String>,
    pub runs: String,
    pub findings: Vec<String>,
    pub group_by: Option<String>,
    pub stages: Vec<StageCfg>,
}

impl RunsSpec {
    pub fn for_node(h: &Harness, targets: &[String]) -> Option<RunsSpec> {
        Some(RunsSpec {
            targets: targets.to_vec(),
            runs: h.runs.clone()?,
            findings: h.findings.clone(),
            group_by: h.group_by.clone(),
            stages: h.stages.clone(),
        })
    }
}

pub async fn remote_runs(n: &Node, spec: &RunsSpec) -> Result<Value, String> {
    let input = serde_json::to_string(spec).map_err(|e| e.to_string())?;
    let out = n.call("runs", &input, 30).await?;
    let line = out
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('['))
        .unwrap_or("[]");
    serde_json::from_str(line).map_err(|e| e.to_string())
}

// ---- the remote side: `indra node info|runs|exec` -----------------------------------

pub fn cmd_info(s: &Settings) -> Result<ExitCode, String> {
    println!(
        "{}",
        serde_json::to_string(&local_info(s)).map_err(|e| e.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

fn read_stdin_line() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(line)
}

fn expand_target(t: &str) -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    config::expand(&base, t)
}

pub fn cmd_runs() -> Result<ExitCode, String> {
    let spec: RunsSpec = serde_json::from_str(read_stdin_line()?.trim())
        .map_err(|e| format!("bad runs request: {}", e))?;
    let h = Harness {
        name: "remote".into(),
        description: String::new(),
        root: PathBuf::from("."),
        agents: vec![],
        skills: vec![],
        prompts: vec![],
        memory: vec![],
        targets: spec.targets.iter().map(|t| expand_target(t)).collect(),
        runs: Some(spec.runs),
        findings: spec.findings,
        group_by: spec.group_by,
        stages: spec.stages,
        builtin: false,
    };
    println!(
        "{}",
        serde_json::to_string(&crate::deck::runs::list(&h, 100)).map_err(|e| e.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub runner: String,
    pub prompt: String,
    pub target: String,
    /// Continue an earlier agent session on this node instead of starting fresh.
    #[serde(default)]
    pub resume: Option<Resume>,
    /// Model, permission mode and the rest, from the profile.
    #[serde(default)]
    pub opts: AgentOpts,
}

/// Continue an earlier agent session (a pivot), optionally as a branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resume {
    pub session: String,
    /// Branch the session instead of continuing it in place.
    #[serde(default)]
    pub fork: bool,
}

/// A session id becomes a command-line argument: keep it to the characters
/// real ids use, and never let it look like an option.
pub fn validate_session(s: &str) -> Result<(), String> {
    let ok = !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(format!("invalid session id '{}'", s))
    }
}

type Out = Arc<Mutex<std::io::Stdout>>;

fn emit(out: &Out, v: Value) {
    let mut o = out.lock().unwrap();
    let _ = writeln!(o, "{}", v);
    let _ = o.flush();
}

pub fn cmd_exec(s: &Settings) -> Result<ExitCode, String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let code = rt.block_on(exec(s));
    // A thread is still blocked reading stdin (the hub keeps it open for the
    // life of the job). Dropping the runtime normally would wait for it.
    rt.shutdown_background();
    code
}

/// Where a job's envelope lines go: stdout for `indra node exec`, the hub
/// link for `indra node connect`.
pub type Emit = Arc<dyn Fn(Value) + Send + Sync>;

/// What the hub sends a running job after it starts.
#[derive(Debug)]
pub enum JobInput {
    /// A line for the agent's stdin (interactive agents).
    Line(String),
    /// Close the agent's stdin: its work is done.
    Close,
    /// Stop the agent and its whole process tree.
    Stop,
}

async fn exec(s: &Settings) -> Result<ExitCode, String> {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let emit_out: Emit = {
        let out = out.clone();
        Arc::new(move |v: Value| emit(&out, v))
    };
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    if stdin
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?
        == 0
    {
        emit(&out, json!({ "t": "error", "msg": "no job on stdin" }));
        return Ok(ExitCode::from(2));
    }
    let job: Job = match serde_json::from_str(line.trim()) {
        Ok(j) => j,
        Err(e) => {
            emit(
                &out,
                json!({ "t": "error", "msg": format!("bad job: {}", e) }),
            );
            return Ok(ExitCode::from(2));
        }
    };
    // Stdin after the job: control lines, and EOF when the hub goes away.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut buf = String::new();
        loop {
            buf.clear();
            match stdin.read_line(&mut buf).await {
                Ok(0) | Err(_) => {
                    let _ = tx.send(JobInput::Stop);
                    return;
                }
                Ok(_) => {}
            }
            let Ok(msg) = serde_json::from_str::<Value>(buf.trim()) else {
                continue;
            };
            let input = match msg.get("t").and_then(Value::as_str) {
                Some("in") => msg
                    .get("l")
                    .and_then(Value::as_str)
                    .map(|l| JobInput::Line(l.to_string())),
                Some("close") => Some(JobInput::Close),
                _ => None,
            };
            if let Some(i) = input {
                if tx.send(i).is_err() {
                    return;
                }
            }
        }
    });
    let code = run_job(s, job, emit_out, rx).await;
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// Run one job on this machine: check it, start its runner, send its output
/// as envelope lines, take input, and stop its process tree on Stop. Returns
/// the exit code (2 when it could not start).
pub async fn run_job(
    s: &Settings,
    job: Job,
    out: Emit,
    mut input: tokio::sync::mpsc::UnboundedReceiver<JobInput>,
) -> i32 {
    let fail = |msg: String| {
        out(json!({ "t": "error", "msg": msg }));
        2
    };
    let Some(runner) = s.runners.iter().find(|r| r.name == job.runner) else {
        return fail(format!(
            "runner '{}' is not configured on {}",
            job.runner,
            hostname()
        ));
    };
    let target = expand_target(&job.target);
    if !target.is_dir() {
        return fail(format!(
            "target is not a directory on {}: {}",
            hostname(),
            target.display()
        ));
    }
    let ac = match agent_command(runner, &job.prompt, &target, job.resume.as_ref(), &job.opts) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let program = ac.program.clone();
    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&ac.args)
        .envs(ac.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&target)
        .stdin(if ac.control.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("cannot start {}: {}", program.display(), e)),
    };
    let pid = child.id();
    out(
        json!({ "t": "start", "runner": runner.name, "format": runner.format, "pid": pid, "host": hostname(), "interactive": ac.control.is_some() }),
    );

    let forward = |stream: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
                   tag: &'static str,
                   out: Emit| {
        tokio::spawn(async move {
            let Some(r) = stream else { return };
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                out(json!({ "t": tag, "l": l }));
            }
        })
    };
    let so = child
        .stdout
        .take()
        .map(|x| Box::new(x) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
    let se = child
        .stderr
        .take()
        .map(|x| Box::new(x) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
    let t1 = forward(so, "out", out.clone());
    let t2 = forward(se, "err", out.clone());

    // An interactive agent's stdin: its opening lines, then what the hub sends.
    let mut agent_in = child.stdin.take();
    if let (Some(w), Some(lines)) = (agent_in.as_mut(), &ac.control) {
        for l in lines {
            let _ = w.write_all(format!("{}\n", l).as_bytes()).await;
        }
        let _ = w.flush().await;
    }
    let stop = async move {
        while let Some(i) = input.recv().await {
            match i {
                JobInput::Line(l) => {
                    if let Some(w) = agent_in.as_mut() {
                        if w.write_all(format!("{}\n", l).as_bytes()).await.is_err()
                            || w.flush().await.is_err()
                        {
                            agent_in = None;
                        }
                    }
                }
                JobInput::Close => agent_in = None,
                JobInput::Stop => return,
            }
        }
        // The input side is gone (the hub went away): stop as well.
    };
    let status = tokio::select! {
        st = child.wait() => st,
        _ = stop => crate::deck::procs::stop_tree(&mut child, pid).await,
    };
    let _ = t1.await;
    let _ = t2.await;
    let code = status.ok().and_then(|st| st.code()).unwrap_or(1);
    out(json!({ "t": "exit", "code": code }));
    code
}

/// Claude Code's permission modes.
pub const PERMISSION_MODES: [&str; 6] = [
    "manual",
    "acceptEdits",
    "auto",
    "plan",
    "dontAsk",
    "bypassPermissions",
];
/// Claude Code's effort levels.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// How to run one agent, beyond its prompt: usually from a profile, and
/// overridable per launch.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentOpts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    /// Environment for the agent (e.g. ANTHROPIC_BASE_URL for a local model).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl AgentOpts {
    /// Values end up as arguments and environment of a program, so each is
    /// checked: no option smuggling (a leading `-`), no control characters.
    pub fn validate(&self) -> Result<(), String> {
        let word = |what: &str, v: &str, max: usize| -> Result<(), String> {
            if v.is_empty()
                || v.len() > max
                || v.starts_with('-')
                || v.chars().any(|c| c.is_control())
            {
                return Err(format!("invalid {}: {:?}", what, v));
            }
            Ok(())
        };
        if let Some(m) = &self.model {
            word("model", m, 128)?;
            if !m
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._:/-[]".contains(c))
            {
                return Err(format!("invalid model name: {:?}", m));
            }
        }
        if let Some(m) = &self.permission_mode {
            if !PERMISSION_MODES.contains(&m.as_str()) {
                return Err(format!(
                    "permission mode must be one of {}",
                    PERMISSION_MODES.join(", ")
                ));
            }
        }
        if let Some(e) = &self.effort {
            if !EFFORTS.contains(&e.as_str()) {
                return Err(format!("effort must be one of {}", EFFORTS.join(", ")));
            }
        }
        for t in self.allowed_tools.iter().chain(&self.disallowed_tools) {
            word("tool rule", t, 300)?;
            if t.contains(',') {
                return Err(format!("a tool rule cannot contain a comma: {:?}", t));
            }
        }
        for (k, v) in &self.env {
            let ok = !k.is_empty()
                && k.len() <= 128
                && !k.starts_with(|c: char| c.is_ascii_digit())
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok {
                return Err(format!("invalid environment variable name: {:?}", k));
            }
            if v.contains('\0') || v.len() > 8192 {
                return Err(format!("invalid value for {}", k));
            }
        }
        Ok(())
    }

    /// `other` on top of `self`: set fields win, lists and env are replaced.
    pub fn with(&self, other: &AgentOpts) -> AgentOpts {
        let mut o = self.clone();
        if other.model.is_some() {
            o.model = other.model.clone();
        }
        if other.permission_mode.is_some() {
            o.permission_mode = other.permission_mode.clone();
        }
        if other.effort.is_some() {
            o.effort = other.effort.clone();
        }
        if !other.allowed_tools.is_empty() {
            o.allowed_tools = other.allowed_tools.clone();
        }
        if !other.disallowed_tools.is_empty() {
            o.disallowed_tools = other.disallowed_tools.clone();
        }
        o.env
            .extend(other.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        o
    }
}

/// Everything needed to start an agent.
#[derive(Debug, Clone)]
pub struct AgentCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Driven over Claude Code's control channel: these lines go to its
    /// stdin first (initialize, then the prompt), and stdin stays open.
    pub control: Option<Vec<String>>,
}

/// The command for a runner with options. A Claude Code runner whose
/// arguments carry `{prompt}` on its own is driven interactively, so its
/// tool requests can be approved and its mode and model changed while it
/// runs; other runners take the prompt as an argument, as before.
pub fn agent_command(
    r: &Runner,
    prompt: &str,
    target: &std::path::Path,
    resume: Option<&Resume>,
    o: &AgentOpts,
) -> Result<AgentCommand, String> {
    o.validate()?;
    let claude = r.format == "claude-stream-json";
    let interactive = claude
        && r.args.iter().any(|a| a == "{prompt}")
        && r.args
            .iter()
            .filter(|a| a.contains("{prompt}"))
            .all(|a| a == "{prompt}");
    let (program, mut args) = runner_argv_with(
        r,
        if interactive { "\u{0}" } else { prompt },
        target,
        resume,
    )?;
    let mut control = None;
    if interactive {
        // The prompt goes over stdin as the first user message instead.
        args.retain(|a| a != "\u{0}");
        args.extend(
            [
                "--input-format",
                "stream-json",
                "--permission-prompt-tool",
                "stdio",
            ]
            .map(String::from),
        );
        let prompt = prompt.replace("{target}", &target.to_string_lossy());
        control = Some(vec![
            json!({ "type": "control_request", "request_id": "indra-init", "request": { "subtype": "initialize" } }).to_string(),
            user_message(&prompt),
        ]);
    }
    if claude {
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
    } else {
        if let Some(m) = &o.model {
            if r.model_args.is_empty() {
                return Err(format!(
                    "runner '{}' cannot choose a model (set model_args in its config)",
                    r.name
                ));
            }
            args.extend(r.model_args.iter().map(|a| a.replace("{model}", m)));
        }
        if o.permission_mode.is_some()
            || o.effort.is_some()
            || !o.allowed_tools.is_empty()
            || !o.disallowed_tools.is_empty()
        {
            return Err(format!(
                "runner '{}' is not Claude Code, so permission mode, effort and tool rules do not apply to it",
                r.name
            ));
        }
    }
    Ok(AgentCommand {
        program,
        args,
        env: o.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        control,
    })
}

/// A user turn for Claude Code's stream-json input.
pub fn user_message(text: &str) -> String {
    json!({ "type": "user", "message": { "role": "user", "content": text }, "parent_tool_use_id": null, "session_id": "" }).to_string()
}

/// The argument vector a runner executes for a prompt in a target. The
/// prompt is substituted last so its own text is never expanded again.
/// With `resume`, an earlier session is continued (or branched) through the
/// runner's `resume_args` / `fork_args`.
pub fn runner_argv_with(
    r: &Runner,
    prompt: &str,
    target: &std::path::Path,
    resume: Option<&Resume>,
) -> Result<(PathBuf, Vec<String>), String> {
    let program = r.program.clone().ok_or_else(|| {
        format!(
            "runner '{}': '{}' is not installed or not on PATH",
            r.name, r.command
        )
    })?;
    let target = target.to_string_lossy();
    let prompt = prompt.replace("{target}", &target);
    let mut args: Vec<String> = r
        .args
        .iter()
        .map(|a| a.replace("{target}", &target).replace("{prompt}", &prompt))
        .collect();
    if let Some(res) = resume {
        if r.resume_args.is_empty() {
            return Err(format!(
                "runner '{}' cannot continue a session (it has no resume_args); pivot with a fresh start instead",
                r.name
            ));
        }
        validate_session(&res.session)?;
        args.extend(
            r.resume_args
                .iter()
                .map(|a| a.replace("{session}", &res.session)),
        );
        if res.fork {
            args.extend(r.fork_args.iter().cloned());
        }
    }
    Ok((program, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_arguments() {
        let r = Runner {
            name: "claude".into(),
            command: "claude".into(),
            args: vec!["-p".into(), "{prompt}".into()],
            format: "claude-stream-json".into(),
            resume_args: vec!["--resume".into(), "{session}".into()],
            fork_args: vec!["--fork-session".into()],
            model_args: vec![],
            program: Some(PathBuf::from("/bin/claude")),
        };
        let t = std::path::Path::new("/t");
        let res = Resume {
            session: "4f9c2c1e-8b1d-4c55-9b3e-0d6c5c7e2a10".into(),
            fork: false,
        };
        let (_, a) = runner_argv_with(&r, "next", t, Some(&res)).unwrap();
        assert_eq!(
            a,
            vec![
                "-p",
                "next",
                "--resume",
                "4f9c2c1e-8b1d-4c55-9b3e-0d6c5c7e2a10"
            ]
        );
        let fork = Resume {
            fork: true,
            ..res.clone()
        };
        let (_, a) = runner_argv_with(&r, "next", t, Some(&fork)).unwrap();
        assert_eq!(a.last().unwrap(), "--fork-session");
        // A session id can never smuggle in an option.
        for bad in ["--dangerously-skip-permissions", "a b", "", "x;y"] {
            let r2 = Resume {
                session: bad.into(),
                fork: false,
            };
            assert!(runner_argv_with(&r, "x", t, Some(&r2)).is_err(), "{}", bad);
        }
        let plain = Runner {
            resume_args: vec![],
            ..r
        };
        assert!(runner_argv_with(&plain, "x", t, Some(&res))
            .unwrap_err()
            .contains("cannot continue"));
    }

    #[test]
    fn destinations() {
        for ok in [
            "host",
            "user@host.example.com",
            "build-box",
            "10.0.0.5",
            "me@[::1]",
            "alias_1",
        ] {
            assert!(validate_destination(ok).is_ok(), "{}", ok);
        }
        for bad in [
            "",
            "-oProxyCommand=sh",
            "host name",
            "a;b",
            "a$(x)",
            "a`b`",
            "h\nx",
        ] {
            assert!(validate_destination(bad).is_err(), "{}", bad);
        }
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("indra"), "indra");
        assert_eq!(shell_quote("/opt/indra/bin/indra"), "/opt/indra/bin/indra");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("$(rm -rf ~)"), "'$(rm -rf ~)'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn ssh_command_shape() {
        let n = Node {
            name: "box".into(),
            description: String::new(),
            ssh: "me@box".into(),
            port: Some(2222),
            identity: Some(PathBuf::from("/k/id")),
            indra: "/opt/my indra".into(),
            args: vec!["--config".into(), "/etc/indra.toml".into()],
            max_parallel: 2,
            targets: vec![],
            ssh_command: "sh".into(),
        };
        let (_, a) = n.ssh_command_for("exec").unwrap();
        let tail: Vec<&str> = a
            .iter()
            .skip_while(|x| *x != "me@box")
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            vec![
                "me@box",
                "--",
                "'/opt/my indra'",
                "node",
                "exec",
                "--config",
                "/etc/indra.toml"
            ]
        );
        assert!(a.contains(&"BatchMode=yes".to_string()));
        assert!(a.windows(2).any(|w| w[0] == "-p" && w[1] == "2222"));
    }
}
