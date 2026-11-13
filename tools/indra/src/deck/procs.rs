//! Launching agents and watching them work.
//!
//! A runner's program is executed directly with an argument vector, never
//! through a shell, so a prompt cannot inject a command. Output is kept in
//! memory for the live view and appended to a log file under the data dir.
//! With Claude Code's `--output-format stream-json`, each line is parsed into
//! text, tool calls and subagent (Task/Agent tool) spawns, so the console can
//! show which agents a run started and what each one is doing.

use crate::deck::config::Runner;
use crate::deck::nodes::{Node, Resume};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, oneshot};

const MAX_EVENTS: usize = 5000;
const MAX_TEXT: usize = 4000;

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub seq: u64,
    pub ts: u64,
    /// text | tool | agent | result | system | error | stderr | exit
    pub kind: String,
    pub text: String,
    /// The subagent this came from, when the runner reports one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubAgent {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub description: String,
    /// running | done | error
    pub state: &'static str,
    pub started: u64,
    pub tools: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcView {
    pub id: u64,
    /// "local" or the name of the node it runs on.
    pub node: String,
    pub runner: String,
    pub harness: String,
    pub prompt: String,
    pub cwd: String,
    pub command: Vec<String>,
    /// Remote host name, once the node reports it.
    pub host: Option<String>,
    /// The agent's own session id, when its runner reports one (Claude Code does).
    pub session: Option<String>,
    /// The process this one was pivoted from.
    pub parent: Option<u64>,
    /// The session this process continued, when it was a "continue" pivot.
    pub resumed: Option<String>,
    /// Whether its runner can continue a session (a "continue" pivot).
    pub can_resume: bool,
    pub started: u64,
    pub ended: Option<u64>,
    /// running | exited | failed | stopped
    pub status: String,
    pub exit_code: Option<i32>,
    pub events: u64,
    pub tools: BTreeMap<String, usize>,
    pub agents: Vec<SubAgent>,
    pub cost_usd: Option<f64>,
    pub log: String,
    pub last: Option<String>,
}

struct State {
    status: String,
    exit_code: Option<i32>,
    ended: Option<u64>,
    events: VecDeque<Event>,
    seq: u64,
    tools: BTreeMap<String, usize>,
    agents: Vec<SubAgent>,
    cost_usd: Option<f64>,
    /// Output format of the agent itself (for remote jobs, learnt from the node).
    format: String,
    host: Option<String>,
    session: Option<String>,
}

pub struct Proc {
    id: u64,
    node: String,
    parent: Option<u64>,
    resumed: Option<String>,
    can_resume: bool,
    runner: String,
    harness: String,
    prompt: String,
    cwd: String,
    command: Vec<String>,
    started: u64,
    log: PathBuf,
    state: Mutex<State>,
    tx: broadcast::Sender<Event>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
}

fn now() -> u64 {
    crate::deck::runs::now()
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut o: String = s.chars().take(n).collect();
        o.push_str("...");
        o
    }
}

impl Proc {
    fn push(&self, kind: &str, text: String, agent: Option<String>) {
        let ev = {
            let mut st = self.state.lock().unwrap();
            st.seq += 1;
            let ev = Event {
                seq: st.seq,
                ts: now(),
                kind: kind.to_string(),
                text: clip(&text, MAX_TEXT),
                agent,
            };
            st.events.push_back(ev.clone());
            while st.events.len() > MAX_EVENTS {
                st.events.pop_front();
            }
            ev
        };
        let _ = self.tx.send(ev);
    }

    pub fn view(&self) -> ProcView {
        let st = self.state.lock().unwrap();
        ProcView {
            id: self.id,
            node: self.node.clone(),
            runner: self.runner.clone(),
            harness: self.harness.clone(),
            prompt: self.prompt.clone(),
            cwd: self.cwd.clone(),
            command: self.command.clone(),
            host: st.host.clone(),
            session: st.session.clone(),
            parent: self.parent,
            resumed: self.resumed.clone(),
            can_resume: self.can_resume,
            started: self.started,
            ended: st.ended,
            status: st.status.clone(),
            exit_code: st.exit_code,
            events: st.seq,
            tools: st.tools.clone(),
            agents: st.agents.clone(),
            cost_usd: st.cost_usd,
            log: self.log.display().to_string(),
            last: st
                .events
                .iter()
                .rev()
                .find(|e| e.kind != "stderr")
                .map(|e| clip(&e.text, 160)),
        }
    }

