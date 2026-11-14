//! Knowledge for agents: the memory vaults behind a harness, managed from
//! the console; the memory an agent is given when it starts; and the
//! learnings agents propose when they finish, reviewed before they become
//! memory.
//!
//! - **Context.** With `memory` on (per launch, or in a profile), the notes
//!   most relevant to the prompt are recalled from the harness's vaults
//!   (`indra mem recall`, graph-aware) and appended to the prompt. The agent's
//!   page lists what it was given.
//! - **Learnings.** With `reflect` on, an interactive Claude Code agent is
//!   asked one more question before its input closes: what it learned that a
//!   future agent should know. Its answer lands in the learnings inbox as
//!   proposals; nothing reaches memory until a person accepts it. People (and
//!   `indra attach`) can propose learnings too.

use crate::deck::config::{Harness, Settings};
use crate::deck::web::{bad, fingerprint, forbidden, internal, not_found, ApiResult, App};
use crate::mem::cli as mem;
use crate::mem::graph::Graph;
use crate::mem::vault::{self, Vault};
use axum::extract::{Path as UrlPath, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A memory note an agent was given.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemRef {
    /// Index of the vault in its harness's memory list.
    pub vault: usize,
    pub id: String,
    pub title: String,
}

/// Notes and characters of memory put in a prompt.
const BRIEF_NOTES: usize = 6;
const BRIEF_CHARS: usize = 4000;

/// The last question a reflecting agent is asked.
pub const REFLECT_PROMPT: &str = "Before you finish: what did you learn in this task that would help a future agent \
working on this codebase or a similar task? Reply with only a JSON array (at most 5 items) of objects \
{\"title\": a short title, \"body\": the learning in one to four sentences, with the evidence (files, commands, \
versions), \"tags\": [a few lowercase tags]}. Keep only durable, non-obvious knowledge: conventions, pitfalls, \
where things live, what worked and what did not. Reply [] if nothing is worth keeping. Do not use any tools.";

fn now() -> u64 {
    crate::deck::runs::now()
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!(
            "{}...",
            s.chars().take(n.saturating_sub(3)).collect::<String>()
        )
    }
}

/// The notes most relevant to `prompt`, from every existing vault of the
/// harness, and the prompt with them appended (after it, so a leading slash
/// command stays first).
pub fn brief(h: &Harness, prompt: &str) -> (String, Vec<MemRef>) {
    let query = clip(prompt, 1000);
    let mut found: Vec<(usize, mem::Recalled)> = Vec::new();
    for (i, p) in h.memory_paths().iter().enumerate() {
        if !p.join(vault::MARKER).is_dir() && !p.is_dir() {
            continue;
        }
        let o = mem::RecallOpts {
            limit: BRIEF_NOTES,
            max_chars: BRIEF_CHARS,
            ..Default::default()
        };
        if let Ok(r) = mem::recall(p, &query, &o) {
            found.extend(r.results.into_iter().map(|n| (i, n)));
        }
    }
    found.sort_by(|a, b| b.1.score.total_cmp(&a.1.score));
    found.truncate(BRIEF_NOTES);
    if found.is_empty() {
        return (prompt.to_string(), vec![]);
    }
    let mut budget = BRIEF_CHARS;
    let mut out = format!(
        "{}\n\n---\nRelevant memory ({} notes recalled by indra from earlier work; use what applies and verify it before relying on it):\n\n",
        prompt.trim_end(),
        found.len()
    );
    let mut refs = Vec::new();
    for (vi, mut n) in found {
        if n.text.chars().count() > budget {
            n.text = clip(&n.text, budget);
        }
        budget = budget.saturating_sub(n.text.chars().count());
        out.push_str(&mem::recalled_block(&n));
        out.push('\n');
        refs.push(MemRef {
            vault: vi,
            id: n.id,
            title: n.title,
        });
    }
    (out.trim_end().to_string(), refs)
}

