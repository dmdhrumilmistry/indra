//! Launching agents and watching them work.
//!
//! A runner's program is executed directly with an argument vector, never
//! through a shell, so a prompt cannot inject a command. Output is kept in
//! memory for the live view and appended to a log file under the data dir.
//! With Claude Code's `--output-format stream-json`, each line is parsed into
//! text, tool calls and subagent (Task/Agent tool) spawns, so the console can
//! show which agents a run started and what each one is doing.

use crate::deck::config::Runner;
use crate::deck::nodes::{AgentOpts, Node, Resume};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};

const MAX_EVENTS: usize = 5000;
const MAX_TEXT: usize = 4000;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Tokens a session used, summed over its runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
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
    /// What it is doing now, for the status pad: approval | question |
    /// compacting | tool | writing | thinking (while it runs), then done |
    /// error | stopped.
    pub phase: &'static str,
    /// The tool it is using now (phase "tool") or asking to use (approval).
    pub activity: Option<String>,
    pub exit_code: Option<i32>,
    pub events: u64,
    pub tools: BTreeMap<String, usize>,
    pub agents: Vec<SubAgent>,
    pub cost_usd: Option<f64>,
    /// The cost is indra's estimate from token prices (`[[price]]`), not
    /// what the runner reported.
    #[serde(default)]
    pub cost_estimated: bool,
    pub log: String,
    pub last: Option<String>,
    /// Model, permission mode and the rest it was started with.
    pub opts: AgentOpts,
    /// Driven over Claude Code's control channel: approvals, mode, model.
    pub interactive: bool,
    /// Its input is still open: it can take a message or a control.
    pub open: bool,
    /// Current permission mode and model, as the agent reports them.
    pub mode: Option<String>,
    pub model: Option<String>,
    /// Tool requests waiting for an answer.
    pub pending: Vec<Approval>,
    /// Memory notes recalled into its prompt.
    pub memory: Vec<crate::deck::knowledge::MemRef>,
    /// It is asked for learnings before it finishes.
    pub reflect: bool,
    /// The account it runs with (`login:<agent>@<node>`: the machine's own login).
    pub account: Option<String>,
    /// none | asked | received
    pub reflection: &'static str,
    /// Tokens in its context at the last turn, and its model's window.
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    /// What its runs used, from Claude Code's result: fresh input, output,
    /// and input read from or written to the prompt cache.
    pub tokens: Option<Tokens>,
    /// Its context at its first reply: mostly what it starts with before
    /// doing anything (instructions, tool and MCP server definitions,
    /// skills, CLAUDE.md, the prompt).
    pub base_tokens: Option<u64>,
    /// MCP servers Claude Code reported at start: name and status
    /// (connected, failed, needs-auth, pending).
    pub mcp: Vec<(String, String)>,
    /// Files it wrote or edited (as its tools named them).
    pub files: Vec<String>,
    /// Its task list, as it last wrote it (Claude Code's TodoWrite).
    pub todos: Value,
    /// Pull requests and issues it linked to.
    pub links: Vec<String>,
    /// From an earlier run of the console (kept on disk; read-only).
    pub archived: bool,
}