    /// Stored events after `after`, plus a live receiver. Subscribing first
    /// and filtering by sequence number means nothing is missed or doubled.
    pub fn subscribe(&self, after: u64) -> (Vec<Event>, broadcast::Receiver<Event>) {
        let rx = self.tx.subscribe();
        let st = self.state.lock().unwrap();
        (
            st.events
                .iter()
                .filter(|e| e.seq > after)
                .cloned()
                .collect(),
            rx,
        )
    }

    /// One line of the agent's own stdout, in its format.
    fn agent_line(&self, line: &str) {
        let claude = self.state.lock().unwrap().format == "claude-stream-json";
        if claude {
            self.claude_line(line);
        } else if !line.trim().is_empty() {
            self.push("text", line.to_string(), None);
        }
    }

    /// One envelope line from `indra node exec` on a remote node.
    fn node_line(&self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            if !line.trim().is_empty() {
                self.push("text", line.to_string(), None);
            }
            return;
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match v.get("t").and_then(Value::as_str).unwrap_or("") {
            "start" => {
                {
                    let mut st = self.state.lock().unwrap();
                    st.format = s("format");
                    st.host = Some(s("host")).filter(|h| !h.is_empty());
                }
                self.push(
                    "system",
                    format!(
                        "running on {} as pid {}",
                        s("host"),
                        v.get("pid").map(|p| p.to_string()).unwrap_or_default()
                    ),
                    None,
                );
            }
            "out" => self.agent_line(&s("l")),
            "err" => {
                let l = s("l");
                if !l.trim().is_empty() {
                    self.push("stderr", l, None);
                }
            }
            "error" => self.push("error", s("msg"), None),
            "exit" => {}
            _ => {}
        }
    }

    /// What the agent concluded: its final result, else its last messages.
    pub fn outcome(&self) -> String {
        let st = self.state.lock().unwrap();
        if let Some(r) = st.events.iter().rev().find(|e| e.kind == "result") {
            return clip(&r.text, 6000);
        }
        let mut texts: Vec<&str> = st
            .events
            .iter()
            .rev()
            .filter(|e| e.kind == "text")
            .take(8)
            .map(|e| e.text.as_str())
            .collect();
        texts.reverse();
        clip(&texts.join("\n\n"), 6000)
    }

    pub fn runner_name(&self) -> &str {
        &self.runner
    }

    pub fn node_name(&self) -> &str {
        &self.node
    }

    pub fn harness_name(&self) -> &str {
        &self.harness
    }

    /// The directory it runs in (a path on its node).
    pub fn target(&self) -> &str {
        &self.cwd
    }

    pub fn can_resume(&self) -> bool {
        self.can_resume
    }

    pub fn is_running(&self) -> bool {
        self.state.lock().unwrap().status == "running"
    }

    pub fn request_stop(&self) -> bool {
        match self.stop.lock().unwrap().take() {
            Some(tx) => tx.send(()).is_ok(),
            None => false,
        }
    }

