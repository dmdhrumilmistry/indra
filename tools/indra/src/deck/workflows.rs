//! Orchestration: run a graph of agent tasks (and shell commands) across
//! nodes, with conditions, loops and failure handling (see `deck::flow`).
//!
//! A workflow is a set of tasks; each runs one agent (a runner and a prompt)
//! in a target directory on a node. `needs` orders them. The engine ticks
//! every half second:
//!   - a waiting task becomes queued by its `on`: "success" (the default)
//!     when every need succeeded (or failed with `allow_failure`), skipped
//!     when one did not; "failure" when one failed; "always" once all are
//!     done. Its `when` condition must then hold, or it is skipped;
//!   - a `foreach` step is copied per item when the run starts, and an
//!     `until` step runs again until its condition holds;
//!   - queued tasks start while their node has capacity (`max_parallel`,
//!     counting every agent on that node, not only this workflow's);
//!   - a failed task is retried up to `retries` more times;
//!   - cancel stops running agents and cancels everything not yet started.

use crate::deck::config::{self, TaskCfg, WorkflowCfg};
use crate::deck::nodes::LOCAL;
use crate::deck::procs::{Launch, Procs};
use crate::deck::runs::now;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const TICK_MS: u64 = 500;

#[derive(Debug, Clone, Serialize)]
pub struct TaskRun {
    pub id: String,
    /// A shell command instead of an agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// success | failure | always
    pub on: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    pub max_runs: u32,
    pub allow_failure: bool,
    /// The loop item this copy runs for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// What it printed last (an agent's result, a command's output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Completed runs (an `until` loop makes several).
    pub runs: u32,
    /// A stop step: "success" or "failure".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
    /// An If step's condition (it answers "yes" or "no" in its output).
    #[serde(rename = "if", skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// The answer of the If step before it that this step follows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Skipped because an If went the other way: a step that both sides
    /// lead to still runs after the side that ran.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub off_branch: bool,
    pub node: String,
    pub target: String,
    pub runner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub prompt: String,
    pub needs: Vec<String>,
    pub retries: u32,
    pub attempts: u32,
    /// waiting | queued | running | succeeded | failed | skipped | cancelled
    pub state: &'static str,
    /// The current (or last) agent process for this task.
    pub proc_id: Option<u64>,
    /// Every process this task started, one per attempt.
    pub procs: Vec<u64>,
    pub error: Option<String>,
    pub started: Option<u64>,
    pub ended: Option<u64>,
}

