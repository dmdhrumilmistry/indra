//! The board: work items in columns, run by agents, synced with trackers.
//!
//! An item is generic (a title, a body, labels, and how to run it), so the
//! board serves security reviews, fixes, research or anything else an agent
//! can do. Columns: todo -> running -> review | failed -> done. An agent
//! finishing moves its item to review (or failed); only a person moves an
//! item to done. Items imported from a tracker carry their source, and each
//! move is reported back: a comment, and the mapped state.

use crate::deck::config::{ConnectorCfg, Settings};
use crate::deck::procs::{Launch, StartOpts};
use crate::deck::trackers::{Remote, Tracker};
use crate::deck::web::App;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub type Result<T> = std::result::Result<T, String>;

pub const COLUMNS: [&str; 5] = ["todo", "running", "review", "done", "failed"];

const PROMPT: &str = "Work on this task.\n\nTitle: {title}\nKey: {key}\nLink: {url}\n\n{body}\n\n\
When you are done, summarise what you did, what you changed, and anything left to do.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub connector: String,
    pub key: String,
    pub url: String,
    /// Its state in the tracker when last seen or set.
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub ts: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub column: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub runner: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub proc_id: Option<u64>,
    #[serde(default)]
    pub procs: Vec<u64>,
    #[serde(default)]
    pub result: Option<String>,
    pub created: u64,
    pub updated: u64,
    #[serde(default)]
    pub notes: Vec<Note>,
}

/// Fields a person can set on an item.
#[derive(Debug, Default, Deserialize)]
pub struct Patch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub labels: Option<Vec<String>>,
    pub column: Option<String>,
    pub profile: Option<String>,
    pub runner: Option<String>,
    pub node: Option<String>,
    pub target: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Data {
    next: u64,
    items: Vec<Item>,
}

pub struct Board {
    path: PathBuf,
    data: Mutex<Data>,
    /// When each connector last imported (for poll_minutes).
    polled: Mutex<HashMap<String, u64>>,
    /// One reconcile at a time.
    ticking: tokio::sync::Mutex<()>,
}

fn now() -> u64 {
    crate::deck::runs::now()
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}

impl Board {
    pub fn open(path: PathBuf) -> Board {
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Board {
            path,
            data: Mutex::new(data),
            polled: Mutex::new(HashMap::new()),
            ticking: tokio::sync::Mutex::new(()),
        }
    }

    fn save(&self, d: &Data) {
        if let Ok(text) = serde_json::to_string_pretty(d) {
            if let Some(dir) = self.path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = crate::deck::confedit::write_atomic(&self.path, &text);
        }
    }

    pub fn items(&self) -> Vec<Item> {
        self.data.lock().unwrap().items.clone()
    }

    pub fn get(&self, id: u64) -> Option<Item> {
        self.data
            .lock()
            .unwrap()
            .items
            .iter()
            .find(|i| i.id == id)
            .cloned()
    }

    fn edit<T>(&self, id: u64, f: impl FnOnce(&mut Item) -> Result<T>) -> Result<T> {
        let mut d = self.data.lock().unwrap();
        let it = d
            .items
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or("no such item")?;
        let r = f(it)?;
        it.updated = now();
        self.save(&d);
        Ok(r)
    }

    pub fn create(&self, title: &str, p: Patch, source: Option<Source>) -> Result<u64> {
        if title.trim().is_empty() || title.len() > 500 {
            return Err("an item needs a title (up to 500 characters)".into());
        }
        let mut d = self.data.lock().unwrap();
        d.next += 1;
        let id = d.next;
        let t = now();
        let mut it = Item {
            id,
            title: title.trim().to_string(),
            body: String::new(),
            labels: vec![],
            column: "todo".into(),
            profile: None,
            runner: None,
            node: None,
            target: None,
            source,
            proc_id: None,
            procs: vec![],
            result: None,
            created: t,
            updated: t,
            notes: vec![],
        };
        apply(&mut it, p)?;
        it.column = "todo".into();
        d.items.push(it);
        self.save(&d);
        Ok(id)
    }

    pub fn update(&self, id: u64, p: Patch) -> Result<Item> {
        self.edit(id, |it| {
            if let Some(c) = &p.column {
                if c == "running" && it.column != "running" {
                    return Err(
                        "an item becomes running when an agent starts on it: use Run".into(),
                    );
                }
                if it.column == "running" && c != "running" {
                    return Err("an agent is working on it; stop the agent first".into());
                }
            }
            apply(it, p)?;
            Ok(it.clone())
        })
    }