    /// Interpret one line of `claude --output-format stream-json`.
    fn claude_line(&self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            self.push("text", line.to_string(), None);
            return;
        };
        let parent = v
            .get("parent_tool_use_id")
            .and_then(Value::as_str)
            .map(String::from);
        let agent = parent.as_deref().and_then(|pid| {
            let st = self.state.lock().unwrap();
            st.agents
                .iter()
                .find(|a| a.id == pid)
                .map(|a| a.kind.clone())
        });
        let s = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        // Claude Code tags its stream messages with the session id; keep the
        // latest so the agent can be continued (pivoted) later.
        if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
            if crate::deck::nodes::validate_session(sid).is_ok() {
                self.state.lock().unwrap().session = Some(sid.to_string());
            }
        }
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "system" if s(&v, "subtype") == "init" => {
                let model = s(&v, "model");
                self.push(
                    "system",
                    format!(
                        "session started, model {}",
                        if model.is_empty() { "?" } else { &model }
                    ),
                    agent,
                );
            }
            "assistant" => {
                let content = v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for c in content {
                    match c.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => {
                            let t = s(&c, "text");
                            if !t.trim().is_empty() {
                                self.push("text", t, agent.clone());
                            }
                        }
                        "tool_use" => self.tool_use(&c, agent.clone(), parent.as_deref()),
                        _ => {}
                    }
                }
            }
            "user" => {
                let content = v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for c in content {
                    if c.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let id = s(&c, "tool_use_id");
                    let is_error = c.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                    let finished = {
                        let mut st = self.state.lock().unwrap();
                        st.agents.iter_mut().find(|a| a.id == id).map(|a| {
                            a.state = if is_error { "error" } else { "done" };
                            a.kind.clone()
                        })
                    };
                    if let Some(kind) = finished {
                        let word = if is_error { "failed" } else { "finished" };
                        self.push("agent", format!("{} {}", kind, word), agent.clone());
                    } else if is_error {
                        self.push("error", result_text(&c), agent.clone());
                    }
                }
            }
            "result" => {
                let cost = v.get("total_cost_usd").and_then(Value::as_f64);
                let turns = v.get("num_turns").and_then(Value::as_u64).unwrap_or(0);
                let secs = v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0) / 1000;
                self.state.lock().unwrap().cost_usd = cost;
                let mut text = format!("{}: {} turns, {}s", s(&v, "subtype"), turns, secs);
                if let Some(c) = cost {
                    text.push_str(&format!(", ${:.4}", c));
                }
                let r = s(&v, "result");
                if !r.is_empty() {
                    text.push_str("\n\n");
                    text.push_str(&r);
                }
                self.push("result", text, agent);
            }
            _ => {}
        }
    }

    fn tool_use(&self, c: &Value, agent: Option<String>, parent: Option<&str>) {
        let name = c
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        let input = c.get("input").cloned().unwrap_or(Value::Null);
        let field = |k: &str| input.get(k).and_then(Value::as_str).map(String::from);
        if name == "Task" || name == "Agent" {
            let kind = field("subagent_type").unwrap_or_else(|| "general-purpose".into());
            let desc = field("description").unwrap_or_default();
            {
                let mut st = self.state.lock().unwrap();
                *st.tools.entry(name.clone()).or_insert(0) += 1;
                st.agents.push(SubAgent {
                    id: c
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    kind: kind.clone(),
                    description: desc.clone(),
                    state: "running",
                    started: now(),
                    tools: 0,
                });
            }
            self.push("agent", format!("spawned {}: {}", kind, desc), agent);
            return;
        }
        let summary = [
            "command",
            "file_path",
            "path",
            "pattern",
            "url",
            "query",
            "skill",
            "description",
            "prompt",
        ]
        .iter()
        .find_map(|k| field(k))
        .unwrap_or_else(|| clip(&input.to_string(), 160));
        {
            let mut st = self.state.lock().unwrap();
            *st.tools.entry(name.clone()).or_insert(0) += 1;
            // Credit the exact subagent: two hunters of the same type are
            // told apart by the tool-use id that spawned them.
            if let Some(pid) = parent {
                if let Some(sa) = st.agents.iter_mut().find(|x| x.id == pid) {
                    sa.tools += 1;
                }
            }
        }
        self.push("tool", format!("{}: {}", name, clip(&summary, 300)), agent);
    }
}

fn result_text(c: &Value) -> String {
    match c.get("content") {
        Some(Value::String(s)) => clip(s, 600),
        Some(Value::Array(a)) => clip(
            &a.iter()
                .filter_map(|x| x.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            600,
        ),
        _ => String::new(),
    }
}

pub struct Procs {
    procs: Mutex<BTreeMap<u64, Arc<Proc>>>,
    next: AtomicU64,
    log_dir: PathBuf,
}

/// Extra launch options for pivots.
#[derive(Debug, Clone, Default)]
pub struct StartOpts {
    pub resume: Option<Resume>,
    pub parent: Option<u64>,
}

/// Where and what to launch.
pub enum Launch<'a> {
    /// A runner from this machine's config, in a local directory.
    Local {
        runner: &'a Runner,
        harness: &'a str,
        prompt: &'a str,
        target: &'a Path,
    },
    /// A runner, by name, on a remote node, in a directory on that node.
    Remote {
        node: &'a Node,
        runner: &'a str,
        harness: &'a str,
        prompt: &'a str,
        target: &'a str,
    },
}