/// The input that answers a question: its own input plus `answers`, each
/// question's text mapped to what was picked. Only the questions it asked
/// can be answered, and each answer is short text.
fn question_input(a: &Approval, answers: &Value) -> Result<Value, String> {
    if a.tool != "AskUserQuestion" {
        return Err("answers are only for a question (AskUserQuestion)".into());
    }
    let asked: Vec<&str> = a
        .input
        .get("questions")
        .and_then(Value::as_array)
        .map(|qs| {
            qs.iter()
                .filter_map(|q| q.get("question").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let given = answers
        .as_object()
        .ok_or("answers must map each question to its answer")?;
    if given.is_empty() {
        return Err("pick an answer first".into());
    }
    for (q, v) in given {
        if !asked.contains(&q.as_str()) {
            return Err(format!("it did not ask {:?}", clip(q, 80)));
        }
        match v.as_str() {
            Some(t) if !t.trim().is_empty() && t.len() <= 4000 => {}
            _ => return Err("each answer is non-empty text (4000 characters at most)".into()),
        }
    }
    let mut input = a.input.clone();
    if let Some(o) = input.as_object_mut() {
        o.insert("answers".into(), Value::Object(given.clone()));
    } else {
        return Err("the question has no input to answer".into());
    }
    Ok(input)
}

/// A tool request the agent is waiting on.
#[derive(Debug, Clone, Serialize)]
pub struct Approval {
    pub request_id: String,
    pub tool: String,
    pub input: Value,
    /// Rules Claude Code suggests for "always allow".
    pub suggestions: Value,
    pub ts: u64,
}

/// What the console sends to an interactive agent.
enum Ctl {
    Line(String),
    Close,
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
    interactive: bool,
    open: bool,
    /// User turns sent and not yet answered by a result.
    turns: u32,
    mode: Option<String>,
    model: Option<String>,
    pending: Vec<Approval>,
    next_request: u64,
    /// 0: not asked for learnings, 1: asked, 2: answered.
    reflect_state: u8,
    last_result: String,
    /// The last turn ended in an error (an API error, a failed run): no
    /// learnings are asked for then.
    last_failed: bool,
    reflection: Option<String>,
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    tokens: Option<Tokens>,
    base_tokens: Option<u64>,
    mcp: Vec<(String, String)>,
    files: Vec<String>,
    todos: Value,
    links: Vec<String>,
    act: Activity,
}

/// What a running agent is doing between events, as its stream shows it.
#[derive(Default)]
struct Activity {
    /// Spend estimated from token counts (US$), for runners and logins
    /// that report no cost, and the messages already counted.
    est_usd: f64,
    est_seen: std::collections::HashSet<String>,
    /// The cost shown is an estimate from token prices.
    estimated: bool,
    /// Text a Gemini agent streamed in parts, until its next step.
    pending_text: String,
    /// tool | writing | thinking ("" before its first step: thinking).
    kind: &'static str,
    tool: Option<String>,
    /// Claude Code is compacting its context.
    compacting: bool,
}

impl State {
    /// The status pad's phase: what needs you first, then what it does.
    fn phase(&self) -> (&'static str, Option<String>) {
        match self.status.as_str() {
            "running" => {
                if let Some(a) = self.pending.first() {
                    let kind = if a.tool == "AskUserQuestion" {
                        "question"
                    } else {
                        "approval"
                    };
                    (kind, Some(a.tool.clone()))
                } else if self.act.compacting {
                    ("compacting", None)
                } else if self.act.kind == "tool" {
                    ("tool", self.act.tool.clone())
                } else if self.act.kind == "writing" {
                    ("writing", None)
                } else {
                    ("thinking", None)
                }
            }
            "failed" => ("error", None),
            "stopped" => ("stopped", None),
            _ if self.exit_code.is_some_and(|c| c != 0) || self.last_failed => ("error", None),
            _ => ("done", None),
        }
    }
}

impl State {
    fn new(format: &str, interactive: bool, agent: &AgentOpts) -> State {
        State {
            status: "running".into(),
            exit_code: None,
            ended: None,
            events: VecDeque::new(),
            seq: 0,
            tools: BTreeMap::new(),
            agents: Vec::new(),
            cost_usd: None,
            format: format.to_string(),
            host: None,
            session: None,
            interactive,
            open: interactive,
            turns: u32::from(interactive),
            mode: agent.permission_mode.clone(),
            model: agent.model.clone(),
            pending: Vec::new(),
            next_request: 0,
            reflect_state: 0,
            last_result: String::new(),
            last_failed: false,
            reflection: None,
            context_tokens: None,
            context_window: None,
            tokens: None,
            base_tokens: None,
            mcp: Vec::new(),
            files: Vec::new(),
            todos: Value::Null,
            links: Vec::new(),
            act: Activity::default(),
        }
    }
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
    opts: AgentOpts,
    profile: Option<String>,
    /// Secret values this agent was given, and their names, for redaction.
    redact: Vec<(String, String)>,
    /// Lines for an interactive agent's stdin (local) or the node (remote).
    ctl: Mutex<Option<mpsc::UnboundedSender<Ctl>>>,
    /// Memory recalled into its prompt.
    memory: Vec<crate::deck::knowledge::MemRef>,
    /// Ask it for learnings before closing its input.
    reflect: bool,
    /// The account it runs with (`login:<agent>@<node>`: the machine's own).
    account: Option<String>,
    /// Restored from disk: it ran under an earlier console.
    archived: bool,
}

fn now() -> u64 {
    crate::deck::runs::now()
}

/// GitHub pull request and issue links in a text.
fn links_in(text: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"https://github\.com/[\w.-]+/[\w.-]+/(?:pull|issues)/\d+").unwrap()
    });
    re.find_iter(text).map(|m| m.as_str().to_string()).collect()
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

/// What is kept of a finished session, so it is still there after the
/// console restarts (events are already redacted).
#[derive(Serialize, Deserialize)]
struct Saved {
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
    opts: AgentOpts,
    profile: Option<String>,
    account: Option<String>,
    status: String,
    exit_code: Option<i32>,
    ended: Option<u64>,
    events: Vec<Event>,
    tools: BTreeMap<String, usize>,
    agents: Vec<Value>,
    cost_usd: Option<f64>,
    format: String,
    host: Option<String>,
    session: Option<String>,
    mode: Option<String>,
    model: Option<String>,
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    #[serde(default)]
    tokens: Option<Tokens>,
    #[serde(default)]
    base_tokens: Option<u64>,
    mcp: Vec<(String, String)>,
    files: Vec<String>,
    todos: Value,
    links: Vec<String>,
}

/// Sessions kept on disk: the newest this many come back at start.
const KEEP_SESSIONS: usize = 300;

fn sessions_dir(log_dir: &Path) -> PathBuf {
    log_dir.join("sessions")
}

impl Proc {
    /// Keep this finished session on disk (owner-only), next to its log.
    fn save(&self) {
        let Some(dir) = self.log.parent().map(sessions_dir) else {
            return;
        };
        let saved = {
            let st = self.state.lock().unwrap();
            Saved {
                id: self.id,
                node: self.node.clone(),
                parent: self.parent,
                resumed: self.resumed.clone(),
                can_resume: self.can_resume,
                runner: self.runner.clone(),
                harness: self.harness.clone(),
                prompt: self.redacted(&self.prompt),
                cwd: self.cwd.clone(),
                command: self.command.clone(),
                started: self.started,
                log: self.log.clone(),
                opts: self.opts.clone(),
                profile: self.profile.clone(),
                account: self.account.clone(),
                status: st.status.clone(),
                exit_code: st.exit_code,
                ended: st.ended,
                events: st.events.iter().cloned().collect(),
                tools: st.tools.clone(),
                agents: st
                    .agents
                    .iter()
                    .filter_map(|a| serde_json::to_value(a).ok())
                    .collect(),
                cost_usd: st.cost_usd,
                format: st.format.clone(),
                host: st.host.clone(),
                session: st.session.clone(),
                mode: st.mode.clone(),
                model: st.model.clone(),
                context_tokens: st.context_tokens,
                context_window: st.context_window,
                tokens: st.tokens.clone(),
                base_tokens: st.base_tokens,
                mcp: st.mcp.clone(),
                files: st.files.clone(),
                todos: st.todos.clone(),
                links: st.links.clone(),
            }
        };
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}-{}.json", self.started, self.id));
        let tmp = path.with_extension("json.tmp");
        let Ok(text) = serde_json::to_string(&saved) else {
            return;
        };
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        if o.open(&tmp)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// A session saved by an earlier console, read-only.
    fn restore(sv: Saved) -> Proc {
        let (tx, _) = broadcast::channel(16);
        let mut st = State::new(&sv.format, false, &sv.opts);
        st.status = sv.status;
        st.exit_code = sv.exit_code;
        st.ended = sv.ended.or(Some(sv.started));
        st.seq = sv.events.last().map(|e| e.seq).unwrap_or(0);
        st.events = sv.events.into();
        st.tools = sv.tools;
        st.agents = sv
            .agents
            .into_iter()
            .filter_map(|a| {
                Some(SubAgent {
                    id: a.get("id")?.as_str()?.into(),
                    kind: a.get("type")?.as_str()?.into(),
                    description: a
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                    state: match a.get("state").and_then(Value::as_str) {
                        Some("error") => "error",
                        _ => "done",
                    },
                    started: a.get("started").and_then(Value::as_u64).unwrap_or(0),
                    tools: a.get("tools").and_then(Value::as_u64).unwrap_or(0) as usize,
                })
            })
            .collect();
        st.cost_usd = sv.cost_usd;
        st.host = sv.host;
        st.session = sv.session;
        st.mode = sv.mode;
        st.model = sv.model;
        st.context_tokens = sv.context_tokens;
        st.context_window = sv.context_window;
        st.tokens = sv.tokens;
        st.base_tokens = sv.base_tokens;
        st.mcp = sv.mcp;
        st.files = sv.files;
        st.todos = sv.todos;
        st.links = sv.links;
        st.open = false;
        st.turns = 0;
        Proc {
            id: sv.id,
            node: sv.node,
            parent: sv.parent,
            resumed: sv.resumed,
            can_resume: sv.can_resume,
            runner: sv.runner,
            harness: sv.harness,
            prompt: sv.prompt,
            cwd: sv.cwd,
            command: sv.command,
            started: sv.started,
            log: sv.log,
            state: Mutex::new(st),
            tx,
            stop: Mutex::new(None),
            opts: sv.opts,
            profile: sv.profile,
            redact: Vec::new(),
            ctl: Mutex::new(None),
            memory: Vec::new(),
            reflect: false,
            account: sv.account,
            archived: true,
        }
    }

    /// `text` with the secret values this agent was given replaced by names.
    fn redacted(&self, text: &str) -> String {
        if self.redact.is_empty() {
            text.to_string()
        } else {
            crate::deck::secrets::redact(text, &self.redact)
        }
    }

    fn push(&self, kind: &str, text: String, agent: Option<String>) {
        let text = self.redacted(&text);
        let ev = {
            let mut st = self.state.lock().unwrap();
            // Pull requests and issues it mentions, for the side panel.
            if text.contains("github.com/") {
                for u in links_in(&text) {
                    if !st.links.contains(&u) && st.links.len() < 50 {
                        st.links.push(u);
                    }
                }
            }
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
        let (phase, activity) = st.phase();
        ProcView {
            id: self.id,
            node: self.node.clone(),
            runner: self.runner.clone(),
            harness: self.harness.clone(),
            // What was asked; the memory recalled into it is listed apart.
            prompt: crate::deck::knowledge::without_memory(&self.prompt).to_string(),
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
            phase,
            activity,
            exit_code: st.exit_code,
            events: st.seq,
            tools: st.tools.clone(),
            agents: st.agents.clone(),
            cost_usd: st.cost_usd,
            cost_estimated: st.act.estimated,
            log: self.log.display().to_string(),
            last: st
                .events
                .iter()
                .rev()
                .find(|e| e.kind != "stderr")
                .map(|e| clip(&e.text, 160)),
            opts: self.opts.clone(),
            interactive: st.interactive,
            open: st.open && st.status == "running",
            mode: st.mode.clone(),
            model: st.model.clone(),
            pending: st.pending.clone(),
            memory: self.memory.clone(),
            reflect: self.reflect,
            account: self.account.clone(),
            reflection: match st.reflect_state {
                0 => "none",
                1 => "asked",
                _ => "received",
            },
            context_tokens: st.context_tokens,
            context_window: st.context_window,
            tokens: st.tokens.clone(),
            base_tokens: st.base_tokens,
            mcp: st.mcp.clone(),
            files: st.files.clone(),
            todos: st.todos.clone(),
            links: st.links.clone(),
            archived: self.archived,
        }
    }

    /// What it answered when asked for learnings; given out once.
    pub fn take_reflection(&self) -> Option<String> {
        self.state.lock().unwrap().reflection.take()
    }

    pub fn opts(&self) -> &AgentOpts {
        &self.opts
    }

    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    fn send(&self, c: Ctl) -> Result<(), String> {
        match self.ctl.lock().unwrap().as_ref() {
            Some(tx) => tx
                .send(c)
                .map_err(|_| "the agent's input is closed".to_string()),
            None => Err("this agent does not take input (only Claude Code runners do)".into()),
        }
    }

    fn check_open(&self) -> Result<(), String> {
        let st = self.state.lock().unwrap();
        if st.status != "running" {
            return Err("the agent is not running".into());
        }
        if !st.interactive || !st.open {
            return Err(if st.interactive {
                "the agent has finished its work; use Pivot > Continue to give it more".into()
            } else {
                "this agent is not driven interactively (only Claude Code runners are)".into()
            });
        }
        Ok(())
    }

    /// Answer a tool request: allow (once, or always for this session, with
    /// the rule Claude Code suggested) or deny with a reason. A question
    /// (`AskUserQuestion`) is answered by allowing it with `answers`: the
    /// question's text to the label (or labels, joined by ", ") picked.
    pub fn answer(
        &self,
        request_id: &str,
        allow: bool,
        always: bool,
        message: Option<&str>,
        answers: Option<&Value>,
        by: &str,
    ) -> Result<(), String> {
        self.check_open()?;
        let a = {
            let mut st = self.state.lock().unwrap();
            let i = st
                .pending
                .iter()
                .position(|a| a.request_id == request_id)
                .ok_or("no such pending request (already answered?)")?;
            let input = match answers {
                Some(ans) if allow => Some(question_input(&st.pending[i], ans)?),
                _ => None,
            };
            let mut a = st.pending.remove(i);
            if let Some(input) = input {
                a.input = input;
            }
            a
        };
        let response = if allow {
            let mut r = serde_json::json!({ "behavior": "allow", "updatedInput": a.input });
            if always
                && a.suggestions
                    .as_array()
                    .map(|x| !x.is_empty())
                    .unwrap_or(false)
            {
                r["updatedPermissions"] = a.suggestions.clone();
            }
            r
        } else {
            serde_json::json!({ "behavior": "deny", "message": message.filter(|m| !m.trim().is_empty()).unwrap_or("Denied in the indra console.") })
        };
        let line = serde_json::json!({ "type": "control_response", "response": { "subtype": "success", "request_id": a.request_id, "response": response } });
        self.send(Ctl::Line(line.to_string()))?;
        let said = match (allow, a.input.get("answers").and_then(Value::as_object)) {
            (true, Some(ans)) if answers.is_some() => format!(
                "answered: {}",
                ans.values()
                    .filter_map(Value::as_str)
                    .map(|t| clip(t, 120))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            _ => format!(
                "{} {}{}{}",
                if allow { "allowed" } else { "denied" },
                a.tool,
                if allow && always {
                    " (always, for this session)"
                } else {
                    ""
                },
                if by.is_empty() {
                    String::new()
                } else {
                    format!(" (decided by {})", by)
                }
            ),
        };
        self.push("permission", said, None);
        Ok(())
    }

    /// Ask the agent to change: `set_permission_mode`, `set_model` or `interrupt`.
    pub fn control(&self, subtype: &str, extra: Value) -> Result<(), String> {
        self.check_open()?;
        let id = {
            let mut st = self.state.lock().unwrap();
            st.next_request += 1;
            format!("indra-{}", st.next_request)
        };
        let mut req = serde_json::json!({ "subtype": subtype });
        if let (Some(r), Some(e)) = (req.as_object_mut(), extra.as_object()) {
            r.extend(e.clone());
        }
        let line =
            serde_json::json!({ "type": "control_request", "request_id": id, "request": req });
        self.send(Ctl::Line(line.to_string()))?;
        self.push(
            "system",
            format!("asked the agent to {}", subtype.replace('_', " ")),
            None,
        );
        Ok(())
    }

    /// A follow-up message, in the same session, answered after the current turn.
    pub fn say(&self, text: &str) -> Result<(), String> {
        self.check_open()?;
        self.send(Ctl::Line(crate::deck::nodes::user_message(text)))?;
        self.state.lock().unwrap().turns += 1;
        self.push("user", text.to_string(), None);
        Ok(())
    }

    /// A turn ended: close the agent's input once nothing more is queued,
    /// so it finishes and exits as a one-shot agent would.
    fn turn_done(&self) {
        enum Next {
            Wait,
            Reflect,
            Close,
        }
        let next = {
            let mut st = self.state.lock().unwrap();
            st.turns = st.turns.saturating_sub(1);
            if st.reflect_state == 1 && st.turns == 0 {
                st.reflect_state = 2;
                // An error is not a learning.
                let r = std::mem::take(&mut st.last_result);
                if !st.last_failed {
                    st.reflection = Some(r);
                }
            }
            if !(st.interactive && st.open && st.turns == 0) {
                Next::Wait
            } else if self.reflect
                && st.reflect_state == 0
                && st.status == "running"
                && !st.last_failed
            {
                st.reflect_state = 1;
                st.turns += 1;
                Next::Reflect
            } else {
                st.open = false;
                st.pending.clear();
                Next::Close
            }
        };
        match next {
            Next::Wait => {}
            Next::Reflect => {
                if self
                    .send(Ctl::Line(crate::deck::nodes::user_message(
                        crate::deck::knowledge::REFLECT_PROMPT,
                    )))
                    .is_ok()
                {
                    self.push(
                        "system",
                        "asked for learnings before finishing".into(),
                        None,
                    );
                } else {
                    self.state.lock().unwrap().reflect_state = 2;
                }
            }
            Next::Close => {
                let _ = self.send(Ctl::Close);
            }
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
        let format = self.state.lock().unwrap().format.clone();
        match format.as_str() {
            "claude-stream-json" => self.claude_line(line),
            "codex-json" => self.codex_line(line),
            "gemini-stream-json" => self.gemini_line(line),
            _ if !line.trim().is_empty() => self.push("text", line.to_string(), None),
            _ => {}
        }
    }

    /// Add `usd` (estimated from tokens) to what the agent spent.
    fn add_estimate(&self, usd: f64) {
        let mut st = self.state.lock().unwrap();
        st.act.est_usd += usd;
        st.act.estimated = true;
        st.cost_usd = Some(st.act.est_usd);
    }

    /// Stop the agent once it has spent its limit, as indra counts it: for
    /// runners without a limit of their own (Codex, Gemini CLI), and for
    /// Claude Code under a subscription login.
    fn check_limit(&self) {
        let Some(limit) = self.opts.max_budget_usd else {
            return;
        };
        let (spent, running) = {
            let st = self.state.lock().unwrap();
            (st.cost_usd.unwrap_or(0.0), st.status == "running")
        };
        if running && spent >= limit {
            self.push(
                "error",
                format!(
                    "stopped: it reached its spending limit of ${} (${:.2} as indra counts it)",
                    crate::deck::nodes::budget_arg(limit),
                    spent
                ),
                None,
            );
            self.request_stop();
        }
    }

    /// The model a Codex agent runs on: as launched, else Codex's default
    /// (priced as gpt-5).
    fn model_or(&self, dflt: &str) -> String {
        self.state
            .lock()
            .unwrap()
            .model
            .clone()
            .or_else(|| self.opts.model.clone())
            .unwrap_or_else(|| dflt.to_string())
    }

    /// One line of `codex exec --json`.
    fn codex_line(&self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            if !line.trim().is_empty() {
                self.push("text", line.to_string(), None);
            }
            return;
        };
        let s = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match s(&v, "type").as_str() {
            "thread.started" => {
                let id = s(&v, "thread_id");
                if crate::deck::nodes::validate_session(&id).is_ok() {
                    self.state.lock().unwrap().session = Some(id.clone());
                }
                self.push("system", format!("session started ({})", id), None);
            }
            "turn.started" => self.state.lock().unwrap().act.kind = "thinking",
            "item.started" | "item.completed" => {
                let done = s(&v, "type") == "item.completed";
                let it = v.get("item").cloned().unwrap_or(Value::Null);
                match s(&it, "type").as_str() {
                    "agent_message" if done => {
                        let t = s(&it, "text");
                        {
                            let mut st = self.state.lock().unwrap();
                            st.act.kind = "writing";
                            st.act.tool = None;
                            st.last_result = t.clone();
                        }
                        if !t.trim().is_empty() {
                            self.push("text", t, None);
                        }
                    }
                    "command_execution" if !done => {
                        let c = serde_json::json!({ "name": "Bash", "input": { "command": s(&it, "command") } });
                        self.tool_use(&c, None, None);
                    }
                    "command_execution" => {
                        self.state.lock().unwrap().act.kind = "thinking";
                        let code = it.get("exit_code").and_then(Value::as_i64);
                        if code.is_some_and(|c| c != 0) {
                            self.push(
                                "error",
                                format!(
                                    "command exited {}: {}",
                                    code.unwrap_or(-1),
                                    clip(&s(&it, "aggregated_output"), 600)
                                ),
                                None,
                            );
                        }
                    }
                    "file_change" if done => {
                        let mut st = self.state.lock().unwrap();
                        for ch in it
                            .get("changes")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                        {
                            let f = s(&ch, "path");
                            if !f.is_empty() && !st.files.contains(&f) && st.files.len() < 500 {
                                st.files.push(f);
                            }
                        }
                        drop(st);
                        self.push("tool", "edited files".into(), None);
                    }
                    "mcp_tool_call" if !done => {
                        let c = serde_json::json!({ "name": format!("mcp__{}__{}", s(&it, "server"), s(&it, "tool")),
                                        "input": it.get("arguments").cloned().unwrap_or(Value::Null) });
                        self.tool_use(&c, None, None);
                    }
                    "web_search" if !done => {
                        let c = serde_json::json!({ "name": "WebSearch", "input": { "query": s(&it, "query") } });
                        self.tool_use(&c, None, None);
                    }
                    "todo_list" => {
                        self.state.lock().unwrap().todos =
                            it.get("items").cloned().unwrap_or(Value::Null);
                    }
                    "error" if done => {
                        let m = s(&it, "message");
                        // Codex says so when indra's hooks run without review.
                        let kind = if m.contains("bypass-hook-trust") {
                            "system"
                        } else {
                            "error"
                        };
                        self.push(kind, m, None);
                    }
                    _ => {}
                }
            }
            "turn.completed" => {
                let u = v.get("usage").cloned().unwrap_or(Value::Null);
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                let cached = n("cached_input_tokens");
                let usage = crate::deck::pricing::Usage {
                    input: n("input_tokens").saturating_sub(cached),
                    cached,
                    cache_write: n("cache_write_input_tokens"),
                    output: n("output_tokens"),
                };
                {
                    let mut st = self.state.lock().unwrap();
                    let t = st.tokens.get_or_insert_with(Tokens::default);
                    t.input += usage.input;
                    t.output += usage.output;
                    t.cache_read += usage.cached;
                    t.cache_write += usage.cache_write;
                    st.act.kind = "thinking";
                }
                let model = self.model_or("gpt-5");
                let cost =
                    crate::deck::pricing::cost(&model, usage, &crate::deck::pricing::table());
                match cost {
                    Some(c) => self.add_estimate(c),
                    None => self.push(
                        "system",
                        format!(
                            "spend not counted: no price for model {} (add a [[price]] for it)",
                            model
                        ),
                        None,
                    ),
                }
                let r = self.state.lock().unwrap().last_result.clone();
                let mut text = format!(
                    "turn done: {} tokens in, {} out",
                    usage.input + usage.cached,
                    usage.output
                );
                if let Some(c) = cost {
                    text.push_str(&format!(
                        ", about ${:.4}",
                        self.state.lock().unwrap().cost_usd.unwrap_or(c)
                    ));
                }
                if !r.is_empty() {
                    text.push_str("\n\n");
                    text.push_str(&r);
                }
                self.push("result", text, None);
                self.check_limit();
            }
            "turn.failed" | "error" => {
                let m = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .unwrap_or_else(|| s(&v, "message"));
                self.state.lock().unwrap().last_failed = true;
                self.push("error", m, None);
            }
            _ => {}
        }
    }

    /// Text a Gemini agent streamed, as one message.
    fn flush_text(&self) {
        let t = std::mem::take(&mut self.state.lock().unwrap().act.pending_text);
        if !t.trim().is_empty() {
            self.state.lock().unwrap().last_result = t.clone();
            self.push("text", t, None);
        }
    }

    /// One line of `gemini --output-format stream-json`.
    fn gemini_line(&self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            if !line.trim().is_empty() {
                self.push("text", line.to_string(), None);
            }
            return;
        };
        let s = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match s(&v, "type").as_str() {
            "init" => {
                let model = s(&v, "model");
                {
                    let mut st = self.state.lock().unwrap();
                    let sid = s(&v, "session_id");
                    if crate::deck::nodes::validate_session(&sid).is_ok() {
                        st.session = Some(sid);
                    }
                    if !model.is_empty() {
                        st.model = Some(model.clone());
                    }
                }
                self.push("system", format!("session started, model {}", model), None);
            }
            "message" if s(&v, "role") == "assistant" => {
                let mut st = self.state.lock().unwrap();
                st.act.kind = "writing";
                st.act.tool = None;
                st.act.pending_text.push_str(&s(&v, "content"));
            }
            "tool_use" => {
                self.flush_text();
                let c = serde_json::json!({ "name": s(&v, "tool_name"), "input": v.get("parameters").cloned().unwrap_or(Value::Null) });
                self.tool_use(&c, None, None);
            }
            "tool_result" => {
                self.state.lock().unwrap().act.kind = "thinking";
                if s(&v, "status") == "error" {
                    let m = v
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .unwrap_or_else(|| s(&v, "output"));
                    self.push("error", clip(&m, 600), None);
                }
            }
            "error" => {
                self.flush_text();
                let kind = if s(&v, "severity") == "warning" {
                    "system"
                } else {
                    "error"
                };
                self.push(kind, s(&v, "message"), None);
            }
            "result" => {
                self.flush_text();
                let stats = v.get("stats").cloned().unwrap_or(Value::Null);
                let mut total = 0.0;
                let mut known = true;
                let mut all = crate::deck::pricing::Usage::default();
                let models = stats
                    .get("models")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for (m, x) in &models {
                    let n = |k: &str| x.get(k).and_then(Value::as_u64).unwrap_or(0);
                    let cached = n("cached");
                    let u = crate::deck::pricing::Usage {
                        input: n("input_tokens").saturating_sub(cached),
                        cached,
                        cache_write: 0,
                        output: n("output_tokens"),
                    };
                    all.input += u.input;
                    all.cached += u.cached;
                    all.output += u.output;
                    match crate::deck::pricing::cost(m, u, &crate::deck::pricing::table()) {
                        Some(c) => total += c,
                        None => known = false,
                    }
                }
                {
                    let mut st = self.state.lock().unwrap();
                    let t = st.tokens.get_or_insert_with(Tokens::default);
                    t.input += all.input;
                    t.output += all.output;
                    t.cache_read += all.cached;
                    st.last_failed = s(&v, "status") != "success";
                }
                if !models.is_empty() {
                    self.add_estimate(total);
                }
                if !known {
                    self.push(
                        "system",
                        "spend not fully counted: a model has no price (add a [[price]] for it)"
                            .into(),
                        None,
                    );
                }
                let r = self.state.lock().unwrap().last_result.clone();
                let mut text = format!(
                    "{}: {} tokens in, {} out, {} tool calls, about ${:.4}",
                    s(&v, "status"),
                    all.input + all.cached,
                    all.output,
                    stats.get("tool_calls").and_then(Value::as_u64).unwrap_or(0),
                    total
                );
                if let Some(e) = v.pointer("/error/message").and_then(Value::as_str) {
                    text.push_str(&format!(" ({})", e));
                }
                if !r.is_empty() {
                    text.push_str("\n\n");
                    text.push_str(&r);
                }
                self.push("result", text, None);
                self.check_limit();
            }
            _ => {}
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
                    if v.get("interactive")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        st.interactive = true;
                        st.open = true;
                        st.turns = 1;
                    }
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

    /// A note from indra in its event stream.
    pub fn note_system(&self, text: String) {
        self.push("system", text, None);
    }

    /// A steer the console queued: shown as the person's own words.
    pub fn note_steer(&self, text: &str) {
        self.push(
            "user",
            format!("{}\n\n(steer: read after its next tool call)", text),
            None,
        );
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

    /// Everything it printed (not stderr), keeping the last `max` characters:
    /// a shell step's output, for conditions and later steps.
    pub fn text_output(&self, max: usize) -> String {
        let st = self.state.lock().unwrap();
        let all: Vec<&str> = st
            .events
            .iter()
            .filter(|e| e.kind == "text" || e.kind == "result")
            .map(|e| e.text.as_str())
            .collect();
        let s = all.join("\n");
        let n = s.chars().count();
        if n > max {
            s.chars().skip(n - max).collect()
        } else {
            s
        }
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
                {
                    let mut st = self.state.lock().unwrap();
                    if !model.is_empty() {
                        st.model = Some(model.clone());
                    }
                    if let Some(m) = v.get("permissionMode").and_then(Value::as_str) {
                        st.mode = Some(m.to_string());
                    }
                }
                self.push(
                    "system",
                    format!(
                        "session started, model {}",
                        if model.is_empty() { "?" } else { &model }
                    ),
                    agent,
                );
                // Its MCP servers: a missing one is otherwise silent.
                if parent.is_none() {
                    let mcp = mcp_servers(&v);
                    if !mcp.is_empty() {
                        self.push(
                            "system",
                            format!(
                                "MCP servers: {}",
                                mcp.iter()
                                    .map(|(n, st)| format!("{} ({})", n, st))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            None,
                        );
                        for (n, st) in mcp
                            .iter()
                            .filter(|(_, st)| st != "connected" && st != "pending")
                        {
                            self.push(
                                "error",
                                format!("MCP server {} is {}: {}", n, st, mcp_hint(st)),
                                None,
                            );
                        }
                    }
                    self.state.lock().unwrap().mcp = mcp;
                }
            }
            "assistant" => {
                // Every model call, a subagent's included, estimated from its
                // tokens (each call once, though it streams in parts): the
                // spend while a turn runs, and the spend at all under a
                // subscription login.
                if let (Some(u), Some(id)) = (
                    v.pointer("/message/usage"),
                    v.pointer("/message/id").and_then(Value::as_str),
                ) {
                    let first = self
                        .state
                        .lock()
                        .unwrap()
                        .act
                        .est_seen
                        .insert(id.to_string());
                    if first {
                        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                        let usage = crate::deck::pricing::Usage {
                            input: n("input_tokens"),
                            cached: n("cache_read_input_tokens"),
                            cache_write: n("cache_creation_input_tokens"),
                            output: n("output_tokens"),
                        };
                        let model = v
                            .pointer("/message/model")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .unwrap_or_else(|| self.model_or("sonnet"));
                        if let Some(c) = crate::deck::pricing::cost(
                            &model,
                            usage,
                            &crate::deck::pricing::table(),
                        ) {
                            let mut st = self.state.lock().unwrap();
                            st.act.est_usd += c;
                            // Claude's own figure wins once a turn reports one.
                            if st.cost_usd.is_none() || st.act.estimated {
                                st.act.estimated = true;
                                st.cost_usd = Some(st.act.est_usd);
                            }
                        }
                        self.check_limit();
                    }
                }
                if parent.is_none() {
                    if let Some(u) = v.pointer("/message/usage") {
                        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                        let total = n("input_tokens")
                            + n("cache_read_input_tokens")
                            + n("cache_creation_input_tokens")
                            + n("output_tokens");
                        if total > 0 {
                            let mut st = self.state.lock().unwrap();
                            st.context_tokens = Some(total);
                            st.base_tokens.get_or_insert(total);
                        }
                    }
                }
                let content = v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if parent.is_none() {
                    self.state.lock().unwrap().act.compacting = false;
                }
                for c in content {
                    match c.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => {
                            let t = s(&c, "text");
                            if !t.trim().is_empty() {
                                if parent.is_none() {
                                    let mut st = self.state.lock().unwrap();
                                    st.act.kind = "writing";
                                    st.act.tool = None;
                                }
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
                    if parent.is_none() {
                        // The tool returned: the model reads the result.
                        let mut st = self.state.lock().unwrap();
                        st.act.kind = "thinking";
                        st.act.tool = None;
                    }
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
                let reported = v.get("total_cost_usd").and_then(Value::as_f64);
                let turns = v.get("num_turns").and_then(Value::as_u64).unwrap_or(0);
                let secs = v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0) / 1000;
                // What Claude Code reports, else (a login it does not price)
                // indra's estimate from the tokens.
                let cost = {
                    let mut st = self.state.lock().unwrap();
                    match reported.filter(|c| *c > 0.0) {
                        Some(c) => {
                            st.cost_usd = Some(c);
                            st.act.estimated = false;
                        }
                        None if st.act.est_usd > 0.0 => {
                            st.cost_usd = Some(st.act.est_usd);
                            st.act.estimated = true;
                        }
                        None => st.cost_usd = reported,
                    }
                    st.cost_usd
                };
                let mut text = format!("{}: {} turns, {}s", s(&v, "subtype"), turns, secs);
                if let Some(c) = cost {
                    text.push_str(&format!(", ${:.4}", c));
                }
                let r = s(&v, "result");
                {
                    let mut st = self.state.lock().unwrap();
                    st.last_result = r.clone();
                    st.last_failed = s(&v, "subtype") != "success"
                        || v.get("is_error").and_then(Value::as_bool) == Some(true);
                    if let Some(u) = v.get("usage") {
                        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                        let t = st.tokens.get_or_insert_with(Tokens::default);
                        t.input += n("input_tokens");
                        t.output += n("output_tokens");
                        t.cache_read += n("cache_read_input_tokens");
                        t.cache_write += n("cache_creation_input_tokens");
                    }
                    // Claude Code reports each model's context window.
                    if let Some(w) = v
                        .get("modelUsage")
                        .and_then(Value::as_object)
                        .and_then(|m| {
                            m.values()
                                .filter_map(|x| x.get("contextWindow").and_then(Value::as_u64))
                                .max()
                        })
                    {
                        st.context_window = Some(w);
                    }
                }
                if !r.is_empty() {
                    text.push_str("\n\n");
                    text.push_str(&r);
                }
                self.push("result", text, agent.clone());
                self.check_limit();
                if s(&v, "subtype") == "error_max_budget_usd" {
                    let limit = self
                        .opts
                        .max_budget_usd
                        .map(|b| format!(" of ${}", crate::deck::nodes::budget_arg(b)))
                        .unwrap_or_default();
                    self.push(
                        "error",
                        format!("stopped: it reached its spending limit{}", limit),
                        agent,
                    );
                }
                self.turn_done();
            }
            "system" if s(&v, "subtype") == "compact_boundary" => {
                self.state.lock().unwrap().act.compacting = false;
            }
            "system" if s(&v, "subtype") == "status" => {
                // Claude Code reports `"status": "compacting"` while it
                // summarises its context, and a null status once done.
                if v.get("status").is_some() {
                    self.state.lock().unwrap().act.compacting = s(&v, "status") == "compacting";
                }
                if let Some(m) = v.get("permissionMode").and_then(Value::as_str) {
                    let changed = {
                        let mut st = self.state.lock().unwrap();
                        let changed = st.mode.as_deref() != Some(m);
                        st.mode = Some(m.to_string());
                        changed
                    };
                    if changed {
                        self.push("system", format!("permission mode is now {}", m), None);
                    }
                }
            }
            // Claude Code asks the host before using a tool.
            "control_request" => {
                let id = s(&v, "request_id");
                let req = v.get("request").cloned().unwrap_or(Value::Null);
                if s(&req, "subtype") == "can_use_tool" {
                    let tool = s(&req, "tool_name");
                    let input = req.get("input").cloned().unwrap_or(Value::Null);
                    let summary = tool_summary(&tool, &input);
                    // Approval rules decide some Bash requests without asking.
                    let verdict = self.opts.approvals.decide(&tool, &input);
                    let decided = match &verdict {
                        crate::deck::approve::Verdict::Allow(rule) => Some((
                            serde_json::json!({ "behavior": "allow", "updatedInput": input }),
                            format!(
                                "allowed {}{} (decided by the rule `{}`)",
                                tool, summary, rule
                            ),
                        )),
                        crate::deck::approve::Verdict::Deny(rule) => Some((
                            serde_json::json!({ "behavior": "deny", "message": format!(
                                "Refused by the approval rule `{}` set in the indra console. Do not try to get around it; ask the operator if it is needed.", rule) }),
                            format!(
                                "denied {}{} (decided by the rule `{}`)",
                                tool, summary, rule
                            ),
                        )),
                        crate::deck::approve::Verdict::Ask => None,
                    };
                    if let Some((response, said)) = decided {
                        let line = serde_json::json!({ "type": "control_response", "response": {
                            "subtype": "success", "request_id": id, "response": response } });
                        if self.send(Ctl::Line(line.to_string())).is_ok() {
                            self.push("permission", said, agent);
                            return;
                        }
                    }
                    self.state.lock().unwrap().pending.push(Approval {
                        request_id: id,
                        tool: tool.clone(),
                        input,
                        suggestions: req
                            .get("permission_suggestions")
                            .cloned()
                            .unwrap_or(Value::Array(vec![])),
                        ts: now(),
                    });
                    self.push(
                        "permission",
                        format!("asks to use {}{}", tool, summary),
                        agent,
                    );
                } else {
                    // Nothing else is supported; say so rather than leave it waiting.
                    let line = serde_json::json!({ "type": "control_response", "response": {
                        "subtype": "error", "request_id": id, "error": "not supported by indra" } });
                    let _ = self.send(Ctl::Line(line.to_string()));
                }
            }
            // Answers to the console's own requests.
            "control_response" => {
                let r = v.get("response").cloned().unwrap_or(Value::Null);
                let id = s(&r, "request_id");
                if id == "indra-init" {
                    return;
                }
                if s(&r, "subtype") == "error" {
                    self.push(
                        "error",
                        format!("the agent refused: {}", s(&r, "error")),
                        None,
                    );
                } else if let Some(m) = r.pointer("/response/model").and_then(Value::as_str) {
                    self.state.lock().unwrap().model = Some(m.to_string());
                    self.push("system", format!("model is now {}", m), None);
                } else if let Some(m) = r.pointer("/response/mode").and_then(Value::as_str) {
                    let changed = {
                        let mut st = self.state.lock().unwrap();
                        let changed = st.mode.as_deref() != Some(m);
                        st.mode = Some(m.to_string());
                        changed
                    };
                    if changed {
                        self.push("system", format!("permission mode is now {}", m), None);
                    }
                }
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
        {
            // A subagent's step counts too: the session is at work on a tool.
            let mut st = self.state.lock().unwrap();
            st.act.kind = "tool";
            st.act.tool = Some(name.clone());
        }
        // What the side panel shows: its task list, and the files it touched.
        if agent.is_none() {
            let mut st = self.state.lock().unwrap();
            if name == "TodoWrite" {
                if let Some(t) = input.get("todos") {
                    st.todos = t.clone();
                }
            }
            if ["Edit", "Write", "MultiEdit", "NotebookEdit"].contains(&name.as_str()) {
                if let Some(f) = field("file_path").or_else(|| field("notebook_path")) {
                    if !st.files.contains(&f) && st.files.len() < 500 {
                        st.files.push(f);
                    }
                }
            }
        }
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
    /// Secrets that profiles reference (`secret:<name>`), next to the logs.
    vault: Arc<crate::deck::secrets::Vault>,
    /// Nodes that dialled in (`indra node connect`).
    links: Arc<crate::deck::link::Links>,
    /// Accounts and routing (set by the console and `indra run`).
    accounts: std::sync::OnceLock<Arc<crate::deck::accounts::Accounts>>,
    /// `[approvals]`: rules every agent started from here is decided by.
    approvals: Mutex<crate::deck::approve::Rules>,
}

/// Extra launch options: a pivot's session and parent, and the agent options.
#[derive(Debug, Clone, Default)]
pub struct StartOpts {
    pub resume: Option<Resume>,
    pub parent: Option<u64>,
    /// Model, permission mode, tools and environment (from a profile).
    /// Environment values may be `secret:<name>` references.
    pub agent: AgentOpts,
    /// The profile it came from, for secret scopes.
    pub profile: Option<String>,
    /// Memory notes recalled into its prompt (for its page).
    pub memory: Vec<crate::deck::knowledge::MemRef>,
    /// Ask it for learnings before it finishes (Claude Code runners).
    pub reflect: bool,
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
        let secrets = log_dir
            .parent()
            .map(|d| d.join("secrets"))
            .unwrap_or_else(|| log_dir.join("secrets"));
        // Sessions from earlier runs of the console, newest first.
        let mut saved: Vec<(u64, PathBuf)> = std::fs::read_dir(sessions_dir(&log_dir))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "json"))
                    .filter_map(|p| {
                        let started = p.file_stem()?.to_str()?.split('-').next()?.parse().ok()?;
                        Some((started, p))
                    })
                    .collect()
            })
            .unwrap_or_default();
        saved.sort_by_key(|x| std::cmp::Reverse(x.0));
        let mut restored = BTreeMap::new();
        for (_, p) in saved.into_iter().take(KEEP_SESSIONS) {
            if let Some(sv) = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| serde_json::from_str::<Saved>(&t).ok())
            {
                let proc = Proc::restore(sv);
                restored.insert(proc.id, Arc::new(proc));
            }
        }
        let next = restored.keys().max().map(|m| m + 1).unwrap_or(1);
        Arc::new(Procs {
            procs: Mutex::new(restored),
            next: AtomicU64::new(next),
            log_dir,
            vault: Arc::new(crate::deck::secrets::Vault::open(secrets)),
            links: Arc::new(crate::deck::link::Links::default()),
            accounts: std::sync::OnceLock::new(),
            approvals: Mutex::default(),
        })
    }

    /// Nodes connected over the agent link.
    pub fn links(&self) -> &Arc<crate::deck::link::Links> {
        &self.links
    }

    /// Follow a job on a node over its link: its envelope lines (the
    /// `indra node exec` protocol) feed the process, console input goes to
    /// the node, and Stop asks the node to stop the agent's tree.
    fn drive_linked(
        &self,
        proc: Arc<Proc>,
        node: String,
        job: u64,
        mut lines: mpsc::UnboundedReceiver<String>,
        mut ctl_rx: mpsc::UnboundedReceiver<Ctl>,
        mut stop_rx: oneshot::Receiver<()>,
    ) {
        let logf = Arc::new(Mutex::new(std::fs::File::create(&proc.log).ok()));
        let links = self.links.clone();
        tokio::spawn(async move {
            let mut stopped = false;
            let mut code: Option<i32> = None;
            let mut stop_deadline: Option<tokio::time::Instant> = None;
            loop {
                let wait = async {
                    match stop_deadline {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::select! {
                    l = lines.recv() => match l {
                        Some(l) => {
                            if let Some(f) = logf.lock().unwrap().as_mut() {
                                let _ = writeln!(f, "{}", proc.redacted(&l));
                            }
                            let v: Value = serde_json::from_str(&l).unwrap_or(Value::Null);
                            if v.get("t").and_then(Value::as_str) == Some("exit") {
                                code = v.get("code").and_then(Value::as_i64).map(|c| c as i32);
                                break;
                            }
                            proc.node_line(&l);
                        }
                        None => break,
                    },
                    c = ctl_rx.recv() => match c {
                        Some(Ctl::Line(l)) => links.job_input(&node, job, crate::deck::nodes::JobInput::Line(l)),
                        Some(Ctl::Close) => links.job_input(&node, job, crate::deck::nodes::JobInput::Close),
                        None => {}
                    },
                    _ = &mut stop_rx, if !stopped => {
                        stopped = true;
                        proc.push("system", "stop requested".into(), None);
                        links.job_input(&node, job, crate::deck::nodes::JobInput::Stop);
                        // The node stops the tree and reports the exit; do not wait forever.
                        stop_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(15));
                    }
                    _ = wait => break,
                }
            }
            *proc.ctl.lock().unwrap() = None;
            let text = {
                let mut st = proc.state.lock().unwrap();
                st.ended = Some(now());
                st.exit_code = code;
                st.open = false;
                st.pending.clear();
                st.status = if stopped {
                    "stopped".into()
                } else if code == Some(0) {
                    "exited".into()
                } else {
                    "failed".into()
                };
                for a in st.agents.iter_mut().filter(|a| a.state == "running") {
                    a.state = if stopped { "error" } else { "done" };
                }
                format!(
                    "{} (exit code {})",
                    st.status,
                    code.map(|c| c.to_string()).unwrap_or_else(|| "none".into())
                )
            };
            *proc.stop.lock().unwrap() = None;
            proc.push("exit", text, None);
            proc.save();
        });
    }

    /// The secret store agents' profiles draw from.
    pub fn vault(&self) -> &crate::deck::secrets::Vault {
        &self.vault
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

    #[cfg(all(test, unix))]
    pub fn start(self: &Arc<Self>, l: Launch) -> Result<u64, String> {
        self.start_with(l, StartOpts::default())
    }

    /// Route launches through these accounts.
    /// The `[approvals]` rules for agents started from now on.
    pub fn set_approvals(&self, r: crate::deck::approve::Rules) {
        *self.approvals.lock().unwrap() = r;
    }

    pub fn set_accounts(&self, a: Arc<crate::deck::accounts::Accounts>) {
        let _ = self.accounts.set(a);
    }

    pub fn accounts(&self) -> Option<&Arc<crate::deck::accounts::Accounts>> {
        self.accounts.get()
    }

    /// Start, optionally continuing an earlier session (a pivot) and
    /// recording the process it came from. The agent gets an account's
    /// credential when routing picks one (see `deck::accounts`).
    pub fn start_with(self: &Arc<Self>, l: Launch, mut opts: StartOpts) -> Result<u64, String> {
        let pick = match self.accounts.get() {
            // A local model costs nothing and needs no credential.
            Some(acc) if opts.agent.provider.as_deref() != Some("ollama") => {
                let (name, command, format) = match &l {
                    Launch::Local { runner, .. } => (
                        runner.name.clone(),
                        runner.command.clone(),
                        runner.format.clone(),
                    ),
                    Launch::Remote { runner, .. } => {
                        (runner.to_string(), runner.to_string(), String::new())
                    }
                };
                if name == crate::deck::config::SHELL_RUNNER {
                    None
                } else {
                    let node = match &l {
                        Launch::Local { .. } => crate::deck::nodes::LOCAL.to_string(),
                        Launch::Remote { node, .. } => node.name.clone(),
                    };
                    let agent = crate::deck::accounts::agent_of(&name, &command, &format);
                    acc.sync(self, now());
                    let pin = opts.profile.as_deref().and_then(|p| acc.pin_of(p));
                    let p = acc.pick(&agent, &node, pin.as_deref(), now())?;
                    // The profile's own environment wins.
                    if let Some((var, val)) = &p.env {
                        opts.agent
                            .env
                            .entry(var.clone())
                            .or_insert_with(|| val.clone());
                    }
                    Some(p)
                }
            }
            _ => None,
        };
        // Model fallbacks that name an account: its credential, as a secret
        // reference resolved like the agent's own, under a private name.
        for (i, f) in opts.agent.fallback.iter_mut().enumerate() {
            let Some(name) = f.account.clone() else {
                continue;
            };
            let a = self
                .accounts
                .get()
                .and_then(|acc| acc.named(&name))
                .ok_or_else(|| format!("fallback: no enabled account named '{}'", name))?;
            if a.agent() != "claude" {
                return Err(format!(
                    "fallback: account '{}' is for {}; the model router speaks Anthropic's API, so it takes Claude accounts",
                    name,
                    a.agent()
                ));
            }
            let var = format!("INDRA_FALLBACK_KEY_{}", i);
            opts.agent.env.insert(var.clone(), a.credential.clone());
            f.api_key_env = Some(var);
            f.kind = Some(a.kind.clone().unwrap_or_else(|| "api_key".into()));
        }
        let id = self.start_plain(l, opts, pick.as_ref().map(|p| p.key.clone()))?;
        if let (Some(p), Some(acc)) = (pick, self.accounts.get()) {
            acc.started(id, &p.key, now());
            if let Some(pr) = self.get(id) {
                pr.push("system", format!("account: {} ({})", p.key, p.why), None);
            }
        }
        Ok(id)
    }

    fn start_plain(
        self: &Arc<Self>,
        l: Launch,
        opts: StartOpts,
        account: Option<String>,
    ) -> Result<u64, String> {
        // Resolve the launch into a program, its arguments, and what the UI shows.
        struct Plan {
            program: PathBuf,
            args: Vec<String>,
            cwd: Option<PathBuf>,
            env: Vec<(String, String)>,
            /// Local interactive agent: its first stdin lines.
            control: Option<Vec<String>>,
            /// Remote: the job line for `indra node exec`.
            job: Option<String>,
            /// A private file to delete when the agent exits (its MCP config).
            temp: Option<PathBuf>,
            /// Kept until the agent exits (its proxy, private hook settings).
            keep: crate::deck::nodes::Keep,
            /// Remote over the agent link: the node and the job.
            link: Option<(String, crate::deck::nodes::Job)>,
            node: String,
            runner: String,
            harness: String,
            prompt: String,
            target: String,
            format: String,
            remote: bool,
            can_resume: bool,
        }
        // Secrets: resolved now, for this profile on this node, or the launch
        // fails before anything starts. The process keeps the references; the
        // values go only to the agent, and are redacted from what it prints.
        let node_name = match &l {
            Launch::Local { .. } => crate::deck::nodes::LOCAL.to_string(),
            Launch::Remote { node, .. } => node.name.clone(),
        };
        let (env, mut redact) =
            self.vault
                .resolve(&opts.agent.env, opts.profile.as_deref(), &node_name)?;
        // MCP & tools: their secrets too.
        let (servers, more) =
            crate::deck::integrations::resolve(&opts.agent.mcp_servers, &self.vault, &node_name)?;
        redact.extend(more);
        let mut opts = opts;
        opts.agent.approvals = self.approvals.lock().unwrap().merged(&opts.agent.approvals);
        let shown = opts.agent.clone();
        opts.agent.env = env;
        opts.agent.mcp_servers = servers;
        let plan = match l {
            Launch::Local {
                runner,
                harness,
                prompt,
                target,
            } => {
                let ac = crate::deck::nodes::agent_command(
                    runner,
                    prompt,
                    target,
                    opts.resume.as_ref(),
                    &opts.agent,
                )?;
                if !target.is_dir() {
                    return Err(format!("target is not a directory: {}", target.display()));
                }
                Plan {
                    program: ac.program,
                    args: ac.args,
                    cwd: Some(target.to_path_buf()),
                    env: ac.env,
                    control: ac.control,
                    job: None,
                    temp: ac.temp,
                    keep: ac.keep,
                    link: None,
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
                opts.agent.validate()?;
                let job = crate::deck::nodes::Job {
                    runner: runner.into(),
                    prompt: prompt.replace("{node}", &node.name),
                    target: target.into(),
                    resume: opts.resume.clone(),
                    opts: opts.agent.clone(),
                };
                // A node that dialled in is reached over its link; SSH is the fallback.
                let linked = self.links.connected(&node.name);
                if !linked && node.ssh.is_empty() {
                    return Err(format!(
                        "node '{}' is not connected (run `indra node connect` there) and has no ssh fallback",
                        node.name
                    ));
                }
                let (program, args) = if linked {
                    (PathBuf::from(format!("link:{}", node.name)), Vec::new())
                } else {
                    node.ssh_command_for("exec")?
                };
                Plan {
                    program,
                    args,
                    cwd: None,
                    env: Vec::new(),
                    control: None,
                    temp: None,
                    keep: Default::default(),
                    job: (!linked)
                        .then(|| serde_json::to_string(&job))
                        .transpose()
                        .map_err(|e| e.to_string())?,
                    link: linked.then(|| (node.name.clone(), job.clone())),
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

        // Over the agent link: the job goes to the node; no local process.
        let linked = match &plan.link {
            Some((n, job)) => Some(self.links.start_job(n, job)?),
            None => None,
        };
        if let Some((job_id, lines)) = linked {
            let (ctl_tx, ctl_rx) = mpsc::unbounded_channel::<Ctl>();
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
                command: vec![
                    format!("indra node connect ({})", plan.node),
                    plan.runner.clone(),
                ],
                started,
                log: log.clone(),
                state: Mutex::new(State::new(&plan.format, false, &opts.agent)),
                tx,
                stop: Mutex::new(Some(stop_tx)),
                opts: shown,
                profile: opts.profile.clone(),
                redact: redact.clone(),
                ctl: Mutex::new(Some(ctl_tx)),
                memory: opts.memory.clone(),
                reflect: opts.reflect,
                account: account.clone(),
                archived: false,
            });
            let used: Vec<String> = redact.iter().map(|(_, n)| n.clone()).collect();
            self.vault
                .record_use(&used, opts.profile.as_deref(), &node_name, id);
            self.procs.lock().unwrap().insert(id, proc.clone());
            proc.push(
                "system",
                format!(
                    "started {} on node {} in {} (over its link)",
                    plan.runner, plan.node, plan.target
                ),
                None,
            );
            self.drive_linked(proc, plan.node.clone(), job_id, lines, ctl_rx, stop_rx);
            return Ok(id);
        }

        let mut cmd = tokio::process::Command::new(&plan.program);
        cmd.args(&plan.args)
            .envs(plan.env.iter().map(|(k, v)| (k, v)))
            .stdin(if plan.job.is_some() || plan.control.is_some() {
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
        let mut child = cmd.spawn().map_err(|e| {
            if let Some(t) = &plan.temp {
                let _ = std::fs::remove_file(t);
            }
            format!("cannot start {}: {}", plan.program.display(), e)
        })?;
        // Stdin carries the remote job (and then control lines wrapped for the
        // node), or a local interactive agent's control lines. A writer task
        // owns it; a remote job lives as long as it stays open.
        let child_stdin = child.stdin.take();
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel::<Ctl>();
        let interactive = plan.control.is_some();

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
            state: Mutex::new(State::new(&plan.format, interactive, &opts.agent)),
            tx,
            stop: Mutex::new(Some(stop_tx)),
            opts: shown,
            profile: opts.profile.clone(),
            redact: redact.clone(),
            ctl: Mutex::new((plan.job.is_some() || interactive).then(|| ctl_tx.clone())),
            memory: opts.memory.clone(),
            reflect: opts.reflect,
            account: account.clone(),
            archived: false,
        });
        let used: Vec<String> = redact.iter().map(|(_, n)| n.clone()).collect();
        self.vault
            .record_use(&used, opts.profile.as_deref(), &node_name, id);
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
                    let _ = writeln!(f, "{}", p1.redacted(&line));
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
                    let _ = writeln!(f, "[stderr] {}", p2.redacted(&line));
                }
                if !line.trim().is_empty() {
                    p2.push("stderr", line, None);
                }
            }
        });

        // The writer: first the job (remote) or the opening lines (local
        // interactive), then whatever the console sends, until closed.
        let (pw, job, first) = (proc.clone(), plan.job.clone(), plan.control.clone());
        let (writer_stop_tx, writer_stop) = oneshot::channel::<()>();
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let Some(mut w) = child_stdin else { return };
            let mut ctl_rx = ctl_rx;
            let mut writer_stop = writer_stop;
            if let Some(job) = job {
                if let Err(e) = w.write_all(format!("{}\n", job).as_bytes()).await {
                    pw.push(
                        "error",
                        format!("could not send the job to the node: {}", e),
                        None,
                    );
                }
            }
            for l in first.unwrap_or_default() {
                let _ = w.write_all(format!("{}\n", l).as_bytes()).await;
            }
            let _ = w.flush().await;
            loop {
                tokio::select! {
                    _ = &mut writer_stop => return,
                    msg = ctl_rx.recv() => match msg {
                        Some(Ctl::Line(l)) => {
                            let line = if remote { serde_json::json!({ "t": "in", "l": l }).to_string() } else { l };
                            if w.write_all(format!("{}\n", line).as_bytes()).await.is_err() || w.flush().await.is_err() {
                                return;
                            }
                        }
                        Some(Ctl::Close) => {
                            if remote {
                                // The node closes the agent's stdin; the SSH session stays.
                                let _ = w.write_all(b"{\"t\":\"close\"}\n").await;
                                let _ = w.flush().await;
                            } else {
                                return; // dropping stdin is the close
                            }
                        }
                        None => {
                            // Keep a remote job's stdin open until the job ends or is stopped.
                            if remote {
                                let _ = (&mut writer_stop).await;
                            }
                            return;
                        }
                    }
                }
            }
        });
        drop(ctl_tx);

        let temp = plan.temp.clone();
        let keep = plan.keep.clone();
        let p3 = proc.clone();
        tokio::spawn(async move {
            let _keep = keep;
            let mut stopped = false;
            let status = tokio::select! {
                s = child.wait() => s,
                _ = stop_rx => {
                    stopped = true;
                    p3.push("system", "stop requested".into(), None);
                    // Closing a remote job's stdin makes the node stop the agent's tree;
                    // then the local ssh (or the local agent) is terminated as well.
                    let _ = writer_stop_tx.send(());
                    if remote {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
                    }
                    stop_tree(&mut child, pid).await
                }
            };
            let _ = t_out.await;
            let _ = t_err.await;
            writer.abort();
            if let Some(t) = &temp {
                let _ = std::fs::remove_file(t);
            }
            *p3.ctl.lock().unwrap() = None;
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
                st.open = false;
                st.pending.clear();
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
            p3.save();
        });
        Ok(id)
    }
}