impl TaskRun {
    fn terminal(&self) -> bool {
        matches!(self.state, "succeeded" | "failed" | "skipped" | "cancelled")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WfRun {
    pub id: u64,
    pub name: String,
    pub harness: String,
    pub started: u64,
    pub ended: Option<u64>,
    /// running | succeeded | failed | cancelled
    pub status: &'static str,
    pub tasks: Vec<TaskRun>,
    /// The inputs it was started with.
    pub inputs: BTreeMap<String, String>,
    /// The stop step that ended it early, if one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<String>,
}

/// A step's result, as conditions and later steps see it.
#[derive(Clone)]
struct Res {
    state: &'static str,
    output: String,
    exit_code: Option<i32>,
    allow_failure: bool,
    is_check: bool,
    off_branch: bool,
}

fn snapshot(tasks: &[TaskRun]) -> HashMap<String, Res> {
    tasks
        .iter()
        .map(|t| {
            (
                t.id.clone(),
                Res {
                    state: t.state,
                    output: t.output.clone().unwrap_or_default(),
                    exit_code: t.exit_code,
                    allow_failure: t.allow_failure,
                    is_check: t.check.is_some(),
                    off_branch: t.off_branch,
                },
            )
        })
        .collect()
}

/// Evaluate a condition against the steps' results, the inputs, and (in
/// `until`) this step's own last run.
fn holds(
    cond: &str,
    snap: &HashMap<String, Res>,
    inputs: &BTreeMap<String, String>,
    me: Option<&Res>,
) -> bool {
    use crate::deck::flow::{eval, parse, Val};
    let Ok(e) = parse(cond) else { return false };
    let lookup = |r: &str| -> Option<Val> {
        let (head, field) = r.split_once('.')?;
        if head == "input" {
            return Some(Val::Str(inputs.get(field).cloned().unwrap_or_default()));
        }
        let res = if head == "self" { me? } else { snap.get(head)? };
        Some(match field {
            "succeeded" => Val::Bool(res.state == "succeeded"),
            "failed" => Val::Bool(res.state == "failed"),
            "skipped" => Val::Bool(res.state == "skipped"),
            "status" => Val::Str(res.state.into()),
            "output" => Val::Str(res.output.clone()),
            "exit_code" => match res.exit_code {
                Some(c) => Val::Num(c as f64),
                None => Val::Str(String::new()),
            },
            _ => return None,
        })
    };
    eval(&e, &lookup)
}

/// `NAME` as an environment variable name part: upper case, `_` for the rest.
fn env_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// A value for the environment: its last 8000 characters, never a NUL, and
/// never read as a `secret:` reference.
fn env_value(v: &str) -> String {
    let v = v.replace('\0', "");
    let n = v.chars().count();
    let v: String = if n > 8000 {
        v.chars().skip(n - 8000).collect()
    } else {
        v
    };
    if crate::deck::secrets::reference(&v).is_some() {
        format!(" {}", v)
    } else {
        v
    }
}

/// One copy of each `foreach` step per item (`<id>-1`, `<id>-2`, ...);
/// whatever needed the step waits for all its copies. No items: nothing to
/// do, so the step drops out.
pub fn expand_loops(mut wf: WorkflowCfg) -> Result<WorkflowCfg, String> {
    use crate::deck::flow;
    let mut out = Vec::new();
    let mut copies: HashMap<String, Vec<String>> = HashMap::new();
    for mut t in std::mem::take(&mut wf.tasks) {
        let Some(f) = t.foreach.take() else {
            out.push(t);
            continue;
        };
        let items = flow::items(&f.list());
        if items.len() > flow::MAX_ITEMS {
            return Err(format!(
                "task '{}': {} items, more than the {} a loop may have",
                t.id,
                items.len(),
                flow::MAX_ITEMS
            ));
        }
        let mut ids = Vec::new();
        for (k, item) in items.iter().enumerate() {
            let mut c = t.clone();
            c.id = format!("{}-{}", t.id, k + 1);
            c.prompt = c.prompt.replace("{item}", item);
            c.target = c.target.replace("{item}", item);
            c.item = Some(item.clone());
            ids.push(c.id.clone());
            out.push(c);
        }
        copies.insert(t.id.clone(), ids);
    }
    for t in &mut out {
        t.needs = t
            .needs
            .iter()
            .flat_map(|n| copies.get(n).cloned().unwrap_or_else(|| vec![n.clone()]))
            .collect();
    }
    wf.tasks = out;
    Ok(wf)
}

pub struct Engine {
    runs: Mutex<BTreeMap<u64, Arc<Mutex<WfRun>>>>,
    cancelled: Mutex<HashSet<u64>>,
    next: AtomicU64,
    procs: Arc<Procs>,
    settings: config::Shared,
}

impl Engine {
    pub fn new(procs: Arc<Procs>, settings: config::Shared) -> Arc<Engine> {
        Arc::new(Engine {
            runs: Mutex::new(BTreeMap::new()),
            cancelled: Mutex::new(HashSet::new()),
            next: AtomicU64::new(1),
            procs,
            settings,
        })
    }

    pub fn list(&self) -> Vec<WfRun> {
        let mut v: Vec<WfRun> = self
            .runs
            .lock()
            .unwrap()
            .values()
            .map(|r| r.lock().unwrap().clone())
            .collect();
        v.sort_by_key(|r| std::cmp::Reverse(r.id));
        v
    }

    pub fn get(&self, id: u64) -> Option<WfRun> {
        self.runs
            .lock()
            .unwrap()
            .get(&id)
            .map(|r| r.lock().unwrap().clone())
    }

    pub fn cancel(&self, id: u64) -> bool {
        let known = self.runs.lock().unwrap().contains_key(&id);
        if known {
            self.cancelled.lock().unwrap().insert(id);
        }
        known
    }

    /// Start a workflow defined in the config, by name.
    /// Start a configured (or built-in) workflow with its inputs filled in.
    pub fn start_with_inputs(
        self: &Arc<Self>,
        name: &str,
        given: &std::collections::BTreeMap<String, String>,
    ) -> Result<u64, String> {
        let settings = self.settings.get();
        let wf = settings
            .workflows
            .iter()
            .find(|w| w.name == name)
            .cloned()
            .ok_or_else(|| format!("no workflow named '{}'", name))?;
        if wf.inputs.is_empty() {
            if !given.is_empty() {
                return Err(format!("workflow '{}' takes no inputs", name));
            }
            return self.start(wf);
        }
        let harness = wf
            .harness
            .as_deref()
            .and_then(|n| settings.harnesses.iter().find(|h| h.name == n))
            .or_else(|| settings.harnesses.first());
        let targets = harness.map(|h| h.targets.clone()).unwrap_or_default();
        let profiles: Vec<String> = settings.profiles.iter().map(|p| p.name.clone()).collect();
        let mut wf = config::apply_inputs(&wf, given, &targets, &profiles)?;
        // A local target from a text input is relative to the config file.
        let base = settings
            .config_path
            .as_deref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        for t in &mut wf.tasks {
            if t.node == LOCAL && !std::path::Path::new(&t.target).is_absolute() {
                t.target = base.join(&t.target).display().to_string();
            }
        }
        self.start(wf)
    }

    /// Start any validated workflow (configured, or an ad-hoc fan-out).
    pub fn start(self: &Arc<Self>, wf: WorkflowCfg) -> Result<u64, String> {
        let settings = self.settings.get();
        let names: Vec<String> = settings.nodes.iter().map(|n| n.name.clone()).collect();
        config::validate_workflow(&wf, &names)?;
        let wf = expand_loops(wf)?;
        if wf.tasks.is_empty() {
            return Err("nothing to run: every step loops over no items".into());
        }
        config::validate_workflow(&wf, &names)?;
        let harness = wf
            .harness
            .clone()
            .or_else(|| settings.harnesses.first().map(|h| h.name.clone()))
            .unwrap_or_default();
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let run = WfRun {
            id,
            name: wf.name.clone(),
            harness,
            started: now(),
            ended: None,
            status: "running",
            tasks: wf.tasks.iter().map(task_run).collect(),
            inputs: wf.values.clone(),
            stopped_by: None,
        };
        let run = Arc::new(Mutex::new(run));
        self.runs.lock().unwrap().insert(id, run.clone());
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                if me.tick(id, &run) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
            }
        });
        Ok(id)
    }

    /// One scheduling pass. Returns true when the run has finished.
    fn tick(&self, id: u64, run: &Arc<Mutex<WfRun>>) -> bool {
        let mut r = run.lock().unwrap();
        // A stop step ended it: like a cancel, but it finishes as the step says.
        let cancel = self.cancelled.lock().unwrap().contains(&id) || r.stopped_by.is_some();
        let inputs = r.inputs.clone();
        let snap = snapshot(&r.tasks);

        // 1. Collect finished agents.
        for t in r.tasks.iter_mut().filter(|t| t.state == "running") {
            let Some(p) = t.proc_id.and_then(|pid| self.procs.get(pid)) else {
                t.state = "failed";
                t.error = Some("its process disappeared".into());
                t.ended = Some(now());
                continue;
            };
            if p.is_running() {
                if cancel {
                    p.request_stop();
                }
                continue;
            }
            let v = p.view();
            t.ended = Some(now());
            t.exit_code = v.exit_code;
            t.output = Some(if t.run.is_some() {
                p.text_output(20_000)
            } else {
                p.outcome()
            });
            match v.status.as_str() {
                "exited" => {
                    t.runs += 1;
                    let again = t.until.as_deref().map(|u| {
                        let me = Res {
                            state: "succeeded",
                            output: t.output.clone().unwrap_or_default(),
                            exit_code: t.exit_code,
                            allow_failure: t.allow_failure,
                            is_check: false,
                            off_branch: false,
                        };
                        !holds(u, &snap, &inputs, Some(&me))
                    });
                    match again {
                        Some(true) if t.runs < t.max_runs => {
                            t.state = "queued";
                            t.attempts = 0;
                            t.error = Some(format!(
                                "run {} of at most {}: until not met yet, running again",
                                t.runs, t.max_runs
                            ));
                        }
                        Some(true) => {
                            t.state = "failed";
                            t.error = Some(format!(
                                "until {:?} still did not hold after {} runs",
                                t.until.clone().unwrap_or_default(),
                                t.runs
                            ));
                        }
                        _ => {
                            t.state = "succeeded";
                            t.error = None;
                        }
                    }
                }
                _ if cancel => t.state = "cancelled",
                _ if t.attempts <= t.retries => {
                    t.state = "queued";
                    t.error = Some(format!("attempt {} {}; retrying", t.attempts, v.status));
                }
                _ => {
                    t.state = "failed";
                    t.error = Some(format!(
                        "agent {} (exit code {})",
                        v.status,
                        v.exit_code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "none".into())
                    ));
                }
            }
        }

        // 2. Cancel what has not started; resolve dependencies.
        if cancel {
            for t in r
                .tasks
                .iter_mut()
                .filter(|t| matches!(t.state, "waiting" | "queued"))
            {
                t.state = "cancelled";
                t.ended = Some(now());
            }
        }
        let snap = snapshot(&r.tasks);
        for t in r.tasks.iter_mut().filter(|t| t.state == "waiting") {
            let state = |n: &String| snap.get(n).map(|x| x.state).unwrap_or("failed");
            let done =
                |n: &String| matches!(state(n), "succeeded" | "failed" | "skipped" | "cancelled");
            // Failed, but allowed to: what follows on success still runs.
            // A side an If did not take counts as done: a step both sides
            // lead to runs after the side that ran.
            let off = |n: &String| {
                snap.get(n)
                    .is_some_and(|x| x.state == "skipped" && x.off_branch)
            };
            let ok = |n: &String| {
                state(n) == "succeeded"
                    || (state(n) == "failed" && snap.get(n).is_some_and(|x| x.allow_failure))
                    || off(n)
            };
            let all_done = t.needs.iter().all(done);
            // Which side of an If this step is on, once its If answered.
            if let Some(b) = t.branch.as_deref() {
                let other_way = t.needs.iter().any(|n| {
                    snap.get(n)
                        .is_some_and(|x| x.is_check && x.state == "succeeded" && x.output != b)
                });
                if other_way {
                    t.state = "skipped";
                    t.off_branch = true;
                    t.error = Some(format!(
                        "the If before it answered the other way (it runs on \"{}\")",
                        b
                    ));
                    t.ended = Some(now());
                    continue;
                }
            }
            // Everything it follows was on a side not taken: so is it.
            if !t.needs.is_empty() && t.needs.iter().all(off) {
                t.state = "skipped";
                t.off_branch = true;
                t.error = Some("on a side of an If that was not taken".into());
                t.ended = Some(now());
                continue;
            }
            let go: Result<bool, &str> = match t.on.as_str() {
                "failure" if all_done => {
                    if t.needs.iter().any(|n| state(n) == "failed") {
                        Ok(true)
                    } else {
                        Err("nothing it follows failed (it runs on failure)")
                    }
                }
                "always" if all_done => Ok(true),
                "failure" | "always" => Ok(false),
                _ => {
                    if t.needs.iter().any(|n| done(n) && !ok(n)) {
                        Err("a step it needs did not succeed")
                    } else {
                        Ok(t.needs.iter().all(ok))
                    }
                }
            };
            match go {
                Ok(false) => {}
                Ok(true) => match t.when.as_deref() {
                    Some(c) if !holds(c, &snap, &inputs, None) => {
                        t.state = "skipped";
                        t.error = Some(format!("its condition was not met: when {:?}", c));
                        t.ended = Some(now());
                    }
                    _ => t.state = "queued",
                },
                Err(why) => {
                    t.state = "skipped";
                    t.error = Some(why.into());
                    t.ended = Some(now());
                }
            }
        }

        // An If step runs nothing: it checks its condition and answers.
        let snap2 = snapshot(&r.tasks);
        for t in r
            .tasks
            .iter_mut()
            .filter(|t| t.state == "queued" && t.check.is_some())
        {
            let c = t.check.clone().unwrap_or_default();
            let yes = holds(&c, &snap2, &inputs, None);
            t.state = "succeeded";
            t.started = Some(now());
            t.ended = Some(now());
            t.output = Some(if yes { "yes" } else { "no" }.into());
            t.exit_code = Some(0);
            t.runs = 1;
        }

        // A stop step that is reached ends the workflow: nothing else starts,
        // and what runs is stopped.
        if r.stopped_by.is_none() {
            if let Some(i) = r
                .tasks
                .iter()
                .position(|t| t.state == "queued" && t.stop.is_some())
            {
                let sid = r.tasks[i].id.clone();
                {
                    let t = &mut r.tasks[i];
                    t.state = "succeeded";
                    t.started = Some(now());
                    t.ended = Some(now());
                    t.output = Some(if t.prompt.trim().is_empty() {
                        format!(
                            "stopped the workflow ({})",
                            t.stop.clone().unwrap_or_default()
                        )
                    } else {
                        t.prompt.clone()
                    });
                }
                for t in r
                    .tasks
                    .iter_mut()
                    .filter(|t| matches!(t.state, "waiting" | "queued"))
                {
                    t.state = "skipped";
                    t.error = Some(format!("the workflow stopped at '{}'", sid));
                    t.ended = Some(now());
                }
                for t in r.tasks.iter().filter(|t| t.state == "running") {
                    if let Some(p) = t.proc_id.and_then(|pid| self.procs.get(pid)) {
                        p.request_stop();
                    }
                }
                r.stopped_by = Some(sid);
            }
        }

        // 3. Start queued tasks while their node has room. A started agent
        // counts as running at once, so the next check already sees it.
        let harness = r.harness.clone();
        let name = r.name.clone();
        let settings = self.settings.get();
        for t in r.tasks.iter_mut().filter(|t| t.state == "queued") {
            if self.procs.running_on(&t.node) >= settings.capacity(&t.node) {
                continue;
            }
            t.attempts += 1;
            match self.launch(t, &harness, &snap, &inputs, &name) {
                Ok(pid) => {
                    t.state = "running";
                    t.proc_id = Some(pid);
                    t.procs.push(pid);
                    t.started.get_or_insert(now());
                }
                Err(e) if t.attempts <= t.retries => {
                    t.error = Some(format!("attempt {}: {}", t.attempts, e))
                }
                Err(e) => {
                    t.state = "failed";
                    t.error = Some(e);
                    t.ended = Some(now());
                }
            }
        }

        // 4. Done?
        if r.tasks.iter().all(TaskRun::terminal) {
            // Skipped steps are part of the plan (a condition, a branch not
            // taken); a failure fails the run unless it was allowed.
            let stop = r
                .stopped_by
                .as_ref()
                .and_then(|s| r.tasks.iter().find(|t| &t.id == s))
                .and_then(|t| t.stop.clone());
            r.status = if let Some(st) = stop {
                if st == "failure" {
                    "failed"
                } else {
                    "succeeded"
                }
            } else if cancel {
                "cancelled"
            } else if r
                .tasks
                .iter()
                .any(|t| t.state == "cancelled" || (t.state == "failed" && !t.allow_failure))
            {
                "failed"
            } else {
                "succeeded"
            };
            r.ended = Some(now());
            return true;
        }
        false
    }

    fn launch(
        &self,
        t: &TaskRun,
        harness: &str,
        snap: &HashMap<String, Res>,
        inputs: &BTreeMap<String, String>,
        workflow: &str,
    ) -> Result<u64, String> {
        // The settings now, so a node or runner edited in the console applies
        // to tasks that have not started yet.
        let settings = self.settings.get();
        // What the step knows, as environment variables: the inputs, its
        // loop item, and every finished step's result.
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        env.insert("INDRA_WORKFLOW".into(), workflow.into());
        env.insert("INDRA_STEP".into(), t.id.clone());
        env.insert("INDRA_NODE".into(), t.node.clone());
        env.insert("INDRA_TARGET".into(), t.target.clone());
        if let Some(i) = &t.item {
            env.insert("INDRA_ITEM".into(), env_value(i));
        }
        for (k, v) in inputs {
            env.insert(format!("INDRA_INPUT_{}", env_name(k)), env_value(v));
        }
        for (sid, res) in snap
            .iter()
            .filter(|(_, r)| matches!(r.state, "succeeded" | "failed" | "skipped" | "cancelled"))
        {
            let n = env_name(sid);
            env.insert(format!("INDRA_STATUS_{}", n), res.state.into());
            env.insert(format!("INDRA_OUTPUT_{}", n), env_value(&res.output));
            if let Some(c) = res.exit_code {
                env.insert(format!("INDRA_EXIT_{}", n), c.to_string());
            }
        }
        // A shell command: the reserved shell runner, here or on the node.
        if let Some(cmd) = &t.run {
            let start = crate::deck::procs::StartOpts {
                agent: crate::deck::nodes::AgentOpts {
                    env,
                    ..Default::default()
                },
                ..Default::default()
            };
            if t.node == LOCAL {
                if !settings.shell_steps {
                    return Err(
                        "shell steps are turned off on this machine ([local] shell_steps = false)"
                            .into(),
                    );
                }
                let runner = config::shell_runner();
                let target = PathBuf::from(&t.target);
                return self.procs.start_with(
                    Launch::Local {
                        runner: &runner,
                        harness,
                        prompt: cmd,
                        target: &target,
                    },
                    start,
                );
            }
            let node = settings
                .node(&t.node)
                .ok_or_else(|| format!("unknown node '{}'", t.node))?;
            return self.procs.start_with(
                Launch::Remote {
                    node,
                    runner: config::SHELL_RUNNER,
                    harness,
                    prompt: cmd,
                    target: &t.target,
                },
                start,
            );
        }
        // An agent: earlier results can be written into its prompt.
        let mut prompt = t.prompt.replace("{node}", &t.node);
        for (sid, res) in snap {
            prompt = prompt
                .replace(&format!("{{steps.{}.output}}", sid), &res.output)
                .replace(&format!("{{steps.{}.status}}", sid), res.state)
                .replace(
                    &format!("{{steps.{}.exit_code}}", sid),
                    &res.exit_code.map(|c| c.to_string()).unwrap_or_default(),
                );
        }
        // A profile brings its runner (if it names one) and the agent options.
        let (runner_name, agent) = match t.profile.as_deref() {
            Some(p) => {
                let prof = settings
                    .profile(p)
                    .ok_or_else(|| format!("profile '{}' is not configured", p))?;
                (
                    prof.runner.clone().unwrap_or_else(|| t.runner.clone()),
                    prof.opts(),
                )
            }
            None => (t.runner.clone(), crate::deck::nodes::AgentOpts::default()),
        };
        // The task's own model and provider win over the profile's.
        let mut agent = agent.with(&crate::deck::nodes::AgentOpts {
            model: t.model.clone().filter(|m| !m.trim().is_empty()),
            provider: t.provider.clone().filter(|p| !p.trim().is_empty()),
            ..Default::default()
        });
        // The profile's own environment wins.
        for (k, v) in env {
            agent.env.entry(k).or_insert(v);
        }
        agent.validate()?;
        let (prompt, memory, reflect) = crate::deck::knowledge::prepare(
            &settings,
            harness,
            &prompt,
            t.profile.as_deref(),
            None,
            None,
        );
        let start = crate::deck::procs::StartOpts {
            agent,
            profile: t.profile.clone(),
            memory,
            reflect,
            ..Default::default()
        };
        if t.node == LOCAL {
            let runner = settings
                .runners
                .iter()
                .find(|r| r.name == runner_name)
                .ok_or_else(|| {
                    format!("runner '{}' is not configured on this machine", runner_name)
                })?;
            let target = PathBuf::from(&t.target);
            self.procs.start_with(
                Launch::Local {
                    runner,
                    harness,
                    prompt: &prompt,
                    target: &target,
                },
                start,
            )
        } else {
            let node = settings
                .node(&t.node)
                .ok_or_else(|| format!("unknown node '{}'", t.node))?;
            self.procs.start_with(
                Launch::Remote {
                    node,
                    runner: &runner_name,
                    harness,
                    prompt: &prompt,
                    target: &t.target,
                },
                start,
            )
        }
    }
}