    pub fn delete(&self, id: u64) -> Result<()> {
        let mut d = self.data.lock().unwrap();
        let n = d.items.len();
        d.items.retain(|i| i.id != id || i.column == "running");
        if d.items.len() == n {
            return Err("no such item, or an agent is working on it".into());
        }
        self.save(&d);
        Ok(())
    }

    pub fn note(&self, id: u64, text: String) {
        let _ = self.edit(id, |it| {
            it.notes.push(Note { ts: now(), text });
            if it.notes.len() > 50 {
                it.notes.remove(0);
            }
            Ok(())
        });
    }

    /// Add tracker items that are not on the board yet; refresh the others' text.
    fn merge(&self, connector: &str, remote: Vec<Remote>) -> (usize, usize) {
        let mut d = self.data.lock().unwrap();
        let (mut added, mut updated) = (0, 0);
        for r in remote {
            if let Some(it) = d.items.iter_mut().find(|i| {
                i.source
                    .as_ref()
                    .map(|s| s.connector == connector && s.key == r.key)
                    .unwrap_or(false)
            }) {
                if it.title != r.title || it.body != r.body {
                    it.title = r.title.clone();
                    it.body = r.body.clone();
                    it.updated = now();
                    updated += 1;
                }
                continue;
            }
            d.next += 1;
            let t = now();
            let id = d.next;
            d.items.push(Item {
                id,
                title: if r.title.is_empty() {
                    r.key.clone()
                } else {
                    r.title.clone()
                },
                body: r.body.clone(),
                labels: vec![],
                column: "todo".into(),
                profile: None,
                runner: None,
                node: None,
                target: None,
                source: Some(Source {
                    connector: connector.into(),
                    key: r.key,
                    url: r.url,
                    state: r.state,
                }),
                proc_id: None,
                procs: vec![],
                result: None,
                created: t,
                updated: t,
                notes: vec![Note {
                    ts: t,
                    text: format!("imported from {}", connector),
                }],
            });
            added += 1;
        }
        self.save(&d);
        (added, updated)
    }
}

fn apply(it: &mut Item, p: Patch) -> Result<()> {
    let opt = |v: Option<String>| v.map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    if let Some(t) = p.title {
        if t.trim().is_empty() {
            return Err("the title cannot be empty".into());
        }
        it.title = t.trim().to_string();
    }
    if let Some(b) = p.body {
        if b.len() > 100_000 {
            return Err("the body is too long".into());
        }
        it.body = b;
    }
    if let Some(l) = p.labels {
        it.labels = l
            .into_iter()
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
    }
    if let Some(c) = p.column {
        if !COLUMNS.contains(&c.as_str()) {
            return Err(format!("column must be one of {}", COLUMNS.join(", ")));
        }
        it.column = c;
    }
    if p.profile.is_some() {
        it.profile = opt(p.profile);
    }
    if p.runner.is_some() {
        it.runner = opt(p.runner);
    }
    if p.node.is_some() {
        it.node = opt(p.node);
    }
    if p.target.is_some() {
        it.target = opt(p.target);
    }
    Ok(())
}

/// A connector with its token taken from the secret store. A secret can be
/// scoped to a connector with the profile entry `connector:<name>`.
fn tracker<'a>(app: &App, c: &'a ConnectorCfg) -> Result<Tracker<'a>> {
    let env: std::collections::BTreeMap<String, String> = [("TOKEN".to_string(), c.token.clone())]
        .into_iter()
        .collect();
    let scope = format!("connector:{}", c.name);
    let (resolved, _) = app
        .procs
        .vault()
        .resolve(&env, Some(&scope), crate::deck::nodes::LOCAL)?;
    Ok(Tracker {
        cfg: c,
        token: resolved["TOKEN"].clone(),
    })
}

/// Import from one connector now. Returns (added, updated).
pub async fn import(app: &App, connector: &str) -> Result<(usize, usize)> {
    let settings = app.settings();
    let c = settings
        .connector(connector)
        .ok_or_else(|| format!("no connector named '{}'", connector))?;
    let t = tracker(app, c)?;
    let remote = t.list().await?;
    app.polled(connector);
    Ok(app.board.merge(connector, remote))
}

/// Where and how an item runs: explicit choices, then the item's, then its
/// connector's, then the first target of the harness on this machine.
pub struct RunReq {
    pub profile: Option<String>,
    pub runner: Option<String>,
    pub node: Option<String>,
    pub target: Option<String>,
}

