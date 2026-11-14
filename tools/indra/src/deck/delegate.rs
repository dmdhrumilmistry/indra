//! Plan and delegate: a strong model plans, cheaper models do the work.
//!
//! 1. A **planner** agent (a profile: Opus or Fable through Claude Code, a
//!    strong Codex model, anything) reads the code and answers with a plan:
//!    small, self-contained steps, each marked `standard` or `light`.
//! 2. The plan waits for review (unless the team runs it straight away); a
//!    person can edit, drop or reorder steps.
//! 3. Each step runs as its own agent with the **worker** profile (Haiku, a
//!    self-hosted model through Ollama, a small Codex model), or the cheaper
//!    **light worker** for mechanical steps, on the workflow engine: in
//!    order (or in parallel where the plan allows), within node capacity,
//!    each with its own process page and controls.
//! 4. An optional **reviewer** checks the result against the plan.
//!
//! indra drives it from the outside, so any runner can play any role, and
//! roles can mix providers (Claude plans, a local model builds).

use crate::deck::config::{Settings, TaskCfg, TeamCfg, WorkflowCfg};
use crate::deck::nodes::LOCAL;
use crate::deck::procs::{Launch, StartOpts};
use crate::deck::web::App;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Where the planner's instructions start, after the task.
pub const PLAN_MARK: &str = "\n\n---\nYou are the planner.";

/// What the planner is asked to do, after the task.
fn plan_prompt(max_steps: usize) -> String {
    format!(
        "You are the planner. Do not change any files: read the code, then plan the work for other, less capable agents \
that will each carry out one step in this same directory, one after another, without seeing this conversation.\n\n\
Reply with only a JSON object, no other text:\n\
{{\"summary\": \"the approach in two or three sentences\",\n \"steps\": [{{\"id\": \"short-kebab-id\", \"title\": \"what this step achieves\", \
\"prompt\": \"complete, self-contained instructions: which files to change and how, what to leave alone, and how to check the step worked\", \
\"needs\": [\"ids of steps that must be done first\"], \"size\": \"standard or light\"}}]}}\n\n\
Rules: at most {} steps; each small and checkable on its own; mark a step light only when it is mechanical (renames, docs, boilerplate, \
simple config); name real files and functions you have read; prefer fewer, clearer steps.",
        max_steps
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub needs: Vec<String>,
    /// standard | light
    #[serde(default = "standard")]
    pub size: String,
}

fn standard() -> String {
    "standard".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub summary: String,
    pub steps: Vec<Step>,
}

/// The roles of one delegation, resolved to profiles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Roles {
    pub planner: String,
    pub worker: String,
    #[serde(default)]
    pub light_worker: Option<String>,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub auto_run: bool,
    #[serde(default)]
    pub parallel: bool,
    #[serde(default = "default_max")]
    pub max_steps: usize,
}

fn default_max() -> usize {
    8
}