fn task_run(t: &TaskCfg) -> TaskRun {
    TaskRun {
        id: t.id.clone(),
        run: t.run.clone(),
        when: t.when.clone(),
        on: t.on.clone().unwrap_or_else(|| "success".into()),
        until: t.until.clone(),
        max_runs: t.max_runs.unwrap_or(3),
        allow_failure: t.allow_failure,
        item: t.item.clone(),
        output: None,
        exit_code: None,
        runs: 0,
        stop: t.stop.clone(),
        check: t.check.clone(),
        branch: t.branch.clone(),
        off_branch: false,
        node: t.node.clone(),
        target: t.target.clone(),
        runner: t.runner.clone(),
        profile: t.profile.clone(),
        model: t.model.clone(),
        provider: t.provider.clone(),
        prompt: t.prompt.clone(),
        needs: t.needs.clone(),
        retries: t.retries,
        attempts: 0,
        state: "waiting",
        proc_id: None,
        procs: Vec::new(),
        error: None,
        started: None,
        ended: None,
    }
}

/// One prompt, many (node, target) pairs, all in parallel: an ad-hoc workflow.
pub fn fan_out(runner: &str, prompt: &str, targets: &[(String, String)]) -> WorkflowCfg {
    let mut used: HashMap<String, usize> = HashMap::new();
    let tasks = targets
        .iter()
        .map(|(node, target)| {
            let base: String = std::path::Path::new(target)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "task".into())
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            let base = format!(
                "{}-{}",
                node,
                if base.is_empty() { "task".into() } else { base }
            );
            let n = used.entry(base.clone()).or_insert(0);
            *n += 1;
            let id = if *n == 1 {
                base
            } else {
                format!("{}-{}", base, n)
            };
            TaskCfg {
                id,
                node: node.clone(),
                target: target.clone(),
                runner: runner.into(),
                prompt: prompt.into(),
                needs: Vec::new(),
                retries: 0,
                profile: None,
                model: None,
                provider: None,
                component: None,
                workflow: None,
                skill: None,
                pos: None,
                ..Default::default()
            }
        })
        .collect();
    WorkflowCfg {
        inputs: vec![],
        schedule: None,
        builtin: false,
        source: None,
        values: Default::default(),
        name: "fan-out".into(),
        description: "Ad-hoc fan-out".into(),
        harness: None,
        tasks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::config::validate_workflow;

    fn task(id: &str, needs: &[&str]) -> TaskCfg {
        TaskCfg {
            id: id.into(),
            node: LOCAL.into(),
            target: ".".into(),
            runner: "claude".into(),
            prompt: "p".into(),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            retries: 0,
            profile: None,
            model: None,
            provider: None,
            component: None,
            workflow: None,
            skill: None,
            pos: None,
            ..Default::default()
        }
    }

    fn wf(tasks: Vec<TaskCfg>) -> WorkflowCfg {
        WorkflowCfg {
            schedule: None,
            inputs: vec![],
            builtin: false,
            source: None,
            values: Default::default(),
            name: "w".into(),
            description: String::new(),
            harness: None,
            tasks,
        }
    }

    #[test]
    fn validation() {
        assert!(validate_workflow(&wf(vec![task("a", &[]), task("b", &["a"])]), &[]).is_ok());
        assert!(validate_workflow(&wf(vec![]), &[])
            .unwrap_err()
            .contains("no [[workflow.task]]"));
        assert!(
            validate_workflow(&wf(vec![task("a", &[]), task("a", &[])]), &[])
                .unwrap_err()
                .contains("twice")
        );
        assert!(validate_workflow(&wf(vec![task("a", &["zz"])]), &[])
            .unwrap_err()
            .contains("unknown task"));
        let cyc = validate_workflow(
            &wf(vec![
                task("a", &["c"]),
                task("b", &["a"]),
                task("c", &["b"]),
                task("d", &[]),
            ]),
            &[],
        )
        .unwrap_err();
        assert!(
            cyc.ends_with("dependency cycle among: a, b, c"),
            "only the tasks on the cycle are named: {}",
            cyc
        );
        let mut t = task("a", &[]);
        t.node = "nowhere".into();
        assert!(validate_workflow(&wf(vec![t.clone()]), &[])
            .unwrap_err()
            .contains("unknown node"));
        assert!(validate_workflow(&wf(vec![t]), &["nowhere".into()]).is_ok());
        assert!(validate_workflow(&wf(vec![task("bad id", &[])]), &[]).is_err());
    }

    #[test]
    fn fan_out_ids_are_unique_and_valid() {
        let w = fan_out(
            "claude",
            "review {target}",
            &[
                ("local".into(), "/a/api".into()),
                ("box".into(), "/srv/api".into()),
                ("local".into(), "/b/api".into()),
                ("local".into(), "/x/my app!".into()),
            ],
        );
        let ids: Vec<&str> = w.tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["local-api", "box-api", "local-api-2", "local-my-app-"]
        );
        assert!(validate_workflow(&w, &["box".into()]).is_ok());
    }
}