fn prompt_for(it: &Item, c: Option<&ConnectorCfg>) -> String {
    let tpl = c
        .and_then(|c| c.prompt.clone())
        .unwrap_or_else(|| PROMPT.to_string());
    let (key, url) = it
        .source
        .as_ref()
        .map(|s| (s.key.clone(), s.url.clone()))
        .unwrap_or_else(|| (format!("board-{}", it.id), String::new()));
    tpl.replace("{title}", &it.title)
        .replace("{key}", &key)
        .replace("{url}", &url)
        .replace("{body}", &it.body)
}

/// Start an agent on an item: it moves to running, and its tracker hears.
pub async fn run(app: &App, id: u64, req: RunReq) -> Result<u64> {
    let it = app.board.get(id).ok_or("no such item")?;
    if it.column == "running" {
        return Err("an agent is already working on it".into());
    }
    let settings = app.settings();
    let conn = it
        .source
        .as_ref()
        .and_then(|s| settings.connector(&s.connector));
    let pick = |a: Option<String>, b: &Option<String>, c: Option<String>| {
        a.filter(|x| !x.is_empty()).or_else(|| b.clone()).or(c)
    };
    let profile = pick(
        req.profile,
        &it.profile,
        conn.and_then(|c| c.profile.clone()),
    );
    let node = pick(req.node, &it.node, conn.and_then(|c| c.node.clone()))
        .unwrap_or_else(|| crate::deck::nodes::LOCAL.into());
    let target = pick(req.target, &it.target, conn.and_then(|c| c.target.clone()));
    let runner = pick(req.runner, &it.runner, None).unwrap_or_default();
    let (runner, agent) = crate::deck::web::resolve_agent(
        &settings,
        profile.as_deref(),
        &runner,
        &Default::default(),
    )
    .map_err(|e| e.message())?;
    let harness = settings
        .harnesses
        .first()
        .map(|h| h.name.clone())
        .unwrap_or_default();
    let (prompt, memory, reflect) = crate::deck::knowledge::prepare(
        &settings,
        &harness,
        &prompt_for(&it, conn),
        profile.as_deref(),
        None,
        None,
    );
    let start = StartOpts {
        agent,
        profile: profile.clone(),
        memory,
        reflect,
        ..Default::default()
    };
    let pid = if node == crate::deck::nodes::LOCAL {
        let target = match target {
            Some(t) => PathBuf::from(t),
            None => settings
                .harnesses
                .first()
                .and_then(|h| h.targets.first().cloned())
                .ok_or("no target: set one on the item, its connector, or the harness")?,
        };
        let r = settings
            .runners
            .iter()
            .find(|r| r.name == runner)
            .ok_or_else(|| format!("unknown runner '{}'", runner))?;
        app.procs.start_with(
            Launch::Local {
                runner: r,
                harness: &harness,
                prompt: &prompt,
                target: &target,
            },
            start,
        )?
    } else {
        let n = settings
            .node(&node)
            .ok_or_else(|| format!("unknown node '{}'", node))?;
        let target = target
            .or_else(|| n.targets.first().cloned())
            .ok_or_else(|| format!("no target on node '{}'", node))?;
        app.procs.start_with(
            Launch::Remote {
                node: n,
                runner: &runner,
                harness: &harness,
                prompt: &prompt,
                target: &target,
            },
            start,
        )?
    };
    app.board.edit(id, |it| {
        it.column = "running".into();
        it.proc_id = Some(pid);
        it.procs.push(pid);
        it.result = None;
        it.notes.push(Note {
            ts: now(),
            text: format!(
                "agent process #{} started{}",
                pid,
                profile
                    .as_deref()
                    .map(|p| format!(" with profile {}", p))
                    .unwrap_or_default()
            ),
        });
        Ok(())
    })?;
    if let Some(c) = conn {
        report(
            app,
            id,
            c,
            "running",
            &format!("indra started an agent on this (process #{}).", pid),
        )
        .await;
    }
    Ok(pid)
}

/// Tell the tracker: a comment, then the state mapped to `column` (if any).
async fn report(app: &App, id: u64, c: &ConnectorCfg, column: &str, comment: &str) {
    let Some(it) = app.board.get(id) else { return };
    let Some(src) = it.source else { return };
    let t = match tracker(app, c) {
        Ok(t) => t,
        Err(e) => {
            app.board
                .note(id, format!("could not reach {}: {}", c.name, e));
            return;
        }
    };
    if !comment.is_empty() {
        if let Err(e) = t.comment(&src.key, comment).await {
            app.board
                .note(id, format!("comment on {} failed: {}", src.key, e));
        }
    }
    if let Some(state) = c.states.get(column) {
        match t.transition(&src.key, state).await {
            Ok(()) => {
                let s = state.clone();
                let _ = app.board.edit(id, |it| {
                    if let Some(src) = it.source.as_mut() {
                        src.state = s.clone();
                    }
                    it.notes.push(Note {
                        ts: now(),
                        text: format!("{} moved to {}", src.key, s),
                    });
                    Ok(())
                });
            }
            Err(e) => app
                .board
                .note(id, format!("moving {} to {} failed: {}", src.key, state, e)),
        }
    }
}