impl Procs {
    pub fn new(log_dir: PathBuf) -> Arc<Procs> {
        Arc::new(Procs {
            procs: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            log_dir,
        })
    }

    pub fn list(&self) -> Vec<ProcView> {
        let mut v: Vec<ProcView> = self
            .procs
            .lock()
            .unwrap()
            .values()
            .map(|p| p.view())
            .collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.id));
        v
    }

    pub fn get(&self, id: u64) -> Option<Arc<Proc>> {
        self.procs.lock().unwrap().get(&id).cloned()
    }

    pub fn running(&self) -> usize {
        self.procs
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.is_running())
            .count()
    }

    /// Agents running on one node.
    /// Stop every running process and wait for them to end: each gets
    /// SIGTERM to its whole tree, then SIGKILL after 5 s. Used when the
    /// console exits, so no agent outlives the hub that started it.
    pub async fn stop_all(&self) {
        let running: Vec<Arc<Proc>> = self
            .procs
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.is_running())
            .cloned()
            .collect();
        for p in &running {
            p.request_stop();
        }
        // Remote stops wait up to 5 s for the node, then 5 s more locally.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
        while running.iter().any(|p| p.is_running()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    pub fn running_on(&self, node: &str) -> usize {
        self.procs
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.node == node && p.is_running())
            .count()
    }

    pub fn start(self: &Arc<Self>, l: Launch) -> Result<u64, String> {
        self.start_with(l, StartOpts::default())
    }

    /// Start, optionally continuing an earlier session (a pivot) and
    /// recording the process it came from.
    pub fn start_with(self: &Arc<Self>, l: Launch, opts: StartOpts) -> Result<u64, String> {
        // Resolve the launch into a program, its arguments, and what the UI shows.
        struct Plan {
            program: PathBuf,
            args: Vec<String>,
            cwd: Option<PathBuf>,
            stdin: Option<String>,
            node: String,
            runner: String,
            harness: String,
            prompt: String,
            target: String,
            format: String,
            remote: bool,
            can_resume: bool,
        }
        let plan = match l {
            Launch::Local {
                runner,
                harness,
                prompt,
                target,
            } => {
                let (program, args) = crate::deck::nodes::runner_argv_with(
                    runner,
                    prompt,
                    target,
                    opts.resume.as_ref(),
                )?;
                if !target.is_dir() {
                    return Err(format!("target is not a directory: {}", target.display()));
                }
                Plan {
                    program,
                    args,
                    cwd: Some(target.to_path_buf()),
                    stdin: None,
                    node: crate::deck::nodes::LOCAL.into(),
                    runner: runner.name.clone(),
                    harness: harness.into(),
                    prompt: prompt.into(),
                    target: target.display().to_string(),
                    format: runner.format.clone(),
                    remote: false,
                    can_resume: !runner.resume_args.is_empty(),
                }
            }
            Launch::Remote {
                node,
                runner,
                harness,
                prompt,
                target,
            } => {
                let (program, args) = node.ssh_command_for("exec")?;
                let job = crate::deck::nodes::Job {
                    runner: runner.into(),
                    prompt: prompt.replace("{node}", &node.name),
                    target: target.into(),
                    resume: opts.resume.clone(),
                };
                Plan {
                    program,
                    args,
                    cwd: None,
                    stdin: Some(serde_json::to_string(&job).map_err(|e| e.to_string())?),
                    node: node.name.clone(),
                    runner: runner.into(),
                    harness: harness.into(),
                    prompt: prompt.into(),
                    target: target.into(),
                    format: String::new(),
                    remote: true,
                    // The node decides; it refuses if its runner cannot resume.
                    can_resume: true,
                }
            }
        };
        std::fs::create_dir_all(&self.log_dir)
            .map_err(|e| format!("cannot create {}: {}", self.log_dir.display(), e))?;
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let started = now();
        let log = self.log_dir.join(format!(
            "{}-{}-{}-{}.log",
            started, id, plan.node, plan.runner
        ));

        let mut cmd = tokio::process::Command::new(&plan.program);
        cmd.args(&plan.args)
            .stdin(if plan.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(c) = &plan.cwd {
            cmd.current_dir(c);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {}: {}", plan.program.display(), e))?;
        // A remote job lives as long as its stdin stays open: the waiter task
        // below writes the job, then holds stdin until the job ends.
        let mut job_stdin = child.stdin.take();
        let payload = plan.stdin.clone();

        let (tx, _) = broadcast::channel(1024);
        let (stop_tx, stop_rx) = oneshot::channel();
        let proc = Arc::new(Proc {
            id,
            node: plan.node.clone(),
            parent: opts.parent,
            resumed: opts.resume.as_ref().map(|r| r.session.clone()),
            can_resume: plan.can_resume,
            runner: plan.runner.clone(),
            harness: plan.harness.clone(),
            prompt: plan.prompt.clone(),
            cwd: plan.target.clone(),
            command: std::iter::once(plan.program.display().to_string())
                .chain(plan.args.iter().cloned())
                .collect(),
            started,
            log: log.clone(),
            state: Mutex::new(State {
                status: "running".into(),
                exit_code: None,
                ended: None,
                events: VecDeque::new(),
                seq: 0,
                tools: BTreeMap::new(),
                agents: Vec::new(),
                cost_usd: None,
                format: plan.format.clone(),
                host: None,
                session: None,
            }),
            tx,
            stop: Mutex::new(Some(stop_tx)),
        });
        self.procs.lock().unwrap().insert(id, proc.clone());
        proc.push(
            "system",
            {
                let mut m = if plan.remote {
                    format!(
                        "started {} on node {} in {}",
                        plan.runner, plan.node, plan.target
                    )
                } else {
                    format!("started {} in {}", plan.runner, plan.target)
                };
                if let Some(parent) = opts.parent {
                    m.push_str(&match &opts.resume {
                        Some(r) if r.fork => {
                            format!(", branching the session of process #{}", parent)
                        }
                        Some(_) => format!(", continuing the session of process #{}", parent),
                        None => format!(", pivoted from process #{}", parent),
                    });
                }
                m
            },
            None,
        );

        let logf = Arc::new(Mutex::new(std::fs::File::create(&log).ok()));
        let remote = plan.remote;
        let pid = child.id();

        let out = child.stdout.take();
        let err = child.stderr.take();
        let (p1, lf1) = (proc.clone(), logf.clone());
        let t_out = tokio::spawn(async move {
            let Some(out) = out else { return };
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(f) = lf1.lock().unwrap().as_mut() {
                    let _ = writeln!(f, "{}", line);
                }
                if remote {
                    p1.node_line(&line);
                } else {
                    p1.agent_line(&line);
                }
            }
        });
        let (p2, lf2) = (proc.clone(), logf.clone());
        let t_err = tokio::spawn(async move {
            let Some(err) = err else { return };
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(f) = lf2.lock().unwrap().as_mut() {
                    let _ = writeln!(f, "[stderr] {}", line);
                }
                if !line.trim().is_empty() {
                    p2.push("stderr", line, None);
                }
            }
        });

        let p3 = proc.clone();
        tokio::spawn(async move {
            if let (Some(si), Some(payload)) = (job_stdin.as_mut(), payload) {
                let line = format!("{}\n", payload);
                if let Err(e) = tokio::io::AsyncWriteExt::write_all(si, line.as_bytes()).await {
                    p3.push(
                        "error",
                        format!("could not send the job to the node: {}", e),
                        None,
                    );
                }
            }
            let mut stopped = false;
            let status = tokio::select! {
                s = child.wait() => s,
                _ = stop_rx => {
                    stopped = true;
                    p3.push("system", "stop requested".into(), None);
                    // Closing a remote job's stdin makes the node stop the agent's tree;
                    // then the local ssh (or the local agent) is terminated as well.
                    drop(job_stdin.take());
                    if remote {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
                    }
                    stop_tree(&mut child, pid).await
                }
            };
            let _ = t_out.await;
            let _ = t_err.await;
            let (kind, text) = {
                let mut st = p3.state.lock().unwrap();
                st.ended = Some(now());
                *p3.stop.lock().unwrap() = None;
                match &status {
                    Ok(s) => {
                        st.exit_code = s.code();
                        st.status = if stopped {
                            "stopped".into()
                        } else if s.success() {
                            "exited".into()
                        } else {
                            "failed".into()
                        };
                    }
                    Err(_) => st.status = "failed".into(),
                }
                for a in st.agents.iter_mut().filter(|a| a.state == "running") {
                    a.state = if stopped { "error" } else { "done" };
                }
                let text = match &status {
                    Ok(s) => format!(
                        "{} (exit code {})",
                        st.status,
                        s.code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "none".into())
                    ),
                    Err(e) => format!("failed: {}", e),
                };
                ("exit", text)
            };
            p3.push(kind, text, None);
        });
        Ok(id)
    }
}

