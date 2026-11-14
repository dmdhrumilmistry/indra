//! Orchestration: run a graph of agent tasks across nodes.
//!
//! A workflow is a set of tasks; each runs one agent (a runner and a prompt)
//! in a target directory on a node. `needs` orders them. The engine ticks
//! every half second:
//!   - a waiting task whose needs all succeeded becomes queued; if any need
//!     failed, was skipped or cancelled, it is skipped;
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
    pub fn start_named(self: &Arc<Self>, name: &str) -> Result<u64, String> {
        let wf = self
            .settings
            .get()
            .workflows
            .iter()
            .find(|w| w.name == name)
            .cloned()
            .ok_or_else(|| format!("no workflow named '{}'", name))?;
        self.start(wf)
    }

    /// Start any validated workflow (configured, or an ad-hoc fan-out).
    pub fn start(self: &Arc<Self>, wf: WorkflowCfg) -> Result<u64, String> {
        let settings = self.settings.get();
        let names: Vec<String> = settings.nodes.iter().map(|n| n.name.clone()).collect();
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
        let cancel = self.cancelled.lock().unwrap().contains(&id);
        let mut r = run.lock().unwrap();

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
            match v.status.as_str() {
                "exited" => t.state = "succeeded",
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
        let states: HashMap<String, &'static str> =
            r.tasks.iter().map(|t| (t.id.clone(), t.state)).collect();
        for t in r.tasks.iter_mut().filter(|t| t.state == "waiting") {
            let need = |n: &String| states.get(n).copied().unwrap_or("failed");
            if t.needs
                .iter()
                .any(|n| matches!(need(n), "failed" | "skipped" | "cancelled"))
            {
                t.state = "skipped";
                t.error = Some("a task it needs did not succeed".into());
                t.ended = Some(now());
            } else if t.needs.iter().all(|n| need(n) == "succeeded") {
                t.state = "queued";
            }
        }

        // 3. Start queued tasks while their node has room. A started agent
        // counts as running at once, so the next check already sees it.
        let harness = r.harness.clone();
        let settings = self.settings.get();
        for t in r.tasks.iter_mut().filter(|t| t.state == "queued") {
            if self.procs.running_on(&t.node) >= settings.capacity(&t.node) {
                continue;
            }
            t.attempts += 1;
            match self.launch(t, &harness) {
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
            r.status = if cancel {
                "cancelled"
            } else if r.tasks.iter().all(|t| t.state == "succeeded") {
                "succeeded"
            } else {
                "failed"
            };
            r.ended = Some(now());
            return true;
        }
        false
    }

    fn launch(&self, t: &TaskRun, harness: &str) -> Result<u64, String> {
        let prompt = t.prompt.replace("{node}", &t.node);
        // The settings now, so a node or runner edited in the console applies
        // to tasks that have not started yet.
        let settings = self.settings.get();
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
        let agent = agent.with(&crate::deck::nodes::AgentOpts {
            model: t.model.clone().filter(|m| !m.trim().is_empty()),
            provider: t.provider.clone().filter(|p| !p.trim().is_empty()),
            ..Default::default()
        });
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
                pos: None,
            }
        })
        .collect();
    WorkflowCfg {
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
            pos: None,
        }
    }

    fn wf(tasks: Vec<TaskCfg>) -> WorkflowCfg {
        WorkflowCfg {
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