/// A person moved an item: report done (and the other mapped columns) back.
pub async fn moved(app: &App, id: u64, column: &str) {
    let settings = app.settings();
    let Some(it) = app.board.get(id) else { return };
    let Some(src) = &it.source else { return };
    let Some(c) = settings.connector(&src.connector) else {
        return;
    };
    let comment = if column == "done" {
        "Accepted as done in indra."
    } else {
        ""
    };
    report(app, id, c, column, comment).await;
}

/// Keep the board moving: finished agents move their items (and report),
/// connectors with poll_minutes import, and auto_run connectors start agents
/// on new items while their node has room. Safe to call often.
pub async fn tick(app: &App) {
    let Ok(_one) = app.board.ticking.try_lock() else {
        return;
    };
    let settings = app.settings();
    // 1. Finished agents.
    for it in app
        .board
        .items()
        .into_iter()
        .filter(|i| i.column == "running")
    {
        let Some(pid) = it.proc_id else { continue };
        let (done, ok, outcome) = match app.procs.get(pid) {
            Some(p) if p.is_running() => continue,
            Some(p) => {
                let v = p.view();
                (true, v.status == "exited", p.outcome())
            }
            // The console restarted: the process is gone.
            None => (
                true,
                false,
                "the agent's process is gone (the console restarted)".to_string(),
            ),
        };
        if !done {
            continue;
        }
        let column = if ok { "review" } else { "failed" };
        let result = clip(&outcome, 20_000);
        let _ = app.board.edit(it.id, |x| {
            x.column = column.into();
            x.result = Some(result.clone());
            x.notes.push(Note {
                ts: now(),
                text: format!(
                    "agent process #{} {}",
                    pid,
                    if ok { "finished" } else { "failed" }
                ),
            });
            Ok(())
        });
        if let Some(c) = it
            .source
            .as_ref()
            .and_then(|s| settings.connector(&s.connector))
        {
            let comment = format!(
                "indra agent process #{} {}.\n\n{}",
                pid,
                if ok {
                    "finished; it is ready for review"
                } else {
                    "did not finish"
                },
                clip(&result, 6000)
            );
            report(app, it.id, c, column, &comment).await;
        }
    }
    // 2. Scheduled imports, then 3. automatic runs.
    for c in &settings.connectors {
        if let Some(m) = c.poll_minutes.filter(|m| *m > 0) {
            let last = app
                .board
                .polled
                .lock()
                .unwrap()
                .get(&c.name)
                .copied()
                .unwrap_or(0);
            if now().saturating_sub(last) >= m * 60 {
                app.polled(&c.name);
                if let Err(e) = import(app, &c.name).await {
                    eprintln!("indra: import from {} failed: {}", c.name, e);
                }
            }
        }
        if !c.auto_run {
            continue;
        }
        let node = c
            .node
            .clone()
            .unwrap_or_else(|| crate::deck::nodes::LOCAL.into());
        for it in app.board.items().into_iter().filter(|i| {
            i.column == "todo"
                && i.procs.is_empty()
                && i.source
                    .as_ref()
                    .map(|s| s.connector == c.name)
                    .unwrap_or(false)
        }) {
            if app.procs.running_on(&node) >= settings.capacity(&node) {
                break;
            }
            let req = RunReq {
                profile: None,
                runner: None,
                node: None,
                target: None,
            };
            if let Err(e) = run(app, it.id, req).await {
                app.board
                    .note(it.id, format!("could not start automatically: {}", e));
            }
        }
    }
}

impl App {
    fn polled(&self, connector: &str) {
        self.board
            .polled
            .lock()
            .unwrap()
            .insert(connector.to_string(), now());
    }
}

/// Settings a board view needs alongside the items.
pub fn connectors_view(s: &Settings) -> serde_json::Value {
    serde_json::json!(s
        .connectors
        .iter()
        .map(|c| serde_json::json!({
            "name": c.name, "kind": c.kind, "project": c.project, "auto_run": c.auto_run,
            "poll_minutes": c.poll_minutes, "states": c.states, "profile": c.profile,
        }))
        .collect::<Vec<_>>())
}