/// A short description of a tool request, for the feed.
fn tool_summary(tool: &str, input: &Value) -> String {
    let pick = [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "description",
        "prompt",
    ]
    .iter()
    .find_map(|k| input.get(*k).and_then(Value::as_str));
    match pick {
        Some(x) => format!(": {}", clip(x, 200)),
        None if tool.is_empty() => String::new(),
        None => String::new(),
    }
}

/// The MCP servers in Claude Code's init message, as (name, status).
pub(crate) fn mcp_servers(init: &Value) -> Vec<(String, String)> {
    init.get("mcp_servers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|m| {
                    (
                        m.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string(),
                        m.get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What to do about an MCP server that did not connect.
pub(crate) fn mcp_hint(status: &str) -> &'static str {
    match status {
        "needs-auth" => "it needs a login: run `claude` in a terminal on that machine, type /mcp, and log in to it; agents started here then use it",
        "failed" => "it did not start: run `claude mcp list` on that machine to see why (a missing command, token or network)",
        _ => "run `claude mcp list` on that machine to check it",
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
            model_args: vec![],
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
            model_args: vec![],
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

    /// A process for feeding stream lines to, in `format`.
    fn test_proc(format: &str, opts: AgentOpts) -> Proc {
        let (tx, _) = broadcast::channel(16);
        Proc {
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
                format: format.into(),
                host: None,
                session: None,
                interactive: false,
                open: false,
                turns: 0,
                mode: None,
                model: None,
                pending: Vec::new(),
                next_request: 0,
                reflect_state: 0,
                last_result: String::new(),
                last_failed: false,
                reflection: None,
                context_tokens: None,
                context_window: None,
                tokens: None,
                base_tokens: None,
                mcp: Vec::new(),
                files: Vec::new(),
                todos: Value::Null,
                links: Vec::new(),
                act: Activity::default(),
            }),
            tx,
            stop: Mutex::new(None),
            opts,
            profile: None,
            redact: Vec::new(),
            ctl: Mutex::new(None),
            memory: vec![],
            reflect: false,
            account: None,
            archived: false,
        }
    }

    #[test]
    fn parses_codex_and_gemini_and_counts_their_spend() {
        // Codex `exec --json`, as Codex 0.17x prints it.
        let p = test_proc(
            "codex-json",
            AgentOpts {
                model: Some("gpt-5".into()),
                ..Default::default()
            },
        );
        for l in [
            r#"{"type":"thread.started","thread_id":"01a21e49-9f7d-7c22-bf6c-69784030c885"}"#,
            r#"{"type":"item.completed","item":{"id":"item_0","type":"error","message":"`--dangerously-bypass-hook-trust` is enabled. Enabled hooks may run without review for this invocation."}}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'curl -s https://app.example.com'","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'curl -s https://app.example.com'","aggregated_output":"refused","exit_code":7,"status":"failed"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"The login form is not reachable."}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":1000000,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":100000,"reasoning_output_tokens":15}}"#,
        ] {
            p.agent_line(l);
        }
        let v = p.view();
        assert_eq!(
            v.session.as_deref(),
            Some("01a21e49-9f7d-7c22-bf6c-69784030c885")
        );
        assert!(
            (v.cost_usd.unwrap() - 2.25).abs() < 1e-9,
            "{:?}",
            v.cost_usd
        );
        assert!(v.cost_estimated);
        assert_eq!(v.tools.get("Bash"), Some(&1));
        let (ev, _) = p.subscribe(0);
        let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["system", "system", "tool", "error", "text", "result"],
            "{:?}",
            ev
        );
        assert!(ev[5].text.contains("about $2.25") && ev[5].text.contains("not reachable"));

        // Gemini CLI `--output-format stream-json`; a limit of $0.01 stops it.
        let p = test_proc(
            "gemini-stream-json",
            AgentOpts {
                max_budget_usd: Some(0.01),
                ..Default::default()
            },
        );
        for l in [
            r#"{"type":"init","timestamp":"2026-11-27T18:35:35.491Z","session_id":"a08abb62-93d9-4ce6-b1d1-03ad4fb421b5","model":"gemini-2.5-flash"}"#,
            r#"{"type":"message","timestamp":"t","role":"user","content":"test"}"#,
            r#"{"type":"message","timestamp":"t","role":"assistant","content":"Let me ","delta":true}"#,
            r#"{"type":"message","timestamp":"t","role":"assistant","content":"check.","delta":true}"#,
            r#"{"type":"tool_use","timestamp":"t","tool_name":"run_shell_command","tool_id":"r1","parameters":{"command":"curl -s https://evil.example.net/x"}}"#,
            r#"{"type":"tool_result","timestamp":"t","tool_id":"r1","status":"error","output":"Tool execution blocked: evil.example.net is outside the scope","error":{"type":"policy_violation","message":"Tool execution blocked: evil.example.net is outside the scope"}}"#,
            r#"{"type":"message","timestamp":"t","role":"assistant","content":"Done.","delta":true}"#,
            r#"{"type":"result","timestamp":"t","status":"success","stats":{"total_tokens":1100000,"input_tokens":1000000,"output_tokens":100000,"cached":0,"input":1000000,"duration_ms":130,"tool_calls":1,"models":{"gemini-2.5-flash":{"total_tokens":1100000,"input_tokens":1000000,"output_tokens":100000,"cached":0,"input":1000000}}}}"#,
        ] {
            p.agent_line(l);
        }
        let v = p.view();
        assert_eq!(v.model.as_deref(), Some("gemini-2.5-flash"));
        assert!(
            (v.cost_usd.unwrap() - 0.55).abs() < 1e-9,
            "{:?}",
            v.cost_usd
        );
        let (ev, _) = p.subscribe(0);
        let texts: Vec<String> = ev
            .iter()
            .map(|e| format!("{}: {}", e.kind, e.text))
            .collect();
        assert!(
            texts.contains(&"text: Let me check.".to_string()),
            "{:?}",
            texts
        );
        assert!(texts
            .iter()
            .any(|t| t.starts_with("tool: run_shell_command")));
        assert!(texts
            .iter()
            .any(|t| t.starts_with("error: Tool execution blocked")));
        assert!(texts
            .iter()
            .any(|t| t.starts_with("result: success") && t.ends_with("Done.")));
        assert!(
            texts
                .iter()
                .any(|t| t.contains("reached its spending limit of $0.01")),
            "{:?}",
            texts
        );

        // Claude Code under a subscription login (no cost reported): the
        // tokens of each model call, counted once.
        let p = test_proc("claude-stream-json", AgentOpts::default());
        for l in [
            r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","usage":{"input_tokens":1000000,"output_tokens":0},"content":[{"type":"text","text":"a"}]}}"#,
            r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","usage":{"input_tokens":1000000,"output_tokens":0},"content":[{"type":"text","text":"b"}]}}"#,
            r#"{"type":"result","subtype":"success","num_turns":1,"duration_ms":5,"result":"ok"}"#,
        ] {
            p.claude_line(l);
        }
        let v = p.view();
        assert!((v.cost_usd.unwrap() - 3.0).abs() < 1e-9, "{:?}", v.cost_usd);
        assert!(v.cost_estimated);
    }

    #[test]
    fn parses_claude_stream_json() {
        let p = test_proc("claude-stream-json", AgentOpts::default());
        for l in [
            r#"{"type":"system","subtype":"init","model":"m1","tools":[],"session_id":"4f9c2c1e-8b1d-4c55-9b3e-0d6c5c7e2a10"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Planning"},{"type":"tool_use","id":"t1","name":"Task","input":{"subagent_type":"sh-hunter","description":"hunt sqli"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t9","name":"Task","input":{"subagent_type":"sh-hunter","description":"hunt xss"}}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":"t1","message":{"content":[{"type":"tool_use","id":"t2","name":"Grep","input":{"pattern":"cursor.execute"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"done"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"zz","is_error":true,"content":[{"type":"text","text":"permission denied"}]}]}}"#,
            r#"{"type":"result","subtype":"success","num_turns":3,"duration_ms":4200,"total_cost_usd":0.0123,"result":"All good","usage":{"input_tokens":120,"output_tokens":800,"cache_read_input_tokens":9000,"cache_creation_input_tokens":300}}"#,
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
            v.tokens,
            Some(Tokens {
                input: 120,
                output: 800,
                cache_read: 9000,
                cache_write: 300
            })
        );
        assert_eq!(
            v.session.as_deref(),
            Some("4f9c2c1e-8b1d-4c55-9b3e-0d6c5c7e2a10")
        );
        assert!(
            p.outcome().contains("All good"),
            "the result is the outcome"
        );
        assert!(p.state.lock().unwrap().events[7].text.contains("$0.0123"));

        // The status pad's phase follows what it does, and what waits on you
        // comes first.
        let phase = |p: &Proc| {
            let v = p.view();
            (v.phase, v.activity)
        };
        assert_eq!(phase(&p), ("thinking", None), "a tool result is read");
        p.claude_line(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#);
        assert_eq!(phase(&p), ("tool", Some("Bash".into())));
        p.claude_line(r#"{"type":"system","subtype":"status","status":"compacting"}"#);
        assert_eq!(phase(&p).0, "compacting");
        p.claude_line(r#"{"type":"system","subtype":"compact_boundary"}"#);
        p.claude_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Here is the plan"}]}}"#);
        assert_eq!(phase(&p), ("writing", None));
        p.claude_line(r#"{"type":"control_request","request_id":"q1","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"question":"Which target?","header":"Target","multiSelect":false,"options":[{"label":"Kubernetes","description":""},{"label":"A VM","description":""}]}]}}}"#);
        assert_eq!(phase(&p), ("question", Some("AskUserQuestion".into())));
        {
            let mut st = p.state.lock().unwrap();
            st.pending.clear();
            st.status = "exited".into();
            st.exit_code = Some(0);
        }
        assert_eq!(phase(&p).0, "done");
        p.state.lock().unwrap().exit_code = Some(2);
        assert_eq!(phase(&p).0, "error");
    }

    /// A question is answered with what was picked, and only what it asked.
    #[test]
    fn questions_take_only_their_own_answers() {
        let q = Approval {
            request_id: "q1".into(),
            tool: "AskUserQuestion".into(),
            input: serde_json::json!({ "questions": [{ "question": "Which target?", "options": [] }] }),
            suggestions: Value::Null,
            ts: 0,
        };
        let ok = question_input(&q, &serde_json::json!({ "Which target?": "Kubernetes" })).unwrap();
        assert_eq!(ok["answers"]["Which target?"], "Kubernetes");
        assert_eq!(
            ok["questions"][0]["question"], "Which target?",
            "the questions stay"
        );
        for bad in [
            serde_json::json!({ "Something else?": "x" }),
            serde_json::json!({ "Which target?": "" }),
            serde_json::json!({ "Which target?": 3 }),
            serde_json::json!({}),
            serde_json::json!("Kubernetes"),
        ] {
            assert!(question_input(&q, &bad).is_err(), "{}", bad);
        }
        let bash = Approval {
            tool: "Bash".into(),
            ..q
        };
        assert!(
            question_input(&bash, &serde_json::json!({ "Which target?": "x" })).is_err(),
            "a tool request takes no answers"
        );
    }

    /// A finished session is kept on disk and comes back, read-only, with
    /// its conversation, tasks, files and links, after the console restarts.
    #[test]
    fn sessions_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("indra-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let logs = dir.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let procs = Procs::new(logs.clone());
        let (tx, _) = broadcast::channel(16);
        let p = Proc {
            id: 7,
            node: "local".into(),
            parent: None,
            resumed: None,
            can_resume: true,
            runner: "claude".into(),
            harness: "h".into(),
            prompt: "fix it".into(),
            cwd: "/t".into(),
            command: vec![],
            started: 1000,
            log: logs.join("1000-7-local-claude.log"),
            state: Mutex::new(State::new(
                "claude-stream-json",
                false,
                &AgentOpts::default(),
            )),
            tx,
            stop: Mutex::new(None),
            opts: AgentOpts::default(),
            profile: None,
            redact: vec![],
            ctl: Mutex::new(None),
            memory: vec![],
            reflect: false,
            account: None,
            archived: false,
        };
        for l in [
            r#"{"type":"system","subtype":"init","model":"m1","session_id":"s-1"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"TodoWrite","input":{"todos":[{"content":"one","status":"completed"}]}},{"type":"tool_use","id":"b","name":"Edit","input":{"file_path":"src/x.rs"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"see https://github.com/o/r/pull/3 and https://github.com/o/r/pull/3"}]}}"#,
        ] {
            p.claude_line(l);
        }
        p.state.lock().unwrap().status = "exited".into();
        p.save();
        drop(procs);
        let again = Procs::new(logs);
        let v = again.get(7).expect("restored").view();
        assert!(v.archived && !v.open);
        assert_eq!(v.status, "exited");
        assert_eq!(v.session.as_deref(), Some("s-1"));
        assert_eq!(v.files, vec!["src/x.rs"]);
        assert_eq!(v.links, vec!["https://github.com/o/r/pull/3"], "once each");
        assert_eq!(v.todos[0]["content"], "one");
        assert!(v.events > 0);
        // New processes do not reuse its id.
        assert_eq!(again.next.load(Ordering::SeqCst), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