impl From<&TeamCfg> for Roles {
    fn from(t: &TeamCfg) -> Roles {
        Roles {
            planner: t.planner.clone(),
            worker: t.worker.clone(),
            light_worker: t.light_worker.clone().filter(|s| !s.is_empty()),
            reviewer: t.reviewer.clone().filter(|s| !s.is_empty()),
            auto_run: t.auto_run,
            parallel: t.parallel,
            max_steps: t.max_steps.unwrap_or(8),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Delegation {
    pub id: u64,
    pub created: u64,
    pub task: String,
    pub harness: String,
    pub node: String,
    pub target: String,
    pub team: Option<String>,
    pub roles: Roles,
    /// planning | review | running | done | failed | cancelled
    pub status: &'static str,
    pub error: Option<String>,
    pub planner_proc: Option<u64>,
    pub plan: Option<Plan>,
    /// The workflow run carrying the plan out.
    pub workflow: Option<u64>,
}

#[derive(Default)]
pub struct Delegations {
    items: Mutex<BTreeMap<u64, Delegation>>,
}

fn now() -> u64 {
    crate::deck::runs::now()
}

/// The plan in a planner's answer: the last JSON object in it that has a
/// `steps` list (agents sometimes talk before or after it, or fence it).
pub fn parse_plan(text: &str) -> Result<Plan, String> {
    let text = if text.len() > 400_000 {
        &text[text.len() - 400_000..]
    } else {
        text
    };
    let mut found: Option<Plan> = None;
    for (i, _) in text.match_indices('{') {
        let mut it = serde_json::Deserializer::from_str(&text[i..]).into_iter::<Value>();
        if let Some(Ok(v)) = it.next() {
            if v.get("steps").map(Value::is_array).unwrap_or(false) {
                if let Ok(p) = serde_json::from_value::<Plan>(v) {
                    found = Some(p);
                }
            }
        }
    }
    found.ok_or_else(|| {
        "the planner did not answer with a plan (a JSON object with steps)".to_string()
    })
}

/// Make a plan safe to run: ids that the engine accepts and that are unique,
/// dependencies on known earlier steps only, no empty steps, at most `max`.
pub fn clean_plan(mut p: Plan, max: usize) -> Result<Plan, String> {
    let mut seen: Vec<String> = Vec::new();
    let mut renamed: BTreeMap<String, String> = BTreeMap::new();
    let mut out = Vec::new();
    for (n, mut s) in p.steps.drain(..).enumerate() {
        if s.prompt.trim().is_empty() && s.title.trim().is_empty() {
            continue;
        }
        let mut id: String =
            s.id.trim()
                .to_lowercase()
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
                .trim_matches('-')
                .chars()
                .take(40)
                .collect();
        if id.is_empty() || id == "review" {
            id = format!("step-{}", n + 1);
        }
        while seen.contains(&id) {
            id.push('x');
        }
        renamed.insert(s.id.clone(), id.clone());
        s.needs = s
            .needs
            .iter()
            .filter_map(|d| renamed.get(d).cloned())
            .filter(|d| *d != id)
            .collect();
        s.needs.dedup();
        s.id = id.clone();
        if s.title.trim().is_empty() {
            s.title = s.id.clone();
        }
        if s.prompt.trim().is_empty() {
            s.prompt = s.title.clone();
        }
        s.size = if s.size.trim().eq_ignore_ascii_case("light") {
            "light".into()
        } else {
            "standard".into()
        };
        s.title = s.title.chars().take(200).collect();
        s.prompt = s.prompt.chars().take(20_000).collect();
        seen.push(id);
        out.push(s);
    }
    if out.is_empty() {
        return Err("the plan has no steps".into());
    }
    if out.len() > max {
        return Err(format!(
            "the plan has {} steps; this team allows {}",
            out.len(),
            max
        ));
    }
    p.steps = out;
    p.summary = p.summary.chars().take(4000).collect();
    Ok(p)
}

fn step_prompt(task: &str, plan: &Plan, s: &Step) -> String {
    let outline: Vec<String> = plan
        .steps
        .iter()
        .enumerate()
        .map(|(i, x)| {
            format!(
                "{}. [{}] {}{}",
                i + 1,
                x.id,
                x.title,
                if x.id == s.id { "  <- yours" } else { "" }
            )
        })
        .collect();
    format!(
        "Plan step {}: {}\n\nYou are one of several agents carrying out a plan, one step each. Do only your step, then stop.\n\n\
Goal: {}\n\nThe plan: {}\n{}\n\nYour step ({}): {}\n\n{}\n\n\
When you are done, reply with a short summary of what you changed and how you checked it.",
        s.id,
        s.title,
        task.trim(),
        plan.summary.trim(),
        outline.join("\n"),
        s.id,
        s.title,
        s.prompt.trim()
    )
}

fn review_prompt(task: &str, plan: &Plan) -> String {
    let outline: Vec<String> = plan
        .steps
        .iter()
        .enumerate()
        .map(|(i, x)| {
            format!(
                "{}. {}: {}",
                i + 1,
                x.title,
                x.prompt.lines().next().unwrap_or("")
            )
        })
        .collect();
    format!(
        "Review the plan's result\n\nOther agents have just carried out this plan in this directory. Review their work.\n\n\
Goal: {}\n\nThe plan: {}\n{}\n\n\
Look at what changed (for example git status and git diff), run the checks the plan names, and fix small mistakes. \
Then report: what is done, what is missing or wrong, and anything risky that a person should look at.",
        task.trim(),
        plan.summary.trim(),
        outline.join("\n")
    )
}

/// The plan as a workflow: one task per step, then the review.
pub fn to_workflow(d: &Delegation, plan: &Plan, settings: &Settings) -> WorkflowCfg {
    let runner_of = |profile: &str| {
        settings
            .profile(profile)
            .and_then(|p| p.runner.clone())
            .unwrap_or_else(|| "claude".into())
    };
    let mut tasks: Vec<TaskCfg> = Vec::new();
    let mut prev: Option<String> = None;
    for s in &plan.steps {
        let profile = if s.size == "light" {
            d.roles
                .light_worker
                .clone()
                .unwrap_or_else(|| d.roles.worker.clone())
        } else {
            d.roles.worker.clone()
        };
        let mut needs = s.needs.clone();
        // Steps share one directory: unless the team allows it, one at a time.
        if !d.roles.parallel {
            if let Some(p) = &prev {
                if !needs.contains(p) {
                    needs.push(p.clone());
                }
            }
        }
        tasks.push(TaskCfg {
            id: s.id.clone(),
            node: d.node.clone(),
            target: d.target.clone(),
            runner: runner_of(&profile),
            profile: Some(profile),
            prompt: step_prompt(&d.task, plan, s),
            needs,
            ..Default::default()
        });
        prev = Some(s.id.clone());
    }
    if let Some(r) = &d.roles.reviewer {
        let needs = plan.steps.iter().map(|s| s.id.clone()).collect();
        tasks.push(TaskCfg {
            id: "review".into(),
            node: d.node.clone(),
            target: d.target.clone(),
            runner: runner_of(r),
            profile: Some(r.clone()),
            prompt: review_prompt(&d.task, plan),
            needs,
            ..Default::default()
        });
    }
    WorkflowCfg {
        name: format!("plan-{}", d.id),
        description: d.task.chars().take(200).collect(),
        harness: Some(d.harness.clone()),
        tasks,
    }
}

/// Start an agent with `profile` in the delegation's place.
fn launch(app: &App, d: &Delegation, profile: &str, prompt: &str) -> Result<u64, String> {
    let settings = app.settings();
    let prof = settings
        .profile(profile)
        .ok_or_else(|| format!("profile '{}' is not configured", profile))?;
    let runner = prof.runner.clone().unwrap_or_else(|| "claude".into());
    let (prompt, memory, reflect) =
        crate::deck::knowledge::prepare(&settings, &d.harness, prompt, Some(profile), None, None);
    let start = StartOpts {
        agent: prof.opts(),
        profile: Some(profile.to_string()),
        memory,
        reflect,
        ..Default::default()
    };
    if d.node == LOCAL {
        let r = settings
            .runners
            .iter()
            .find(|r| r.name == runner)
            .ok_or_else(|| format!("runner '{}' is not configured", runner))?;
        app.procs.start_with(
            Launch::Local {
                runner: r,
                harness: &d.harness,
                prompt: &prompt,
                target: &PathBuf::from(&d.target),
            },
            start,
        )
    } else {
        let n = settings
            .node(&d.node)
            .ok_or_else(|| format!("unknown node '{}'", d.node))?;
        app.procs.start_with(
            Launch::Remote {
                node: n,
                runner: &runner,
                harness: &d.harness,
                prompt: &prompt,
                target: &d.target,
            },
            start,
        )
    }
}

/// Check that every role names a configured profile.
pub fn check_roles(settings: &Settings, r: &Roles) -> Result<(), String> {
    for p in [
        Some(&r.planner),
        Some(&r.worker),
        r.light_worker.as_ref(),
        r.reviewer.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if settings.profile(p).is_none() {
            return Err(format!("no profile named '{}'", p));
        }
    }
    if !(1..=30).contains(&r.max_steps) {
        return Err("max_steps must be 1 to 30".into());
    }
    Ok(())
}

impl Delegations {
    pub fn list(&self) -> Vec<Delegation> {
        self.items.lock().unwrap().values().rev().cloned().collect()
    }

    pub fn get(&self, id: u64) -> Option<Delegation> {
        self.items.lock().unwrap().get(&id).cloned()
    }

    fn edit(&self, id: u64, f: impl FnOnce(&mut Delegation)) {
        if let Some(d) = self.items.lock().unwrap().get_mut(&id) {
            f(d);
        }
    }

    /// Start planning `task` in `target` on `node`.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &self,
        app: &App,
        task: &str,
        harness: &str,
        node: &str,
        target: &str,
        team: Option<String>,
        roles: Roles,
    ) -> Result<u64, String> {
        check_roles(&app.settings(), &roles)?;
        let id = {
            let mut items = self.items.lock().unwrap();
            let id = items.keys().next_back().copied().unwrap_or(0) + 1;
            items.insert(
                id,
                Delegation {
                    id,
                    created: now(),
                    task: task.to_string(),
                    harness: harness.to_string(),
                    node: node.to_string(),
                    target: target.to_string(),
                    team,
                    roles,
                    status: "planning",
                    error: None,
                    planner_proc: None,
                    plan: None,
                    workflow: None,
                },
            );
            id
        };
        let d = self.get(id).unwrap();
        let prompt = format!("{}\n\n---\n{}", task.trim(), plan_prompt(d.roles.max_steps));
        debug_assert!(prompt.contains(PLAN_MARK));
        match launch(app, &d, &d.roles.planner, &prompt) {
            Ok(pid) => {
                self.edit(id, |d| d.planner_proc = Some(pid));
                Ok(id)
            }
            Err(e) => {
                self.items.lock().unwrap().remove(&id);
                Err(e)
            }
        }
    }

    /// Run the plan (as written, or as a person edited it).
    pub fn approve(&self, app: &App, id: u64, edited: Option<Plan>) -> Result<u64, String> {
        let d = self.get(id).ok_or("no such plan")?;
        if d.status != "review" {
            return Err(format!("this plan is {}, not waiting for review", d.status));
        }
        let plan = match edited {
            Some(p) => clean_plan(p, d.roles.max_steps)?,
            None => d.plan.clone().ok_or("no plan to run")?,
        };
        let wf = to_workflow(&d, &plan, &app.settings());
        let run = app.engine.start(wf)?;
        self.edit(id, |d| {
            d.plan = Some(plan);
            d.workflow = Some(run);
            d.status = "running";
        });
        Ok(run)
    }

    pub fn cancel(&self, app: &App, id: u64) -> Result<(), String> {
        let d = self.get(id).ok_or("no such plan")?;
        match d.status {
            "planning" => {
                if let Some(p) = d.planner_proc.and_then(|p| app.procs.get(p)) {
                    p.request_stop();
                }
            }
            "running" => {
                if let Some(w) = d.workflow {
                    app.engine.cancel(w);
                }
            }
            "review" => {}
            _ => return Err(format!("this plan is {} already", d.status)),
        }
        self.edit(id, |d| d.status = "cancelled");
        Ok(())
    }
}

/// Move delegations along: read finished plans, follow running ones.
pub fn tick(app: &App) {
    for d in app.delegations.list() {
        match d.status {
            "planning" => {
                let Some(p) = d.planner_proc.and_then(|p| app.procs.get(p)) else {
                    app.delegations.edit(d.id, |d| {
                        d.status = "failed";
                        d.error = Some("the planner's process is gone".into());
                    });
                    continue;
                };
                if p.is_running() {
                    continue;
                }
                let v = p.view();
                let parsed =
                    parse_plan(&p.outcome()).and_then(|x| clean_plan(x, d.roles.max_steps));
                match parsed {
                    Ok(plan) if v.status != "stopped" => {
                        let auto = d.roles.auto_run;
                        app.delegations.edit(d.id, |d| {
                            d.plan = Some(plan);
                            d.status = "review";
                        });
                        if auto {
                            if let Err(e) = app.delegations.approve(app, d.id, None) {
                                app.delegations.edit(d.id, |d| {
                                    d.status = "failed";
                                    d.error = Some(e);
                                });
                            }
                        }
                    }
                    Ok(_) => app.delegations.edit(d.id, |d| {
                        d.status = "cancelled";
                    }),
                    Err(e) => app.delegations.edit(d.id, |d| {
                        d.status = "failed";
                        d.error = Some(format!("{} (planner {})", e, v.status));
                    }),
                }
            }
            "running" => {
                let Some(run) = d.workflow.and_then(|w| app.engine.get(w)) else {
                    continue;
                };
                let status = match run.status {
                    "succeeded" => "done",
                    "failed" => "failed",
                    "cancelled" => "cancelled",
                    _ => continue,
                };
                app.delegations.edit(d.id, |d| d.status = status);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_are_found_in_chatty_answers_and_made_safe() {
        let answer = "Here is the plan.\n```json\n{\"summary\": \"Add a flag\", \"steps\": [\
{\"id\": \"Add Flag!\", \"title\": \"Add the flag\", \"prompt\": \"Edit src/cli.rs\", \"size\": \"standard\"},\
{\"id\": \"docs\", \"title\": \"Document it\", \"prompt\": \"README\", \"needs\": [\"Add Flag!\", \"nope\", \"docs\"], \"size\": \"LIGHT\"},\
{\"id\": \"docs\", \"title\": \"\", \"prompt\": \"\"},\
{\"id\": \"review\", \"title\": \"Second\", \"prompt\": \"x\"}]}\n```\nDone.";
        let p = clean_plan(parse_plan(answer).unwrap(), 8).unwrap();
        let ids: Vec<&str> = p.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["add-flag", "docs", "step-4"]);
        assert_eq!(p.steps[1].needs, vec!["add-flag".to_string()]);
        assert_eq!(p.steps[1].size, "light");
        assert!(parse_plan("no plan here {\"x\": 1}").is_err());
        assert!(clean_plan(parse_plan(answer).unwrap(), 2).is_err());
    }
}