/// Ask the whole process tree to stop: the runner usually has children of
/// its own (shells, MCP servers, subagents).
pub(crate) fn terminate_tree(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    {
        // The child leads its own process group (process_group(0) at spawn),
        // so a negative pid signals the runner and everything it started.
        if let Ok(pgid) = i32::try_from(pid) {
            // SAFETY: kill(2) takes plain integers and has no memory effects.
            unsafe {
                libc::kill(-pgid, libc::SIGTERM);
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Stop a runner and everything it started: SIGTERM to its process group,
/// up to 5 s for the group to exit, then SIGKILL to whatever is left. The
/// group matters, not just the runner: a child that ignores SIGTERM would
/// otherwise outlive it and keep its output pipe open.
pub(crate) async fn stop_tree(
    child: &mut tokio::process::Child,
    pid: Option<u32>,
) -> std::io::Result<std::process::ExitStatus> {
    terminate_tree(pid);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = tokio::time::timeout_at(deadline, child.wait()).await.ok();
    while group_alive(pid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    kill_tree(pid);
    match status {
        Some(s) => s,
        None => {
            let _ = child.start_kill();
            child.wait().await
        }
    }
}

/// Whether any process in the runner's group is still alive.
fn group_alive(pid: Option<u32>) -> bool {
    #[cfg(unix)]
    if let Some(pgid) = pid.and_then(|p| i32::try_from(p).ok()) {
        // SAFETY: signal 0 only checks that the group exists.
        return unsafe { libc::kill(-pgid, 0) == 0 };
    }
    let _ = pid;
    false
}

/// SIGKILL the whole process group (a no-op once it is empty).
fn kill_tree(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pgid) = pid.and_then(|p| i32::try_from(p).ok()) {
        // SAFETY: kill(2) takes plain integers and has no memory effects.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    // Windows: terminate_tree already ended the tree with taskkill /F.
    let _ = pid;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner(format: &str, program: Option<PathBuf>) -> Runner {
        Runner {
            name: "r".into(),
            command: "r".into(),
            args: vec!["-p".into(), "{prompt}".into(), "--cwd={target}".into()],
            format: format.into(),
            resume_args: vec![],
            fork_args: vec![],
            program,
        }
    }

    /// A child that ignores SIGTERM must not outlive Stop (or the console).
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_kills_children_that_ignore_sigterm() {
        let dir = std::env::temp_dir().join(format!("indra-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("child.pid");
        let r = Runner {
            name: "stubborn".into(),
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "sh -c 'trap \"\" TERM; echo $$ > \"$0\"; exec sleep 600' \"$0\" & wait".into(),
                "{prompt}".into(),
            ],
            format: "text".into(),
            resume_args: vec![],
            fork_args: vec![],
            program: crate::deck::config::which("sh"),
        };
        let procs = Procs::new(dir.join("logs"));
        let id = procs
            .start(Launch::Local {
                runner: &r,
                harness: "h",
                prompt: pidfile.to_str().unwrap(),
                target: &dir,
            })
            .unwrap();
        let mut child = None;
        for _ in 0..200 {
            child = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok());
            if child.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let child = child.expect("the child never started");
        // Alive, and not a zombie: a killed orphan stays a zombie until
        // something reaps it, and a container's PID 1 often never does.
        let alive = |pid: i32| {
            // SAFETY: signal 0 only checks that the process exists.
            let exists = unsafe { libc::kill(pid, 0) == 0 };
            let zombie = std::fs::read_to_string(format!("/proc/{}/stat", pid))
                .ok()
                .and_then(|s| s.rsplit_once(") ").map(|(_, r)| r.starts_with('Z')))
                .unwrap_or(false);
            exists && !zombie
        };
        assert!(alive(child));

        procs.stop_all().await;
        assert_eq!(procs.get(id).unwrap().view().status, "stopped");
        assert!(!alive(child), "a child that ignored SIGTERM survived Stop");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prompt_is_one_argument_and_never_re_expanded() {
        let r = runner("text", Some(PathBuf::from("/bin/r")));
        let (_, args) = crate::deck::nodes::runner_argv_with(
            &r,
            "review {target}; rm -rf / {prompt}",
            Path::new("/t"),
            None,
        )
        .unwrap();
        assert_eq!(args, vec!["-p", "review /t; rm -rf / {prompt}", "--cwd=/t"]);
        let r = runner("text", None);
        assert!(
            crate::deck::nodes::runner_argv_with(&r, "x", Path::new("/t"), None)
                .unwrap_err()
                .contains("not installed")
        );
    }

    #[test]
    fn parses_claude_stream_json() {
        let (tx, _) = broadcast::channel(16);
        let p = Proc {
            id: 1,
            node: "local".into(),
            parent: None,
            resumed: None,
            can_resume: true,
            runner: "claude".into(),
            harness: "h".into(),
            prompt: "p".into(),
            cwd: ".".into(),
            command: vec![],
            started: 0,
            log: PathBuf::from("x.log"),
            state: Mutex::new(State {
                status: "running".into(),
                exit_code: None,
                ended: None,
                events: VecDeque::new(),
                seq: 0,
                tools: BTreeMap::new(),
                agents: Vec::new(),
                cost_usd: None,
                format: "claude-stream-json".into(),
                host: None,
                session: None,
            }),
            tx,
            stop: Mutex::new(None),
        };
        for l in [
            r#"{"type":"system","subtype":"init","model":"m1","tools":[],"session_id":"4f9c2c1e-8b1d-4c55-9b3e-0d6c5c7e2a10"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Planning"},{"type":"tool_use","id":"t1","name":"Task","input":{"subagent_type":"sh-hunter","description":"hunt sqli"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t9","name":"Task","input":{"subagent_type":"sh-hunter","description":"hunt xss"}}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":"t1","message":{"content":[{"type":"tool_use","id":"t2","name":"Grep","input":{"pattern":"cursor.execute"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"done"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"zz","is_error":true,"content":[{"type":"text","text":"permission denied"}]}]}}"#,
            r#"{"type":"result","subtype":"success","num_turns":3,"duration_ms":4200,"total_cost_usd":0.0123,"result":"All good"}"#,
            "not json at all",
        ] {
            p.claude_line(l);
        }
        let v = p.view();
        let kinds: Vec<(String, Option<String>)> = p
            .state
            .lock()
            .unwrap()
            .events
            .iter()
            .map(|e| (e.kind.clone(), e.agent.clone()))
            .collect();
        assert_eq!(
            kinds.iter().map(|k| k.0.as_str()).collect::<Vec<_>>(),
            vec!["system", "text", "agent", "agent", "tool", "agent", "error", "result", "text"]
        );
        assert_eq!(
            kinds[4].1.as_deref(),
            Some("sh-hunter"),
            "subagent tool calls are attributed"
        );
        assert_eq!(v.agents.len(), 2);
        assert_eq!(v.agents[0].state, "done");
        assert_eq!(
            (v.agents[0].tools, v.agents[1].tools),
            (1, 0),
            "credited to the spawning call, not the newest of that type"
        );
        assert_eq!(v.tools["Grep"], 1);
        assert_eq!(v.cost_usd, Some(0.0123));
        assert_eq!(
            v.session.as_deref(),
            Some("4f9c2c1e-8b1d-4c55-9b3e-0d6c5c7e2a10")
        );
        assert!(
            p.outcome().contains("All good"),
            "the result is the outcome"
        );
        assert!(p.state.lock().unwrap().events[7].text.contains("$0.0123"));
    }
}