/// What a launch gets from its profile and the request: the prompt (with
/// memory when asked), the notes it was given, and whether to reflect.
pub fn prepare(
    settings: &Settings,
    harness: &str,
    prompt: &str,
    profile: Option<&str>,
    memory: Option<bool>,
    reflect: Option<bool>,
) -> (String, Vec<MemRef>, bool) {
    let prof = profile.and_then(|p| settings.profile(p));
    let memory = memory.unwrap_or(prof.map(|p| p.memory).unwrap_or(false));
    let reflect = reflect.unwrap_or(prof.map(|p| p.reflect).unwrap_or(false));
    if !memory {
        return (prompt.to_string(), vec![], reflect);
    }
    match settings.harnesses.iter().find(|h| h.name == harness) {
        Some(h) => {
            let (p, refs) = brief(h, prompt);
            (p, refs, reflect)
        }
        None => (prompt.to_string(), vec![], reflect),
    }
}

/// Learnings in an agent's answer to [`REFLECT_PROMPT`].
pub fn parse_reflection(text: &str) -> Vec<(String, String, Vec<String>)> {
    let t = text.trim();
    let parsed = match (t.find('['), t.rfind(']')) {
        (Some(a), Some(b)) if a < b => serde_json::from_str::<Value>(&t[a..=b]).ok(),
        _ => None,
    };
    if let Some(Value::Array(items)) = parsed {
        return items
            .iter()
            .filter_map(|x| {
                let title = x.get("title").and_then(Value::as_str)?.trim();
                let body = x.get("body").and_then(Value::as_str).unwrap_or("").trim();
                if title.is_empty() {
                    return None;
                }
                let tags = x
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|s| clip(s, 40))
                            .take(8)
                            .collect()
                    })
                    .unwrap_or_default();
                Some((clip(title, 120), clip(body, 4000), tags))
            })
            .take(8)
            .collect();
    }
    if t.is_empty() || t == "[]" {
        return vec![];
    }
    // Not the format asked for: keep the text as one proposal to edit.
    vec![("Learnings".into(), clip(t, 4000), vec![])]
}

// ---- learnings ------------------------------------------------------------------

/// Where a learning came from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Origin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proc: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub runner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// "agent" (it reflected) or "person".
    #[serde(default)]
    pub by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Learning {
    pub id: u64,
    pub created: u64,
    /// proposed | accepted
    pub status: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub origin: Origin,
    /// The note it became: (vault path, note id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<(String, String)>,
}

#[derive(Default, Serialize, Deserialize)]
struct Data {
    next: u64,
    items: Vec<Learning>,
}

/// The learnings inbox, kept in `learnings.json` in the data directory.
pub struct Learnings {
    path: PathBuf,
    data: Mutex<Data>,
}

