//! Launching agents and watching them work.
//!
//! A runner's program is executed directly with an argument vector, never
//! through a shell, so a prompt cannot inject a command. Output is kept in
//! memory for the live view and appended to a log file under the data dir.
//! With Claude Code's `--output-format stream-json`, each line is parsed into
//! text, tool calls and subagent (Task/Agent tool) spawns, so the console can
//! show which agents a run started and what each one is doing.

use crate::deck::config::Runner;
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
    pub runner: String,
    pub harness: String,
    pub prompt: String,
    pub cwd: String,
    pub command: Vec<String>,
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
}

pub struct Proc {
    id: u64,
    runner: String,
    harness: String,
    prompt: String,
    cwd: PathBuf,
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
            runner: self.runner.clone(),
            harness: self.harness.clone(),
            prompt: self.prompt.clone(),
            cwd: self.cwd.display().to_string(),
            command: self.command.clone(),
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

pub struct Launch<'a> {
    pub runner: &'a Runner,
    pub harness: &'a str,
    pub prompt: &'a str,
    pub target: &'a Path,
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

    /// The exact argument vector a launch would execute (also used by tests).
    pub fn command_for(l: &Launch) -> Result<(PathBuf, Vec<String>), String> {
        let program = l.runner.program.clone().ok_or_else(|| {
            format!(
                "runner '{}': '{}' is not installed or not on PATH",
                l.runner.name, l.runner.command
            )
        })?;
        let target = l.target.to_string_lossy();
        // {target} inside the prompt is intended (presets use it); the
        // prompt is substituted last so its text is never expanded again.
        let prompt = l.prompt.replace("{target}", &target);
        let args = l
            .runner
            .args
            .iter()
            .map(|a| a.replace("{target}", &target).replace("{prompt}", &prompt))
            .collect();
        Ok((program, args))
    }

    pub fn start(self: &Arc<Self>, l: Launch) -> Result<u64, String> {
        let (program, args) = Self::command_for(&l)?;
        if !l.target.is_dir() {
            return Err(format!("target is not a directory: {}", l.target.display()));
        }
        std::fs::create_dir_all(&self.log_dir)
            .map_err(|e| format!("cannot create {}: {}", self.log_dir.display(), e))?;
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let started = now();
        let log = self
            .log_dir
            .join(format!("{}-{}-{}.log", started, id, l.runner.name));

        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&args)
            .current_dir(l.target)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {}: {}", program.display(), e))?;

        let (tx, _) = broadcast::channel(1024);
        let (stop_tx, stop_rx) = oneshot::channel();
        let proc = Arc::new(Proc {
            id,
            runner: l.runner.name.clone(),
            harness: l.harness.to_string(),
            prompt: l.prompt.to_string(),
            cwd: l.target.to_path_buf(),
            command: std::iter::once(program.display().to_string())
                .chain(args.iter().cloned())
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
            }),
            tx,
            stop: Mutex::new(Some(stop_tx)),
        });
        self.procs.lock().unwrap().insert(id, proc.clone());
        proc.push(
            "system",
            format!("started {} in {}", l.runner.name, l.target.display()),
            None,
        );

        let logf = Arc::new(Mutex::new(std::fs::File::create(&log).ok()));
        let claude = l.runner.format == "claude-stream-json";
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
                if claude {
                    p1.claude_line(&line);
                } else if !line.trim().is_empty() {
                    p1.push("text", line, None);
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
            let mut stopped = false;
            let status = tokio::select! {
                s = child.wait() => s,
                _ = stop_rx => {
                    stopped = true;
                    p3.push("system", "stop requested".into(), None);
                    terminate_tree(pid);
                    match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                        Ok(s) => s,
                        Err(_) => {
                            let _ = child.start_kill();
                            child.wait().await
                        }
                    }
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
fn terminate_tree(pid: Option<u32>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn runner(format: &str, program: Option<PathBuf>) -> Runner {
        Runner {
            name: "r".into(),
            command: "r".into(),
            args: vec!["-p".into(), "{prompt}".into(), "--cwd={target}".into()],
            format: format.into(),
            program,
        }
    }

    #[test]
    fn prompt_is_one_argument_and_never_re_expanded() {
        let r = runner("text", Some(PathBuf::from("/bin/r")));
        let l = Launch {
            runner: &r,
            harness: "h",
            prompt: "review {target}; rm -rf / {prompt}",
            target: Path::new("/t"),
        };
        let (_, args) = Procs::command_for(&l).unwrap();
        assert_eq!(args, vec!["-p", "review /t; rm -rf / {prompt}", "--cwd=/t"]);
        let r = runner("text", None);
        let l = Launch {
            runner: &r,
            harness: "h",
            prompt: "x",
            target: Path::new("/t"),
        };
        assert!(Procs::command_for(&l)
            .unwrap_err()
            .contains("not installed"));
    }

    #[test]
    fn parses_claude_stream_json() {
        let (tx, _) = broadcast::channel(16);
        let p = Proc {
            id: 1,
            runner: "claude".into(),
            harness: "h".into(),
            prompt: "p".into(),
            cwd: PathBuf::from("."),
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
            }),
            tx,
            stop: Mutex::new(None),
        };
        for l in [
            r#"{"type":"system","subtype":"init","model":"m1","tools":[]}"#,
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
        assert!(p.state.lock().unwrap().events[7].text.contains("$0.0123"));
    }
}