impl Learnings {
    pub fn open(path: PathBuf) -> Learnings {
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Learnings {
            path,
            data: Mutex::new(data),
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

    pub fn list(&self) -> Vec<Learning> {
        let mut v = self.data.lock().unwrap().items.clone();
        v.sort_by(|a, b| {
            (a.status != "proposed")
                .cmp(&(b.status != "proposed"))
                .then(b.id.cmp(&a.id))
        });
        v
    }

    pub fn proposed(&self) -> usize {
        self.data
            .lock()
            .unwrap()
            .items
            .iter()
            .filter(|l| l.status == "proposed")
            .count()
    }

    pub fn add(&self, title: &str, body: &str, tags: Vec<String>, origin: Origin) -> u64 {
        let mut d = self.data.lock().unwrap();
        d.next += 1;
        let id = d.next;
        d.items.push(Learning {
            id,
            created: now(),
            status: "proposed".into(),
            title: clip(title, 120),
            body: clip(body, 8000),
            tags,
            origin,
            note: None,
        });
        // Keep the inbox bounded: drop the oldest accepted ones first.
        while d.items.len() > 2000 {
            match d.items.iter().position(|l| l.status != "proposed") {
                Some(i) => d.items.remove(i),
                None => d.items.remove(0),
            };
        }
        self.save(&d);
        id
    }

    pub fn edit<R>(&self, id: u64, f: impl FnOnce(&mut Learning) -> R) -> Option<R> {
        let mut d = self.data.lock().unwrap();
        let r = d.items.iter_mut().find(|l| l.id == id).map(f);
        if r.is_some() {
            self.save(&d);
        }
        r
    }

    pub fn remove(&self, id: u64) -> bool {
        let mut d = self.data.lock().unwrap();
        let n = d.items.len();
        d.items.retain(|l| l.id != id);
        let removed = d.items.len() != n;
        if removed {
            self.save(&d);
        }
        removed
    }
}

fn origin_of(app: &App, pid: u64, by: &str) -> Origin {
    match app.procs.get(pid) {
        Some(p) => {
            let v = p.view();
            Origin {
                proc: Some(pid),
                session: v.session,
                harness: v.harness,
                target: v.cwd,
                runner: v.runner,
                profile: p.profile().map(str::to_string),
                by: by.into(),
            }
        }
        None => Origin {
            proc: Some(pid),
            by: by.into(),
            ..Default::default()
        },
    }
}

/// Move what reflecting agents answered into the inbox.
pub fn harvest(app: &App) {
    for v in app.procs.list() {
        if v.reflection != "received" {
            continue;
        }
        let Some(p) = app.procs.get(v.id) else {
            continue;
        };
        let Some(text) = p.take_reflection() else {
            continue;
        };
        let items = parse_reflection(&text);
        let n = items.len();
        for (title, body, tags) in items {
            app.learnings
                .add(&title, &body, tags, origin_of(app, v.id, "agent"));
        }
        if n > 0 {
            p.note_system(format!(
                "proposed {} learning(s); review them under Knowledge > Learnings",
                n
            ));
        } else {
            p.note_system("had nothing worth keeping as a learning".into());
        }
    }
}

// ---- vault helpers ----------------------------------------------------------------

#[derive(Deserialize)]
pub struct VaultQ {
    h: Option<String>,
    #[serde(default)]
    i: usize,
}

fn vault_path(app: &App, h: Option<&str>, i: usize) -> ApiResult<PathBuf> {
    let h = app.harness(h)?;
    h.memory_paths()
        .get(i)
        .cloned()
        .ok_or_else(|| not_found("no such memory vault"))
}

fn open_vault(p: &Path) -> ApiResult<Vault> {
    if !p.is_dir() {
        return Err(not_found("this vault does not exist yet; create it first"));
    }
    Vault::open(p).map_err(internal)
}

fn writable(app: &App) -> ApiResult<()> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    Ok(())
}

// ---- handlers: vaults and notes ---------------------------------------------------

/// The harness's vaults, with their size and health.
pub async fn vaults(
    State(app): State<Arc<App>>,
    Query(q): Query<VaultQ>,
) -> ApiResult<Json<Value>> {
    let h = app.harness(q.h.as_deref())?;
    let list: Vec<Value> = h
        .memory_paths()
        .iter()
        .enumerate()
        .map(|(i, p)| match Vault::open(p) {
            Ok(v) if p.is_dir() => {
                let g = Graph::build(&v);
                let orphans = (0..v.notes.len()).filter(|&j| g.degree(j) == 0).count();
                json!({ "index": i, "path": p.display().to_string(), "exists": true, "notes": v.notes.len(),
                        "links": g.edge_count(), "dangling": g.dangling.len(), "orphans": orphans })
            }
            _ => json!({ "index": i, "path": p.display().to_string(), "exists": false, "notes": 0, "links": 0,
                         "dangling": 0, "orphans": 0 }),
        })
        .collect();
    Ok(Json(
        json!({ "vaults": list, "learnings": app.learnings.proposed() }),
    ))
}

/// Create a vault that the harness names but that does not exist yet.
pub async fn vault_init(
    State(app): State<Arc<App>>,
    Json(q): Json<VaultQ>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let p = vault_path(&app, q.h.as_deref(), q.i)?;
    let created = vault::init(&p).map_err(internal)?;
    Ok(Json(
        json!({ "created": created, "path": p.display().to_string() }),
    ))
}

#[derive(Deserialize)]
pub struct NotesQ {
    h: Option<String>,
    #[serde(default)]
    i: usize,
    q: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    tag: Option<String>,
}

/// Notes in a vault: ranked by a search, else the newest first.
pub async fn notes(State(app): State<Arc<App>>, Query(q): Query<NotesQ>) -> ApiResult<Json<Value>> {
    let p = vault_path(&app, q.h.as_deref(), q.i)?;
    let v = open_vault(&p)?;
    let g = Graph::build(&v);
    let keep = |n: &crate::mem::note::Note| {
        q.kind
            .as_deref()
            .map(|k| n.kind.as_deref() == Some(k))
            .unwrap_or(true)
            && q.tag
                .as_deref()
                .map(|t| n.tags.iter().any(|x| x.eq_ignore_ascii_case(t)))
                .unwrap_or(true)
    };
    let query = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let out: Vec<Value> = match query {
        Some(text) => {
            let idx = crate::mem::search::Index::build(&v.notes);
            idx.search(&v.notes, text, keep)
                .into_iter()
                .take(200)
                .map(|(i, score)| {
                    let mut x = mem::note_summary(&v, &g, i);
                    x["score"] = json!((score * 1000.0).round() / 1000.0);
                    x["snippet"] = json!(crate::mem::search::snippet(&v.notes[i].body, text, 200));
                    x
                })
                .collect()
        }
        None => {
            let mut idx: Vec<usize> = (0..v.notes.len()).filter(|&i| keep(&v.notes[i])).collect();
            idx.sort_by(|a, b| {
                let (x, y) = (&v.notes[*a], &v.notes[*b]);
                y.updated.cmp(&x.updated).then(x.title.cmp(&y.title))
            });
            idx.into_iter()
                .take(500)
                .map(|i| mem::note_summary(&v, &g, i))
                .collect()
        }
    };
    let mut kinds: Vec<String> = v.notes.iter().filter_map(|n| n.kind.clone()).collect();
    kinds.sort();
    kinds.dedup();
    Ok(Json(
        json!({ "notes": out, "total": v.notes.len(), "types": kinds }),
    ))
}

#[derive(Deserialize)]
pub struct NoteQ {
    h: Option<String>,
    #[serde(default)]
    i: usize,
    id: String,
}

/// One note: its parsed fields, relations, and the file as text to edit.
pub async fn note_get(
    State(app): State<Arc<App>>,
    Query(q): Query<NoteQ>,
) -> ApiResult<Json<Value>> {
    let p = vault_path(&app, q.h.as_deref(), q.i)?;
    let v = open_vault(&p)?;
    let g = Graph::build(&v);
    let i = v.require(&q.id).map_err(not_found)?;
    let text = std::fs::read_to_string(&v.notes[i].path).map_err(|e| internal(e.to_string()))?;
    let mut d = mem::note_detail(&v, &g, i);
    d["hash"] = json!(fingerprint(&text));
    d["text"] = json!(text);
    Ok(Json(d))
}

#[derive(Deserialize)]
pub struct NotePut {
    h: Option<String>,
    #[serde(default)]
    i: usize,
    id: String,
    text: String,
    hash: String,
}

/// Replace a note's file, unless it changed since it was loaded.
pub async fn note_put(
    State(app): State<Arc<App>>,
    Json(r): Json<NotePut>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    if r.text.trim().is_empty() {
        return Err(bad("a note cannot be empty; delete it instead"));
    }
    if r.text.len() > 1_000_000 {
        return Err(bad("note too large"));
    }
    let p = vault_path(&app, r.h.as_deref(), r.i)?;
    let _lock = vault::Lock::acquire(&p).map_err(internal)?;
    let v = open_vault(&p)?;
    let i = v.require(&r.id).map_err(not_found)?;
    let path = &v.notes[i].path;
    let now_text = std::fs::read_to_string(path).map_err(|e| internal(e.to_string()))?;
    if fingerprint(&now_text) != r.hash {
        return Err(crate::deck::web::conflict(
            "the note changed since you opened it; reload and edit again",
        ));
    }
    vault::write_atomic(path, &r.text).map_err(internal)?;
    Ok(Json(
        json!({ "id": v.notes[i].id, "hash": fingerprint(&r.text) }),
    ))
}

#[derive(Deserialize)]
pub struct NoteNew {
    h: Option<String>,
    #[serde(default)]
    i: usize,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    links: Vec<String>,
    source: Option<String>,
}

pub async fn note_create(
    State(app): State<Arc<App>>,
    Json(r): Json<NoteNew>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let p = vault_path(&app, r.h.as_deref(), r.i)?;
    open_vault(&p)?;
    let w = mem::add_note(
        &p,
        &mem::NewNote {
            title: r.title,
            body: r.body,
            kind: r.kind.filter(|k| !k.trim().is_empty()),
            tags: r.tags,
            links: r
                .links
                .into_iter()
                .filter(|l| !l.trim().is_empty())
                .collect(),
            source: r.source.filter(|s| !s.trim().is_empty()),
            ..Default::default()
        },
    )
    .map_err(bad)?;
    Ok(Json(json!(w)))
}

pub async fn note_delete(
    State(app): State<Arc<App>>,
    Query(q): Query<NoteQ>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let p = vault_path(&app, q.h.as_deref(), q.i)?;
    open_vault(&p)?;
    let (id, back) = mem::remove_note(&p, &q.id).map_err(not_found)?;
    Ok(Json(json!({ "removed": id, "now_unresolved_in": back })))
}

#[derive(Deserialize)]
pub struct LinkReq {
    h: Option<String>,
    #[serde(default)]
    i: usize,
    from: String,
    to: String,
    #[serde(default = "related")]
    rel: String,
}

fn related() -> String {
    "related".into()
}

pub async fn note_link(
    State(app): State<Arc<App>>,
    Json(r): Json<LinkReq>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let p = vault_path(&app, r.h.as_deref(), r.i)?;
    open_vault(&p)?;
    let changed = mem::link_notes(&p, &r.from, &r.to, &r.rel).map_err(bad)?;
    Ok(Json(json!({ "changed": changed })))
}

#[derive(Deserialize)]
pub struct RecallQ {
    h: Option<String>,
    q: String,
}

/// What an agent launched with this prompt would be given.
pub async fn recall_preview(
    State(app): State<Arc<App>>,
    Query(q): Query<RecallQ>,
) -> ApiResult<Json<Value>> {
    let h = app.harness(q.h.as_deref())?;
    let (prompt, notes) = brief(&h, &q.q);
    let given = prompt
        .strip_prefix(q.q.trim_end())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(Json(json!({ "notes": notes, "text": given })))
}

/// Dangling links and orphans in a vault.
pub async fn health(
    State(app): State<Arc<App>>,
    Query(q): Query<VaultQ>,
) -> ApiResult<Json<Value>> {
    let p = vault_path(&app, q.h.as_deref(), q.i)?;
    let v = open_vault(&p)?;
    let g = Graph::build(&v);
    let dangling: Vec<Value> = g
        .dangling
        .iter()
        .map(|d| json!({ "from": v.notes[d.from].id, "title": v.notes[d.from].title, "rel": d.rel, "target": d.target }))
        .collect();
    let orphans: Vec<Value> = (0..v.notes.len())
        .filter(|&j| g.degree(j) == 0)
        .map(|j| json!({ "id": v.notes[j].id, "title": v.notes[j].title }))
        .collect();
    Ok(Json(json!({ "dangling": dangling, "orphans": orphans })))
}

// ---- handlers: learnings ---------------------------------------------------------

pub async fn learnings_list(State(app): State<Arc<App>>) -> Json<Value> {
    Json(json!(app.learnings.list()))
}

#[derive(Deserialize)]
pub struct LearningNew {
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    tags: Vec<String>,
    /// The process it is about.
    proc: Option<u64>,
}

fn clean_tags(tags: Vec<String>) -> Vec<String> {
    let mut t: Vec<String> = tags
        .into_iter()
        .map(|t| t.trim().trim_start_matches('#').to_lowercase())
        .filter(|t| !t.is_empty())
        .map(|t| clip(&t, 40))
        .collect();
    t.dedup();
    t.truncate(12);
    t
}

pub async fn learning_create(
    State(app): State<Arc<App>>,
    Json(r): Json<LearningNew>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    if r.title.trim().is_empty() {
        return Err(bad("a learning needs a title"));
    }
    let origin = match r.proc {
        Some(pid) => origin_of(&app, pid, "person"),
        None => Origin {
            by: "person".into(),
            ..Default::default()
        },
    };
    let id = app
        .learnings
        .add(&r.title, &r.body, clean_tags(r.tags), origin);
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct LearningEdit {
    title: Option<String>,
    body: Option<String>,
    tags: Option<Vec<String>>,
}

pub async fn learning_update(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Json(r): Json<LearningEdit>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    app.learnings
        .edit(id, |l| {
            if let Some(t) = r.title.filter(|t| !t.trim().is_empty()) {
                l.title = clip(&t, 120);
            }
            if let Some(b) = r.body {
                l.body = clip(&b, 8000);
            }
            if let Some(t) = r.tags {
                l.tags = clean_tags(t);
            }
        })
        .ok_or_else(|| not_found("no such learning"))?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn learning_delete(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    if !app.learnings.remove(id) {
        return Err(not_found("no such learning"));
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct AcceptReq {
    h: Option<String>,
    #[serde(default)]
    i: usize,
    /// Notes to link it to.
    #[serde(default)]
    links: Vec<String>,
}

/// Write a learning into a vault as a `learning` note (merged into a note of
/// the same title if there is one), with where it came from.
pub async fn learning_accept(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Json(r): Json<AcceptReq>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let l = app
        .learnings
        .list()
        .into_iter()
        .find(|l| l.id == id)
        .ok_or_else(|| not_found("no such learning"))?;
    if l.status != "proposed" {
        return Err(bad("this learning was accepted already"));
    }
    let p = vault_path(&app, r.h.as_deref(), r.i)?;
    if !p.is_dir() {
        vault::init(&p).map_err(internal)?;
    }
    let o = &l.origin;
    let mut source = match o.proc {
        Some(pid) => format!("indra process #{}", pid),
        None => "indra console".to_string(),
    };
    if !o.runner.is_empty() {
        source.push_str(&format!(", {}", o.runner));
    }
    if let Some(s) = &o.session {
        source.push_str(&format!(", session {}", s));
    }
    if !o.target.is_empty() {
        source.push_str(&format!(", in {}", o.target));
    }
    let w = mem::add_note(
        &p,
        &mem::NewNote {
            title: l.title.clone(),
            body: l.body.clone(),
            kind: Some("learning".into()),
            tags: l.tags.clone(),
            links: r
                .links
                .into_iter()
                .filter(|x| !x.trim().is_empty())
                .collect(),
            source: Some(source),
            upsert: true,
            ..Default::default()
        },
    )
    .map_err(bad)?;
    let vault_s = p.display().to_string();
    app.learnings.edit(id, |x| {
        x.status = "accepted".into();
        x.note = Some((vault_s.clone(), w.id.clone()));
    });
    Ok(Json(json!({ "note": w, "vault": vault_s })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflections_become_proposals() {
        let t = "Here you go:\n```json\n[{\"title\":\"Tests need the db\",\"body\":\"Run make db first.\",\"tags\":[\"tests\"]},{\"body\":\"no title\"}]\n```";
        let v = parse_reflection(t);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, "Tests need the db");
        assert_eq!(v[0].2, vec!["tests".to_string()]);
        assert!(parse_reflection("[]").is_empty());
        assert!(parse_reflection("  ").is_empty());
        let free = parse_reflection("The build uses pnpm, not npm.");
        assert_eq!(free.len(), 1);
        assert!(free[0].1.contains("pnpm"));
    }

    #[test]
    fn the_inbox_keeps_proposals_first() {
        let dir = std::env::temp_dir().join(format!("indra-learn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Learnings::open(dir.join("learnings.json"));
        let a = s.add("a", "x", vec![], Origin::default());
        let b = s.add("b", "y", vec![], Origin::default());
        s.edit(b, |l| l.status = "accepted".into());
        assert_eq!(s.proposed(), 1);
        let again = Learnings::open(dir.join("learnings.json"));
        let l = again.list();
        assert_eq!((l[0].id, l[1].id), (a, b));
        assert!(again.remove(a) && !again.remove(a));
        let _ = std::fs::remove_dir_all(dir);
    }
}
