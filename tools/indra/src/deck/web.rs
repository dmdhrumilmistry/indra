//! The HTTP API and the embedded UI.
//!
//! Security model (this server can start agents and edit prompts, so it is
//! treated like a remote shell):
//! - it binds to loopback by default;
//! - every `/api` call needs the per-start token, sent as an HttpOnly,
//!   SameSite=Strict cookie (set by opening the printed URL once) or as
//!   `Authorization: Bearer <token>`;
//! - the Host header must name the loopback interface when bound to it,
//!   which defeats DNS rebinding;
//! - state-changing calls must be same-origin (Origin / Sec-Fetch-Site), and
//!   take JSON bodies only;
//! - files are read or written only when discovery found them (prompts) or
//!   when they resolve inside a run directory; runners come from config only;
//! - a strict Content-Security-Policy: no inline script, no third parties.

use crate::deck::confedit;
use crate::deck::config::{self, Harness, Settings};
use crate::deck::discover::{self, Def};
use crate::deck::nodes::{self, NodeInfo, Resume, RunsSpec, LOCAL};
use crate::deck::procs::{Launch, Procs, StartOpts};
use crate::deck::runs;
use crate::deck::workflows::{self, Engine};
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::Mutex;

/// When a remote node was last checked, and what it said.
pub type NodeProbe = (u64, Result<NodeInfo, String>);

pub struct App {
    settings: config::Shared,
    pub token: String,
    pub procs: Arc<Procs>,
    pub engine: Arc<Engine>,
    /// Bound to a loopback address: enforce a loopback Host header.
    pub loopback: bool,
    /// Remote node facts, cached briefly: (checked at, result).
    pub node_cache: Mutex<HashMap<String, NodeProbe>>,
    /// Editing the config file from the console (None: not offered).
    pub config: Option<ConfigAccess>,
    /// Work items, run by agents and synced with trackers.
    pub board: Arc<crate::deck::board::Board>,
    /// The browser terminal (`indra serve --shell`): a shell on this machine.
    pub shell: std::sync::atomic::AtomicBool,
    /// Learnings agents and people proposed, waiting for review.
    pub learnings: Arc<crate::deck::knowledge::Learnings>,
}

/// What the console needs to edit the config file and apply the result.
pub struct ConfigAccess {
    /// How the settings were loaded, to load them again after an edit.
    pub opts: config::LoadOptions,
    /// The file edits are written to (it may not exist yet).
    pub path: std::path::PathBuf,
    /// Why editing is turned off, if it is.
    pub locked: Option<String>,
    /// One save at a time.
    pub saving: Mutex<()>,
}

impl App {
    #[cfg(test)]
    pub fn new(settings: Settings, token: String, loopback: bool) -> Arc<App> {
        App::with_config(settings, token, loopback, None)
    }

    pub fn with_config(
        settings: Settings,
        token: String,
        loopback: bool,
        config: Option<ConfigAccess>,
    ) -> Arc<App> {
        let procs = Procs::new(settings.data_dir.join("logs"));
        apply_link_settings(&procs, &settings);
        let board = Arc::new(crate::deck::board::Board::open(
            settings.data_dir.join("board.json"),
        ));
        let learnings = Arc::new(crate::deck::knowledge::Learnings::open(
            settings.data_dir.join("learnings.json"),
        ));
        let shared = config::Shared::new(settings);
        let engine = Engine::new(procs.clone(), shared.clone());
        Arc::new(App {
            settings: shared,
            token,
            procs,
            engine,
            loopback,
            node_cache: Mutex::new(HashMap::new()),
            config,
            board,
            shell: std::sync::atomic::AtomicBool::new(false),
            learnings,
        })
    }

    /// The settings now. Take one snapshot per request: an edit in the
    /// console replaces them.
    pub fn settings(&self) -> Arc<Settings> {
        self.settings.get()
    }
}

const COOKIE: &str = "indra_token";
const INDEX: &str = include_str!("../../ui/index.html");
const APP_JS: &str = include_str!("../../ui/app.js");
const STYLE: &str = include_str!("../../ui/style.css");
const FAVICON: &str = include_str!("../../ui/favicon.svg");
const TERM_HTML: &str = include_str!("../../ui/term.html");
const TERM_JS: &str = include_str!("../../ui/term.js");
const XTERM_JS: &str = include_str!("../../ui/vendor/xterm.js");
const XTERM_CSS: &str = include_str!("../../ui/vendor/xterm.css");
const XTERM_FIT_JS: &str = include_str!("../../ui/vendor/addon-fit.js");
const XTERM_LICENSE: &str = include_str!("../../ui/vendor/LICENSE-xterm");
/// The terminal page only: xterm.js sets styles with a `<style>` element, so
/// it needs inline styles. Scripts stay `'self'`.
const TERM_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                        connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

// ---- errors ------------------------------------------------------------------

pub struct ApiError(pub StatusCode, pub String);

impl ApiError {
    pub fn message(self) -> String {
        self.1
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

pub(crate) fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
pub(crate) fn not_found(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}
pub(crate) fn forbidden(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, msg.into())
}
pub(crate) fn internal(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg.into())
}
pub(crate) fn conflict(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::CONFLICT, msg.into())
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

// ---- auth --------------------------------------------------------------------

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn presented_token(h: &HeaderMap) -> Option<String> {
    if let Some(v) = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(t) = v.strip_prefix("Bearer ") {
            return Some(t.trim().to_string());
        }
    }
    for c in h.get_all(header::COOKIE) {
        let Ok(c) = c.to_str() else { continue };
        for part in c.split(';') {
            if let Some(t) = part.trim().strip_prefix(&format!("{}=", COOKIE)) {
                return Some(t.to_string());
            }
        }
    }
    None
}

fn host_name(h: &HeaderMap) -> Option<String> {
    let host = h.get(header::HOST)?.to_str().ok()?.trim().to_lowercase();
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().map(|s| format!("[{}]", s));
    }
    host.split(':').next().map(String::from)
}

pub fn is_loopback_host(name: &str) -> bool {
    matches!(name, "localhost" | "127.0.0.1" | "[::1]") || name.ends_with(".localhost")
}

async fn guard(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let h = req.headers();
    if app.loopback && !host_name(h).map(|n| is_loopback_host(&n)).unwrap_or(false) {
        return forbidden("unexpected Host header").into_response();
    }
    let path = req.uri().path();
    if path.starts_with("/api/") {
        let ok = presented_token(h)
            .map(|t| ct_eq(t.as_bytes(), app.token.as_bytes()))
            .unwrap_or(false);
        if !ok {
            return ApiError(
                StatusCode::UNAUTHORIZED,
                "missing or wrong token: open the URL `indra serve` printed when it started".into(),
            )
            .into_response();
        }
        if req.method() != Method::GET && req.method() != Method::HEAD {
            if h.get("sec-fetch-site")
                .and_then(|v| v.to_str().ok())
                .map(|s| s == "cross-site" || s == "same-site")
                .unwrap_or(false)
            {
                return forbidden("cross-site request refused").into_response();
            }
            if let Some(origin) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
                let host = h
                    .get(header::HOST)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if origin != format!("http://{}", host) && origin != format!("https://{}", host) {
                    return forbidden("cross-origin request refused").into_response();
                }
            }
        }
    }
    let mut res = next.run(req).await;
    let hd = res.headers_mut();
    if !hd.contains_key("content-security-policy") {
        hd.insert("content-security-policy", HeaderValue::from_static(CSP));
    }
    hd.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    hd.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    hd.insert("x-frame-options", HeaderValue::from_static("DENY"));
    if !hd.contains_key(header::CACHE_CONTROL) {
        hd.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

// ---- static ------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenQ {
    token: Option<String>,
}

async fn index(State(app): State<Arc<App>>, Query(q): Query<TokenQ>) -> Response {
    if let Some(t) = q.token {
        if ct_eq(t.as_bytes(), app.token.as_bytes()) {
            // Trade the URL token for a cookie and drop it from the address bar.
            let cookie = format!(
                "{}={}; HttpOnly; SameSite=Strict; Path=/",
                COOKIE, app.token
            );
            return (
                StatusCode::SEE_OTHER,
                [
                    (header::SET_COOKIE, cookie),
                    (header::LOCATION, "/".to_string()),
                ],
            )
                .into_response();
        }
    }
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX).into_response()
}

fn asset(ct: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, ct)], body).into_response()
}

// ---- helpers -------------------------------------------------------------------

#[derive(Deserialize)]
struct HQ {
    h: Option<String>,
}

impl App {
    pub(crate) fn harness(&self, name: Option<&str>) -> ApiResult<Harness> {
        let s = self.settings();
        match name.filter(|n| !n.is_empty()) {
            Some(n) => s
                .harnesses
                .iter()
                .find(|h| h.name == n)
                .cloned()
                .ok_or_else(|| not_found(format!("no harness named '{}'", n))),
            None => s
                .harnesses
                .first()
                .cloned()
                .ok_or_else(|| not_found("no harness configured; see `indra init`")),
        }
    }
}

fn def_by_id(h: &Harness, id: &str) -> ApiResult<Def> {
    discover::discover(h)
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| not_found(format!("no definition '{}'", id)))
}

/// FNV-1a: a cheap content fingerprint for optimistic concurrency on saves.
pub fn fingerprint(s: &str) -> String {
    let mut x: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        x ^= *b as u64;
        x = x.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", x)
}

fn harness_json(h: &Harness) -> Value {
    let defs = discover::discover(h);
    let count = |k: discover::Kind| defs.iter().filter(|d| d.kind == k).count();
    let runs = runs::list(h, 500);
    json!({
        "name": h.name, "description": h.description, "root": h.root.display().to_string(), "builtin": h.builtin,
        "targets": h.targets.iter().map(|t| t.display().to_string()).collect::<Vec<_>>(),
        "stages": h.stages, "has_runs": h.runs.is_some(),
        "counts": { "agents": count(discover::Kind::Agent), "skills": count(discover::Kind::Skill),
                    "prompts": count(discover::Kind::Prompt), "runs": runs.len(),
                    "active_runs": runs.iter().filter(|r| r.active).count(),
                    "memory": h.memory_paths().iter().filter(|p| p.is_dir()).count() },
    })
}

// ---- handlers ------------------------------------------------------------------

async fn overview(State(app): State<Arc<App>>) -> Json<Value> {
    let s = &app.settings();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "read_only": s.read_only,
        "shell": app.shell_on(),
        "config": s.config_path.as_ref().map(|p| p.display().to_string()),
        "data_dir": s.data_dir.display().to_string(),
        "harnesses": s.harnesses.iter().map(harness_json).collect::<Vec<_>>(),
        "runners": s.runners.iter().map(|r| json!({
            "name": r.name, "command": r.command, "args": r.args, "format": r.format,
            "available": r.program.is_some(),
            "program": r.program.as_ref().map(|p| p.display().to_string()),
        })).collect::<Vec<_>>(),
        "presets": s.presets,
        "profiles": s.profiles,
        "approvals": app.procs.list().iter().map(|p| p.pending.len()).sum::<usize>(),
        "learnings": app.learnings.proposed(),
        "procs": app.procs.list(),
        "nodes": std::iter::once(LOCAL.to_string()).chain(s.nodes.iter().map(|n| n.name.clone())).collect::<Vec<_>>(),
        "workflows": s.workflows.iter().map(|w| w.name.clone()).collect::<Vec<_>>(),
        "running": app.procs.running(),
        "settings": app.config.as_ref().map(|c| json!({
            "path": c.path.display().to_string(), "editable": c.locked.is_none(), "locked": c.locked,
        })),
    }))
}

async fn defs(State(app): State<Arc<App>>, Query(q): Query<HQ>) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    Ok(Json(json!(discover::discover(h))))
}

#[derive(Deserialize)]
struct DefQ {
    h: Option<String>,
    id: String,
}

async fn def_get(State(app): State<Arc<App>>, Query(q): Query<DefQ>) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    let d = def_by_id(h, &q.id)?;
    let content = discover::read_body(&d).map_err(|e| not_found(e.to_string()))?;
    let defs = discover::discover(h);
    let g = discover::graph(h, &defs);
    let name_of = |id: &str| {
        defs.iter()
            .find(|x| x.id == id)
            .map(|x| x.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    let uses: Vec<Value> = g
        .edges
        .iter()
        .filter(|e| e.source == d.id)
        .map(|e| json!({ "id": e.target, "name": name_of(&e.target), "rel": e.rel }))
        .collect();
    let used_by: Vec<Value> = g
        .edges
        .iter()
        .filter(|e| e.target == d.id)
        .map(|e| json!({ "id": e.source, "name": name_of(&e.source), "rel": e.rel }))
        .collect();
    Ok(Json(json!({
        "def": d, "abs": d.abs.display().to_string(), "content": content,
        "hash": fingerprint(&content), "uses": uses, "used_by": used_by,
    })))
}

#[derive(Deserialize)]
struct SaveReq {
    h: Option<String>,
    id: String,
    content: String,
    hash: String,
}

async fn def_put(State(app): State<Arc<App>>, Json(r): Json<SaveReq>) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    if r.content.len() > 1_000_000 {
        return Err(bad("content is larger than 1 MB"));
    }
    let h = &app.harness(r.h.as_deref())?;
    let d = def_by_id(h, &r.id)?;
    let current = discover::read_body(&d).map_err(|e| not_found(e.to_string()))?;
    if fingerprint(&current) != r.hash {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "the file changed on disk since you opened it; reload to see the new version".into(),
        ));
    }
    crate::mem::vault::write_atomic(&d.abs, &r.content)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(
        json!({ "saved": d.path, "hash": fingerprint(&r.content) }),
    ))
}

#[derive(Deserialize)]
struct NewDef {
    h: Option<String>,
    /// agent | skill | prompt
    kind: String,
    name: String,
    content: String,
}

/// Where a new definition of `kind` named `name` goes: the harness's first
/// pattern for that kind with exactly one `*`, e.g. `agents/*.md` or
/// `skills/*/SKILL.md`.
fn new_def_path(h: &Harness, kind: &str, name: &str) -> ApiResult<std::path::PathBuf> {
    let globs = match kind {
        "agent" => &h.agents,
        "skill" => &h.skills,
        "prompt" => &h.prompts,
        _ => return Err(bad("kind must be agent, skill or prompt")),
    };
    let pat = globs
        .iter()
        .find(|g| g.matches('*').count() == 1 && !g.contains('?') && !g.contains('['))
        .ok_or_else(|| {
            bad(format!(
                "harness '{}' has no simple pattern for {}s (like {}/*.md) to put a new one in",
                h.name, kind, kind
            ))
        })?;
    Ok(config::expand(&h.root, &pat.replacen('*', name, 1)))
}

/// Create an agent, skill or prompt file (for example an imported one).
async fn def_create(State(app): State<Arc<App>>, Json(r): Json<NewDef>) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let name = r.name.trim();
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        || name.starts_with('.')
    {
        return Err(bad(
            "name: letters, digits, '-', '_' and '.', up to 64, not starting with '.'",
        ));
    }
    if r.content.trim().is_empty() || r.content.len() > 1_000_000 {
        return Err(bad("content must be between 1 byte and 1 MB"));
    }
    let h = &app.harness(r.h.as_deref())?;
    let path = new_def_path(h, &r.kind, name)?;
    if path.exists() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!("{} exists already; open it to edit", path.display()),
        ));
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| internal(e.to_string()))?;
    }
    crate::mem::vault::write_atomic(&path, &r.content).map_err(internal)?;
    let found = discover::discover(h).into_iter().find(|d| d.abs == path);
    Ok(Json(json!({
        "id": found.map(|d| d.id),
        "path": path.display().to_string(),
    })))
}

#[derive(Deserialize)]
struct DefQ2 {
    h: Option<String>,
    id: String,
}

/// Delete an agent, skill or prompt file. A skill's folder goes too when
/// nothing else is left in it.
async fn def_delete(State(app): State<Arc<App>>, Query(q): Query<DefQ2>) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let h = &app.harness(q.h.as_deref())?;
    let d = def_by_id(h, &q.id)?;
    std::fs::remove_file(&d.abs)
        .map_err(|e| internal(format!("cannot delete {}: {}", d.abs.display(), e)))?;
    if d.kind == discover::Kind::Skill {
        if let Some(dir) = d.abs.parent() {
            let _ = std::fs::remove_dir(dir); // only when empty
        }
    }
    Ok(Json(json!({ "deleted": d.path })))
}

async fn graph(State(app): State<Arc<App>>, Query(q): Query<HQ>) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    let defs = discover::discover(h);
    Ok(Json(json!(discover::graph(h, &defs))))
}

async fn memory(State(app): State<Arc<App>>, Query(q): Query<HQ>) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    let list: Vec<Value> = h
        .memory_paths()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (notes, links) = match crate::mem::vault::Vault::open(p) {
                Ok(v) if p.is_dir() => {
                    let g = crate::mem::graph::Graph::build(&v);
                    (v.notes.len(), g.edge_count())
                }
                _ => (0, 0),
            };
            json!({ "index": i, "path": p.display().to_string(), "exists": p.is_dir(), "notes": notes, "links": links })
        })
        .collect();
    Ok(Json(json!(list)))
}

#[derive(Deserialize)]
struct MemQ {
    h: Option<String>,
    i: usize,
}

async fn memory_graph(
    State(app): State<Arc<App>>,
    Query(q): Query<MemQ>,
) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    let p = h
        .memory_paths()
        .get(q.i)
        .cloned()
        .ok_or_else(|| not_found("no such memory vault"))?;
    let v = crate::mem::vault::Vault::open(&p).map_err(not_found)?;
    let g = crate::mem::graph::Graph::build(&v);
    Ok(Json(json!(crate::mem::export::data(&v, &g, true))))
}

async fn runs_list(State(app): State<Arc<App>>, Query(q): Query<HQ>) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    Ok(Json(json!(runs::list(h, 200))))
}

#[derive(Deserialize)]
struct RunQ {
    h: Option<String>,
    key: String,
    rel: Option<String>,
    max: Option<u64>,
}

async fn run_get(State(app): State<Arc<App>>, Query(q): Query<RunQ>) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    let dir = runs::find(h, &q.key).ok_or_else(|| not_found("no such run"))?;
    Ok(Json(json!(runs::summarize(h, &q.key, &dir, true))))
}

async fn run_file(State(app): State<Arc<App>>, Query(q): Query<RunQ>) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    let dir = runs::find(h, &q.key).ok_or_else(|| not_found("no such run"))?;
    let rel = q.rel.ok_or_else(|| bad("rel is required"))?;
    let p = runs::confined(&dir, &rel)
        .ok_or_else(|| forbidden("that path is not a file inside this run"))?;
    let max = q.max.unwrap_or(256 * 1024).clamp(1024, 4 * 1024 * 1024);
    let (text, truncated) = runs::tail(&p, max).map_err(|e| not_found(e.to_string()))?;
    Ok(Json(
        json!({ "rel": rel, "text": text, "truncated": truncated, "modified": discover::mtime(&p) }),
    ))
}

#[derive(Deserialize)]
struct LaunchReq {
    h: Option<String>,
    /// "local" (default) or a configured node.
    node: Option<String>,
    /// Runner; may be left out when the profile names one.
    #[serde(default)]
    runner: String,
    prompt: String,
    /// Index into that node's targets (see /api/nodes).
    target: usize,
    /// Profile to launch with.
    profile: Option<String>,
    /// Per-launch changes on top of the profile (model, permission mode, ...).
    #[serde(default)]
    opts: nodes::AgentOpts,
    /// Give it the relevant memory notes (default: the profile's choice).
    memory: Option<bool>,
    /// Ask it for learnings before it finishes (default: the profile's choice).
    reflect: Option<bool>,
}

/// A profile and per-launch overrides, resolved into a runner and options.
pub(crate) fn resolve_agent(
    settings: &Settings,
    profile: Option<&str>,
    runner: &str,
    overrides: &nodes::AgentOpts,
) -> ApiResult<(String, nodes::AgentOpts)> {
    let (prof_runner, base) = match profile.filter(|p| !p.is_empty()) {
        Some(name) => {
            let p = settings
                .profile(name)
                .ok_or_else(|| bad(format!("no profile named '{}'", name)))?;
            (p.runner.clone(), p.opts())
        }
        None => (None, nodes::AgentOpts::default()),
    };
    let runner = if !runner.is_empty() {
        runner.to_string()
    } else {
        prof_runner.unwrap_or_else(|| "claude".into())
    };
    let opts = base.with(overrides);
    opts.validate().map_err(bad)?;
    Ok((runner, opts))
}

/// The directories agents may run in on a node: the harness targets for
/// this machine, a node's configured `targets` for a remote one.
fn node_targets(app: &App, h: &Harness, node: &str) -> ApiResult<Vec<String>> {
    if node == LOCAL {
        return Ok(h.targets.iter().map(|t| t.display().to_string()).collect());
    }
    let n = app
        .settings()
        .node(node)
        .map(|n| n.targets.clone())
        .ok_or_else(|| bad(format!("unknown node '{}'", node)))?;
    // A node that dialled in reports its own targets when the config lists none.
    if n.is_empty() {
        if let Some(l) = app.procs.links().get(node) {
            return Ok(l.targets());
        }
    }
    Ok(n)
}

/// WebRTC on or off, and its ICE servers, for nodes that dial in.
fn apply_link_settings(procs: &Procs, s: &Settings) {
    let l = procs.links();
    l.webrtc.store(
        s.webrtc && cfg!(feature = "webrtc"),
        std::sync::atomic::Ordering::SeqCst,
    );
    *l.ice_servers.lock().unwrap() = s.ice_servers.clone();
}

fn check_prompt(p: &str) -> ApiResult<&str> {
    let p = p.trim();
    if p.is_empty() || p.len() > 20_000 {
        return Err(bad("the prompt must be 1 to 20000 characters"));
    }
    Ok(p)
}

async fn proc_start(
    State(app): State<Arc<App>>,
    Json(r): Json<LaunchReq>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let prompt = check_prompt(&r.prompt)?;
    let h = &app.harness(r.h.as_deref())?;
    let node = r.node.as_deref().unwrap_or(LOCAL);
    let targets = node_targets(&app, h, node)?;
    let target = targets.get(r.target).ok_or_else(|| bad("unknown target"))?;
    let settings = app.settings();
    let (runner_name, agent) = resolve_agent(&settings, r.profile.as_deref(), &r.runner, &r.opts)?;
    let profile = r.profile.clone().filter(|p| !p.is_empty());
    let (full_prompt, memory, reflect) = crate::deck::knowledge::prepare(
        &settings,
        &h.name,
        prompt,
        profile.as_deref(),
        r.memory,
        r.reflect,
    );
    let prompt = full_prompt.as_str();
    let start = StartOpts {
        agent,
        profile,
        memory,
        reflect,
        ..StartOpts::default()
    };
    let id = if node == LOCAL {
        let runner = settings
            .runners
            .iter()
            .find(|x| x.name == runner_name)
            .ok_or_else(|| bad(format!("unknown runner '{}'", runner_name)))?;
        let target = std::path::PathBuf::from(target);
        app.procs.start_with(
            Launch::Local {
                runner,
                harness: &h.name,
                prompt,
                target: &target,
            },
            start,
        )
    } else {
        let n = settings.node(node).ok_or_else(|| bad("unknown node"))?;
        nodes::validate_name(&runner_name).map_err(bad)?;
        app.procs.start_with(
            Launch::Remote {
                node: n,
                runner: &runner_name,
                harness: &h.name,
                prompt,
                target,
            },
            start,
        )
    }
    .map_err(bad)?;
    Ok(Json(json!({ "id": id })))
}

async fn procs_list(State(app): State<Arc<App>>) -> Json<Value> {
    Json(json!(app.procs.list()))
}

async fn proc_get(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    let p = app
        .procs
        .get(id)
        .ok_or_else(|| not_found("no such process"))?;
    Ok(Json(json!(p.view())))
}

async fn proc_stop(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let p = app
        .procs
        .get(id)
        .ok_or_else(|| not_found("no such process"))?;
    Ok(Json(json!({ "stopping": p.request_stop() })))
}

#[derive(Deserialize)]
struct AfterQ {
    after: Option<u64>,
}

async fn proc_events(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Query(q): Query<AfterQ>,
) -> ApiResult<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    let p = app
        .procs
        .get(id)
        .ok_or_else(|| not_found("no such process"))?;
    let after = q.after.unwrap_or(0);
    let (backlog, rx) = p.subscribe(after);
    let last = backlog.last().map(|e| e.seq).unwrap_or(after);
    let to_sse = |e: &crate::deck::procs::Event| {
        SseEvent::default().data(serde_json::to_string(e).unwrap_or_default())
    };
    let first = stream::iter(backlog.iter().map(to_sse).map(Ok).collect::<Vec<_>>());
    let live = stream::unfold((rx, last), move |(mut rx, last)| async move {
        loop {
            match rx.recv().await {
                Ok(e) if e.seq <= last => continue,
                Ok(e) => {
                    let seq = e.seq;
                    return Some((Ok(to_sse(&e)), (rx, seq)));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Ok(Sse::new(first.chain(live)).keep_alive(KeepAlive::default()))
}

// ---- pivot -----------------------------------------------------------------------

#[derive(Deserialize)]
struct PivotReq {
    prompt: String,
    /// "continue" (default): resume the agent's own session with the new
    /// direction. "fresh": a new agent, given the old one's outcome as context.
    mode: Option<String>,
    /// Leave the current agent running; a continue pivot then branches the session.
    #[serde(default)]
    keep_running: bool,
}

/// Give an agent a new direction. It runs on the same node, in the same
/// target, with the same runner: an agent session lives on the machine where
/// it ran.
async fn proc_pivot(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Json(r): Json<PivotReq>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let prompt = check_prompt(&r.prompt)?;
    let p = app
        .procs
        .get(id)
        .ok_or_else(|| not_found("no such process"))?;
    let cont = match r.mode.as_deref().unwrap_or("continue") {
        "continue" => true,
        "fresh" => false,
        _ => return Err(bad("mode must be 'continue' or 'fresh'")),
    };
    let view = p.view();
    // Check everything that can fail before touching the running agent.
    if cont && !p.can_resume() {
        return Err(bad(format!(
            "runner '{}' cannot continue a session; pivot with a fresh start",
            p.runner_name()
        )));
    }
    if cont && view.session.is_none() {
        return Err(bad(
            "this agent has not reported a session to continue (only Claude Code does); pivot with a fresh start",
        ));
    }
    if p.node_name() == LOCAL {
        if !app
            .settings()
            .runners
            .iter()
            .any(|x| x.name == p.runner_name())
        {
            return Err(bad("its runner is no longer configured"));
        }
    } else if app.settings().node(p.node_name()).is_none() {
        return Err(bad("its node is no longer configured"));
    }
    let was_running = p.is_running();
    let mut fork = cont && was_running && r.keep_running;
    if was_running && !r.keep_running {
        p.request_stop();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while p.is_running() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // Never let two agents write to one session.
        fork |= cont && p.is_running();
    }
    let new_prompt = if cont {
        prompt.to_string()
    } else {
        format!(
            "{}\n\nContext: this continues the work of an earlier agent (process #{}, which {}). It was asked:\n{}\n\nWhat it reported:\n{}",
            prompt,
            id,
            view.status,
            crate::mem::search::truncate(&view.prompt, 2000),
            p.outcome()
        )
    };
    let opts = StartOpts {
        resume: if cont {
            view.session.clone().map(|session| Resume { session, fork })
        } else {
            None
        },
        parent: Some(id),
        // The same model, mode and environment as the agent it continues.
        agent: p.opts().clone(),
        profile: p.profile().map(String::from),
        // A continuation already has the memory it was given; it reflects
        // at the end if the original would have.
        memory: vec![],
        reflect: view.reflect,
    };
    let harness = p.harness_name().to_string();
    let new = if p.node_name() == LOCAL {
        let settings = app.settings();
        let runner = settings
            .runners
            .iter()
            .find(|x| x.name == p.runner_name())
            .ok_or_else(|| bad("its runner is no longer configured"))?;
        let target = std::path::PathBuf::from(p.target());
        app.procs.start_with(
            Launch::Local {
                runner,
                harness: &harness,
                prompt: &new_prompt,
                target: &target,
            },
            opts,
        )
    } else {
        let settings = app.settings();
        let node = settings
            .node(p.node_name())
            .ok_or_else(|| bad("its node is no longer configured"))?;
        app.procs.start_with(
            Launch::Remote {
                node,
                runner: p.runner_name(),
                harness: &harness,
                prompt: &new_prompt,
                target: p.target(),
            },
            opts,
        )
    }
    .map_err(bad)?;
    Ok(Json(json!({
        "id": new,
        "mode": if cont { "continue" } else { "fresh" },
        "forked": fork,
        "stopped_previous": was_running && !p.is_running(),
    })))
}

// ---- nodes ---------------------------------------------------------------------

#[derive(Deserialize)]
struct NodesQ {
    h: Option<String>,
    refresh: Option<u8>,
}

const NODE_CACHE_SECS: u64 = 30;

async fn nodes_list(
    State(app): State<Arc<App>>,
    Query(q): Query<NodesQ>,
) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    let now = runs::now();
    // Probe stale remote nodes in parallel.
    let mut set = tokio::task::JoinSet::new();
    for n in app
        .settings()
        .nodes
        .iter()
        .filter(|n| !n.ssh.is_empty() && !app.procs.links().connected(&n.name))
        .cloned()
    {
        let fresh = q.refresh.is_none()
            && app
                .node_cache
                .lock()
                .unwrap()
                .get(&n.name)
                .map(|(t, _)| now.saturating_sub(*t) < NODE_CACHE_SECS)
                .unwrap_or(false);
        if !fresh {
            set.spawn(async move { (n.name.clone(), nodes::remote_info(&n).await) });
        }
    }
    while let Some(Ok((name, res))) = set.join_next().await {
        app.node_cache
            .lock()
            .unwrap()
            .insert(name, (runs::now(), res));
    }
    let local = json!({
        "name": LOCAL, "kind": "local", "description": "This machine",
        "max_parallel": app.settings().local_max_parallel, "running": app.procs.running_on(LOCAL),
        "targets": node_targets(&app, h, LOCAL)?, "info": nodes::local_info(&app.settings()), "error": null, "checked": now,
    });
    let cache = app.node_cache.lock().unwrap();
    let mut out = vec![local];
    for n in &app.settings().nodes {
        let link = app.procs.links().get(&n.name);
        let linked = link.as_ref().map(|l| l.connected()).unwrap_or(false);
        let (checked, info, error) = if linked {
            let l = link.as_ref().unwrap();
            (now, l.info().unwrap_or(Value::Null), Value::Null)
        } else if n.ssh.is_empty() {
            (
                now,
                Value::Null,
                json!("not connected: run `indra node connect` on it"),
            )
        } else {
            match cache.get(&n.name) {
                Some((t, Ok(i))) => (*t, json!(i), Value::Null),
                Some((t, Err(e))) => (*t, Value::Null, json!(e)),
                None => (0, Value::Null, json!("not checked yet")),
            }
        };
        let targets = if n.targets.is_empty() {
            link.as_ref().map(|l| l.targets()).unwrap_or_default()
        } else {
            n.targets.clone()
        };
        out.push(json!({
            "name": n.name, "kind": if linked { "link" } else { "ssh" }, "ssh": n.ssh, "description": n.description,
            "transport": if linked { link.as_ref().unwrap().view()["path"].clone() } else if n.ssh.is_empty() { json!("none") } else { json!("ssh") },
            "link": link.as_ref().map(|l| l.view()),
            "max_parallel": n.max_parallel, "running": app.procs.running_on(&n.name),
            "targets": targets, "info": info, "error": error, "checked": checked,
        }));
    }
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
struct NodeRunsQ {
    h: Option<String>,
    node: String,
}

async fn node_runs(
    State(app): State<Arc<App>>,
    Query(q): Query<NodeRunsQ>,
) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    if q.node == LOCAL {
        return Ok(Json(json!(runs::list(h, 100))));
    }
    let settings = app.settings();
    let n = settings
        .node(&q.node)
        .ok_or_else(|| not_found("unknown node"))?;
    let Some(spec) = RunsSpec::for_node(h, &n.targets) else {
        return Ok(Json(json!([])));
    };
    let v = nodes::remote_runs(n, &spec)
        .await
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(v))
}

// ---- workflows -------------------------------------------------------------------

async fn workflows_list(State(app): State<Arc<App>>) -> Json<Value> {
    Json(json!({ "defs": app.settings().workflows, "runs": app.engine.list() }))
}

#[derive(Deserialize)]
struct RunWfReq {
    name: String,
}

async fn workflow_run(
    State(app): State<Arc<App>>,
    Json(r): Json<RunWfReq>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let id = app.engine.start_named(&r.name).map_err(bad)?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
struct FanTarget {
    node: String,
    target: usize,
}

#[derive(Deserialize)]
struct FanOutReq {
    h: Option<String>,
    runner: String,
    prompt: String,
    targets: Vec<FanTarget>,
    profile: Option<String>,
}

async fn workflow_fanout(
    State(app): State<Arc<App>>,
    Json(r): Json<FanOutReq>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let prompt = check_prompt(&r.prompt)?;
    nodes::validate_name(&r.runner).map_err(bad)?;
    if r.targets.is_empty() || r.targets.len() > 100 {
        return Err(bad("pick 1 to 100 targets"));
    }
    let h = &app.harness(r.h.as_deref())?;
    let mut pairs = Vec::new();
    for t in &r.targets {
        let list = node_targets(&app, h, &t.node)?;
        let path = list
            .get(t.target)
            .ok_or_else(|| bad(format!("unknown target on node '{}'", t.node)))?;
        pairs.push((t.node.clone(), path.clone()));
    }
    if let Some(p) = r.profile.as_deref().filter(|p| !p.is_empty()) {
        app.settings()
            .profile(p)
            .ok_or_else(|| bad(format!("no profile named '{}'", p)))?;
    }
    let mut wf = workflows::fan_out(&r.runner, prompt, &pairs);
    for t in &mut wf.tasks {
        t.profile = r.profile.clone().filter(|p| !p.is_empty());
    }
    wf.harness = Some(h.name.clone());
    let id = app.engine.start(wf).map_err(bad)?;
    Ok(Json(json!({ "id": id })))
}

async fn workflow_get(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    app.engine
        .get(id)
        .map(|r| Json(json!(r)))
        .ok_or_else(|| not_found("no such workflow run"))
}

async fn workflow_cancel(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    if !app.engine.cancel(id) {
        return Err(not_found("no such workflow run"));
    }
    Ok(Json(json!({ "cancelling": true })))
}

// ---- driving a running agent ------------------------------------------------------

fn running_proc(app: &App, id: u64) -> ApiResult<Arc<crate::deck::procs::Proc>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    app.procs
        .get(id)
        .ok_or_else(|| not_found("no such process"))
}

#[derive(Deserialize)]
struct PermissionReq {
    request_id: String,
    allow: bool,
    #[serde(default)]
    always: bool,
    message: Option<String>,
}

/// Approve or deny a tool request the agent is waiting on.
async fn proc_permission(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Json(r): Json<PermissionReq>,
) -> ApiResult<Json<Value>> {
    let p = running_proc(&app, id)?;
    p.answer(&r.request_id, r.allow, r.always, r.message.as_deref())
        .map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ControlReq {
    mode: Option<String>,
    model: Option<String>,
    #[serde(default)]
    interrupt: bool,
}

/// Change the permission mode or model of a running agent, or interrupt it.
async fn proc_control(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Json(r): Json<ControlReq>,
) -> ApiResult<Json<Value>> {
    let p = running_proc(&app, id)?;
    let check = nodes::AgentOpts {
        model: r.model.clone(),
        permission_mode: r.mode.clone(),
        ..Default::default()
    };
    check.validate().map_err(bad)?;
    if r.mode.is_none() && r.model.is_none() && !r.interrupt {
        return Err(bad("nothing to change: send mode, model or interrupt"));
    }
    if let Some(m) = &r.mode {
        p.control("set_permission_mode", json!({ "mode": m }))
            .map_err(bad)?;
    }
    if let Some(m) = &r.model {
        p.control("set_model", json!({ "model": m })).map_err(bad)?;
    }
    if r.interrupt {
        p.control("interrupt", json!({})).map_err(bad)?;
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct MessageReq {
    text: String,
}

/// A follow-up message to a running agent, in the same session.
async fn proc_message(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Json(r): Json<MessageReq>,
) -> ApiResult<Json<Value>> {
    let p = running_proc(&app, id)?;
    let text = check_prompt(&r.text)?;
    p.say(text).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

// ---- secrets ---------------------------------------------------------------------

/// Why secrets cannot be changed here, if they cannot: the same rules as
/// settings (read-only, --lock-config, a non-loopback bind).
fn secrets_locked(app: &App) -> Option<String> {
    if app.settings().read_only {
        return Some("indra is running read-only".into());
    }
    app.config.as_ref().and_then(|c| c.locked.clone())
}

/// Names, descriptions, scopes and use; never values.
async fn secrets_list(State(app): State<Arc<App>>) -> ApiResult<Json<Value>> {
    let list = app.procs.vault().list().map_err(internal)?;
    let locked = secrets_locked(&app);
    Ok(Json(
        json!({ "editable": locked.is_none(), "locked": locked, "secrets": list }),
    ))
}

/// Create a secret, or change its value, description or scope (write-only).
async fn secret_put(
    State(app): State<Arc<App>>,
    UrlPath(name): UrlPath<String>,
    Json(c): Json<crate::deck::secrets::Change>,
) -> ApiResult<Json<Value>> {
    if let Some(why) = secrets_locked(&app) {
        return Err(forbidden(why));
    }
    app.procs.vault().set(&name, c).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

async fn secret_delete(
    State(app): State<Arc<App>>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    if let Some(why) = secrets_locked(&app) {
        return Err(forbidden(why));
    }
    if !app.procs.vault().remove(&name).map_err(bad)? {
        return Err(not_found("no such secret"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ---- the board -------------------------------------------------------------------

fn board_writable(app: &App) -> ApiResult<()> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    Ok(())
}

async fn board_get(State(app): State<Arc<App>>) -> Json<Value> {
    crate::deck::board::tick(&app).await;
    let s = app.settings();
    Json(json!({
        "columns": crate::deck::board::COLUMNS,
        "items": app.board.items(),
        "connectors": crate::deck::board::connectors_view(&s),
    }))
}

#[derive(Deserialize)]
struct NewItem {
    title: String,
    #[serde(flatten)]
    rest: crate::deck::board::Patch,
}

async fn board_create(
    State(app): State<Arc<App>>,
    Json(r): Json<NewItem>,
) -> ApiResult<Json<Value>> {
    board_writable(&app)?;
    let id = app.board.create(&r.title, r.rest, None).map_err(bad)?;
    Ok(Json(json!({ "id": id })))
}

async fn board_update(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Json(p): Json<crate::deck::board::Patch>,
) -> ApiResult<Json<Value>> {
    board_writable(&app)?;
    let before = app.board.get(id).ok_or_else(|| not_found("no such item"))?;
    let to = p.column.clone();
    let it = app.board.update(id, p).map_err(bad)?;
    if let Some(c) = to.filter(|c| *c != before.column) {
        crate::deck::board::moved(&app, id, &c).await;
    }
    Ok(Json(json!(it)))
}

async fn board_delete(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    board_writable(&app)?;
    app.board.delete(id).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, Default)]
struct RunItem {
    profile: Option<String>,
    runner: Option<String>,
    node: Option<String>,
    target: Option<String>,
}

async fn board_run(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Json(r): Json<RunItem>,
) -> ApiResult<Json<Value>> {
    board_writable(&app)?;
    let req = crate::deck::board::RunReq {
        profile: r.profile,
        runner: r.runner,
        node: r.node,
        target: r.target,
    };
    let pid = crate::deck::board::run(&app, id, req).await.map_err(bad)?;
    Ok(Json(json!({ "proc": pid })))
}

#[derive(Deserialize)]
struct ImportReq {
    connector: String,
}

async fn board_import(
    State(app): State<Arc<App>>,
    Json(r): Json<ImportReq>,
) -> ApiResult<Json<Value>> {
    board_writable(&app)?;
    let (added, updated) = crate::deck::board::import(&app, &r.connector)
        .await
        .map_err(bad)?;
    Ok(Json(json!({ "added": added, "updated": updated })))
}

// ---- nodes that dial in -------------------------------------------------------------

/// `GET /agent/connect` (WebSocket): a node presents its name and either its
/// link key or a one-time enrolment code. Outside /api: nodes have no console
/// token, and the console token gives no access here.
async fn agent_connect(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let name = h("x-indra-node");
    if nodes::validate_name(&name).is_err() || app.settings().node(&name).is_none() {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        return ApiError(StatusCode::UNAUTHORIZED, "unknown node".into()).into_response();
    }
    let vault = app.procs.vault();
    let key = h("authorization")
        .strip_prefix("Bearer ")
        .map(str::to_string);
    let enrolled = match (key, h("x-indra-enroll")) {
        (Some(k), _) if vault.check_node_key(&name, &k) => None,
        (None, code) if !code.is_empty() && app.procs.links().redeem(&name, &code) => {
            let mut b = [0u8; 32];
            if getrandom::fill(&mut b).is_err() {
                return internal("no OS randomness").into_response();
            }
            let k: String = b.iter().map(|x| format!("{:02x}", x)).collect();
            if let Err(e) = vault.set_node_key(&name, &k) {
                return internal(e).into_response();
            }
            Some(k)
        }
        _ => {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            return ApiError(
                StatusCode::UNAUTHORIZED,
                "wrong key or enrolment code".into(),
            )
            .into_response();
        }
    };
    let links = app.procs.links().clone();
    ws.on_upgrade(move |socket| links.session(name, enrolled, socket))
}

#[derive(Deserialize)]
struct EnrolReq {
    name: String,
    #[serde(default)]
    targets: Vec<String>,
}

/// Make a one-time code for a node to dial in with, adding the node to
/// indra.toml if it is not there yet.
async fn node_enrol(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(r): Json<EnrolReq>,
) -> ApiResult<Json<Value>> {
    let c = config_access(&app)?;
    if let Some(why) = &c.locked {
        return Err(forbidden(why.clone()));
    }
    nodes::validate_name(&r.name).map_err(bad)?;
    if r.name == LOCAL {
        return Err(bad("'local' is this machine"));
    }
    if app.settings().node(&r.name).is_none() {
        let _one = c.saving.lock().unwrap_or_else(|e| e.into_inner());
        let (current, _) = confedit::read(&c.path).map_err(internal)?;
        let cf = confedit::parse(&current).map_err(bad)?;
        let mut list = serde_json::to_value(&cf.node).map_err(|e| internal(e.to_string()))?;
        let mut entry = json!({ "name": r.name });
        if !r.targets.is_empty() {
            entry["targets"] = json!(r.targets);
        }
        list.as_array_mut().unwrap().push(entry);
        let next = confedit::set_section(&current, "node", &list).map_err(bad)?;
        let settings = check_config(&app, c, &next)?;
        confedit::write_atomic(&c.path, &next).map_err(internal)?;
        app.settings.set(settings);
    }
    let code = app.procs.links().enrol_code(&r.name);
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("HUB:PORT");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    Ok(Json(json!({
        "code": code, "expires_in": 900,
        "command": format!("indra node connect {}://{} --name {} --code {}", scheme, host, r.name, code),
    })))
}

// ---- settings (the config file) ------------------------------------------------

fn config_access(app: &App) -> ApiResult<&ConfigAccess> {
    app.config
        .as_ref()
        .ok_or_else(|| not_found("settings cannot be edited from this console"))
}

/// A built-in harness as config, for "customize": a [[harness]] with the
/// same name replaces the built-in one.
fn harness_cfg(h: &Harness) -> config::HarnessCfg {
    config::HarnessCfg {
        name: h.name.clone(),
        description: h.description.clone(),
        root: h.root.display().to_string(),
        agents: h.agents.clone(),
        skills: h.skills.clone(),
        prompts: h.prompts.clone(),
        memory: h.memory.clone(),
        targets: h.targets.iter().map(|t| t.display().to_string()).collect(),
        runs: h.runs.clone(),
        findings: h.findings.clone(),
        group_by: h.group_by.clone(),
        stages: h.stages.clone(),
    }
}

fn config_payload(app: &App) -> ApiResult<Value> {
    let c = config_access(app)?;
    let (text, exists) = confedit::read(&c.path).map_err(internal)?;
    let (parsed, error) = match confedit::parse(&text) {
        Ok(cf) => (json!(cf), Value::Null),
        Err(e) => (Value::Null, json!(e)),
    };
    let s = app.settings();
    Ok(json!({
        "path": c.path.display().to_string(),
        "dir": c.path.parent().map(|d| d.display().to_string()),
        "exists": exists,
        "editable": c.locked.is_none(),
        "locked": c.locked,
        "version": confedit::version(&text),
        "text": text,
        "config": parsed,
        "error": error,
        "builtin": {
            "runners": config::default_runners(),
            "presets": config::default_presets(),
            "harnesses": s.harnesses.iter().filter(|h| h.builtin).map(harness_cfg).collect::<Vec<_>>(),
        },
        "installed": s.runners.iter().map(|r| json!({
            "name": r.name, "available": r.program.is_some(),
            "program": r.program.as_ref().map(|p| p.display().to_string()),
        })).collect::<Vec<_>>(),
        "startup": {
            "bind": s.bind, "read_only": s.read_only,
            "targets": c.opts.targets.iter().map(|t| t.display().to_string()).collect::<Vec<_>>(),
        },
    }))
}

/// Load `text` exactly as `indra serve` would, without writing anything.
fn check_config(app: &App, c: &ConfigAccess, text: &str) -> ApiResult<Settings> {
    let file = confedit::parse(text).map_err(|e| bad(format!("not saved: {}", e)))?;
    let mut s = config::build_settings(&c.opts, Some(c.path.clone()), &file)
        .map_err(|e| bad(format!("not saved: {}", e)))?;
    // Where the console listens and whether it is read-only are fixed at start.
    let now = app.settings();
    s.bind = now.bind.clone();
    s.read_only = now.read_only;
    s.data_dir = now.data_dir.clone();
    Ok(s)
}

async fn config_get(State(app): State<Arc<App>>) -> ApiResult<Json<Value>> {
    Ok(Json(config_payload(&app)?))
}

#[derive(Deserialize)]
struct ConfigEdit {
    /// The `version` the page loaded; a save over a newer file is refused.
    version: String,
    /// Replace one section (`runner`, `node`, ...) with `value`...
    section: Option<String>,
    #[serde(default)]
    value: Value,
    /// ...or the whole file with `text`.
    text: Option<String>,
}

async fn config_put(
    State(app): State<Arc<App>>,
    Json(r): Json<ConfigEdit>,
) -> ApiResult<Json<Value>> {
    let c = config_access(&app)?;
    if let Some(why) = &c.locked {
        return Err(forbidden(why.clone()));
    }
    {
        let _one_at_a_time = c.saving.lock().unwrap_or_else(|e| e.into_inner());
        let (current, _) = confedit::read(&c.path).map_err(internal)?;
        if confedit::version(&current) != r.version {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "the config file changed since this page loaded it; reload to see the new version"
                    .into(),
            ));
        }
        let next = match (r.section, r.text) {
            (Some(section), None) => {
                confedit::set_section(&current, &section, &r.value).map_err(bad)?
            }
            (None, Some(text)) => text,
            _ => return Err(bad("send a section and its value, or the whole text")),
        };
        let settings = check_config(&app, c, &next)?;
        confedit::write_atomic(&c.path, &next).map_err(internal)?;
        apply_link_settings(&app.procs, &settings);
        app.settings.set(settings);
        app.node_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
    Ok(Json(config_payload(&app)?))
}

#[derive(Deserialize)]
struct ConfigCheck {
    text: String,
}

async fn config_check(
    State(app): State<Arc<App>>,
    Json(r): Json<ConfigCheck>,
) -> ApiResult<Json<Value>> {
    let c = config_access(&app)?;
    let s = check_config(&app, c, &r.text)?;
    Ok(Json(json!({
        "ok": true,
        "harnesses": s.harnesses.len(), "runners": s.runners.len(), "presets": s.presets.len(),
        "nodes": s.nodes.len(), "workflows": s.workflows.len(),
    })))
}

// ---- terminal -------------------------------------------------------------------

impl App {
    /// Whether the browser terminal is offered right now.
    pub fn shell_on(&self) -> bool {
        self.shell.load(std::sync::atomic::Ordering::SeqCst)
            && self.loopback
            && !self.settings().read_only
    }
}

async fn term_page() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (
                header::HeaderName::from_static("content-security-policy"),
                TERM_CSP,
            ),
        ],
        TERM_HTML,
    )
        .into_response()
}

#[derive(Deserialize)]
struct TermQ {
    /// shell | claude | agent
    what: String,
    h: Option<String>,
    target: Option<usize>,
    proc: Option<u64>,
}

/// Browsers send cookies with a WebSocket from any page and do not apply
/// CORS to it, so the terminal insists on the console's own origin.
fn same_origin(h: &HeaderMap) -> bool {
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok());
    let origin = h.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    match (host, origin) {
        (Some(host), Some(origin)) => {
            origin == format!("http://{}", host) || origin == format!("https://{}", host)
        }
        _ => false,
    }
}

/// What a terminal request starts, checked against the config.
fn term_spec(app: &App, q: &TermQ) -> ApiResult<crate::deck::term::Spec> {
    use crate::deck::term::{self, Spec};
    let s = app.settings();
    let claude = |name: Option<&str>| -> ApiResult<&config::Runner> {
        let r = match name {
            Some(n) => s.runners.iter().find(|r| r.name == n),
            None => s
                .runners
                .iter()
                .find(|r| r.format == "claude-stream-json" && r.program.is_some()),
        };
        let r = r.ok_or_else(|| bad("no Claude Code runner is configured"))?;
        if r.program.is_none() {
            return Err(bad(format!("runner '{}' is not installed", r.name)));
        }
        Ok(r)
    };
    let dir = || -> ApiResult<std::path::PathBuf> {
        let h = app.harness(q.h.as_deref())?;
        match q.target {
            Some(i) => h
                .targets
                .get(i)
                .cloned()
                .ok_or_else(|| bad("unknown target")),
            None => Ok(h.targets.first().cloned().unwrap_or_else(|| h.root.clone())),
        }
    };
    match q.what.as_str() {
        "shell" => {
            let cwd = dir()?;
            Ok(Spec {
                program: term::shell(),
                args: vec![],
                label: format!("shell in {}", cwd.display()),
                cwd,
            })
        }
        "claude" => {
            let cwd = dir()?;
            let r = claude(None)?;
            Ok(Spec {
                program: r.program.clone().unwrap(),
                args: vec![],
                label: format!("Claude Code in {}", cwd.display()),
                cwd,
            })
        }
        "agent" => {
            let id = q.proc.ok_or_else(|| bad("which process?"))?;
            let p = app
                .procs
                .get(id)
                .ok_or_else(|| not_found("no such process"))?;
            if p.node_name() != LOCAL {
                return Err(bad(format!(
                    "process {} runs on node '{}'; the terminal opens sessions on this machine only",
                    id,
                    p.node_name()
                )));
            }
            let session = p.view().session.ok_or_else(|| {
                bad("this agent has not reported a session (only Claude Code does)")
            })?;
            let r = claude(Some(p.runner_name()))?;
            if r.resume_args.is_empty() {
                return Err(bad(format!("runner '{}' has no resume_args", r.name)));
            }
            let mut args: Vec<String> = r
                .resume_args
                .iter()
                .map(|a| a.replace("{session}", &session))
                .collect();
            // While the agent still runs, branch its session rather than
            // writing into the same one from two places.
            let running = p.is_running();
            if running {
                args.extend(r.fork_args.iter().cloned());
            }
            Ok(Spec {
                program: r.program.clone().unwrap(),
                args,
                label: format!(
                    "session of process {}{}",
                    id,
                    if running { " (forked)" } else { "" }
                ),
                cwd: std::path::PathBuf::from(p.target()),
            })
        }
        _ => Err(bad("what must be shell, claude or agent")),
    }
}

/// What a terminal request would start, or why it cannot: the page asks
/// first, because a refused WebSocket tells the browser nothing.
async fn term_check(State(app): State<Arc<App>>, Query(q): Query<TermQ>) -> ApiResult<Json<Value>> {
    if !app.shell_on() {
        return Err(forbidden(
            "the terminal is off: start `indra serve --shell` (loopback only, not read-only)",
        ));
    }
    let spec = term_spec(&app, &q)?;
    Ok(Json(json!({
        "label": spec.label, "cwd": spec.cwd.display().to_string(),
        "program": spec.program.display().to_string(), "args": spec.args,
    })))
}

async fn term_ws(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(q): Query<TermQ>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    use std::sync::atomic::Ordering;
    if !app.shell_on() {
        return forbidden(
            "the terminal is off: start `indra serve --shell` (loopback only, not read-only)",
        )
        .into_response();
    }
    if !same_origin(&headers) {
        return forbidden("cross-origin terminal refused").into_response();
    }
    let spec = match term_spec(&app, &q) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !spec.cwd.is_dir() {
        return bad(format!("{} is not a directory", spec.cwd.display())).into_response();
    }
    if crate::deck::term::OPEN.load(Ordering::SeqCst) >= crate::deck::term::MAX_OPEN {
        return ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "too many terminals open".into(),
        )
        .into_response();
    }
    crate::deck::term::audit(&app.settings().data_dir.join("logs"), &spec);
    ws.on_upgrade(move |socket| crate::deck::term::bridge(spec, socket))
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .route(
            "/style.css",
            get(|| async { asset("text/css; charset=utf-8", STYLE) }),
        )
        .route(
            "/favicon.svg",
            get(|| async { asset("image/svg+xml", FAVICON) }),
        )
        .route("/term", get(term_page))
        .route(
            "/term.js",
            get(|| async { asset("text/javascript; charset=utf-8", TERM_JS) }),
        )
        .route(
            "/vendor/xterm.js",
            get(|| async { asset("text/javascript; charset=utf-8", XTERM_JS) }),
        )
        .route(
            "/vendor/xterm.css",
            get(|| async { asset("text/css; charset=utf-8", XTERM_CSS) }),
        )
        .route(
            "/vendor/addon-fit.js",
            get(|| async { asset("text/javascript; charset=utf-8", XTERM_FIT_JS) }),
        )
        .route(
            "/vendor/LICENSE-xterm",
            get(|| async { asset("text/plain; charset=utf-8", XTERM_LICENSE) }),
        )
        .route("/api/knowledge", get(crate::deck::knowledge::vaults))
        .route(
            "/api/knowledge/init",
            post(crate::deck::knowledge::vault_init),
        )
        .route(
            "/api/knowledge/notes",
            get(crate::deck::knowledge::notes).post(crate::deck::knowledge::note_create),
        )
        .route(
            "/api/knowledge/note",
            get(crate::deck::knowledge::note_get)
                .put(crate::deck::knowledge::note_put)
                .delete(crate::deck::knowledge::note_delete),
        )
        .route(
            "/api/knowledge/link",
            post(crate::deck::knowledge::note_link),
        )
        .route(
            "/api/knowledge/recall",
            get(crate::deck::knowledge::recall_preview),
        )
        .route("/api/knowledge/health", get(crate::deck::knowledge::health))
        .route("/api/knowledge/export", get(crate::deck::knowledge::export))
        .route(
            "/api/knowledge/import",
            post(crate::deck::knowledge::import)
                .layer(axum::extract::DefaultBodyLimit::max(64 << 20)),
        )
        .route(
            "/api/learnings/import",
            post(crate::deck::knowledge::learnings_import)
                .layer(axum::extract::DefaultBodyLimit::max(16 << 20)),
        )
        .route(
            "/api/learnings",
            get(crate::deck::knowledge::learnings_list)
                .post(crate::deck::knowledge::learning_create),
        )
        .route(
            "/api/learnings/{id}",
            axum::routing::patch(crate::deck::knowledge::learning_update)
                .delete(crate::deck::knowledge::learning_delete),
        )
        .route(
            "/api/learnings/{id}/accept",
            post(crate::deck::knowledge::learning_accept),
        )
        .route("/api/term", get(term_ws))
        .route("/api/term/check", get(term_check))
        .route("/api/overview", get(overview))
        .route("/api/config", get(config_get).put(config_put))
        .route("/api/config/check", post(config_check))
        .route("/api/board", get(board_get))
        .route("/api/board/items", post(board_create))
        .route(
            "/api/board/items/{id}",
            axum::routing::patch(board_update).delete(board_delete),
        )
        .route("/api/board/items/{id}/run", post(board_run))
        .route("/api/board/import", post(board_import))
        .route("/api/secrets", get(secrets_list))
        .route(
            "/api/secrets/{name}",
            axum::routing::put(secret_put).delete(secret_delete),
        )
        .route("/api/defs", get(defs))
        .route(
            "/api/def",
            get(def_get)
                .put(def_put)
                .post(def_create)
                .delete(def_delete),
        )
        .route("/api/graph", get(graph))
        .route("/api/memory", get(memory))
        .route("/api/memory/graph", get(memory_graph))
        .route("/api/runs", get(runs_list))
        .route("/api/run", get(run_get))
        .route("/api/run/file", get(run_file))
        .route("/api/procs", get(procs_list).post(proc_start))
        .route("/api/nodes", get(nodes_list))
        .route("/api/nodes/enrol", post(node_enrol))
        .route("/agent/connect", get(agent_connect))
        .route("/api/nodes/runs", get(node_runs))
        .route("/api/workflows", get(workflows_list).post(workflow_run))
        .route("/api/workflows/fanout", post(workflow_fanout))
        .route("/api/workflows/{id}", get(workflow_get))
        .route("/api/workflows/{id}/cancel", post(workflow_cancel))
        .route("/api/procs/{id}", get(proc_get))
        .route("/api/procs/{id}/stop", post(proc_stop))
        .route("/api/procs/{id}/pivot", post(proc_pivot))
        .route("/api/procs/{id}/permission", post(proc_permission))
        .route("/api/procs/{id}/control", post(proc_control))
        .route("/api/procs/{id}/message", post(proc_message))
        .route("/api/procs/{id}/events", get(proc_events))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::config::{Harness, Runner, StageCfg};
    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn fixture(name: &str, read_only: bool) -> (Arc<App>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("indra-web-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        let w = |rel: &str, s: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        w("agents/alpha.md", "---\nname: alpha\ndescription: Plans the work\nmodel: opus\ntools: Read, Task\n---\nSpawn `beta` for each task. Load the kb-* skills.\n");
        w(
            "agents/beta.md",
            "---\nname: beta\ndescription: Does the work\n---\nUse kb-one.\n",
        );
        w(
            "skills/kb-one/SKILL.md",
            "---\nname: kb-one\ndescription: Knowledge\n---\nbody\n",
        );
        w("target/runs/run-1/plan.md", "the plan");
        w(
            "target/runs/run-1/results.jsonl",
            "{\"agent\":\"beta\",\"status\":\"ok\"}\n",
        );
        w("secret.txt", "outside the run");
        let target = std::fs::canonicalize(dir.join("target")).unwrap();
        let h = Harness {
            name: "t".into(),
            description: String::new(),
            root: std::fs::canonicalize(&dir).unwrap(),
            agents: vec!["agents/*.md".into()],
            skills: vec!["skills/*/SKILL.md".into()],
            prompts: vec![],
            memory: vec![],
            targets: vec![target],
            runs: Some("runs/*".into()),
            findings: vec!["results.jsonl".into()],
            group_by: Some("agent".into()),
            stages: vec![StageCfg {
                name: "plan".into(),
                agent: Some("alpha".into()),
                done_when: Some("plan.md".into()),
            }],
            builtin: false,
        };
        let echo = Runner {
            name: "echo".into(),
            command: "echo".into(),
            args: vec!["{prompt}".into()],
            format: "text".into(),
            resume_args: vec![],
            fork_args: vec![],
            model_args: vec![],
            program: crate::deck::config::which("echo"),
        };
        let settings = Settings {
            harnesses: vec![h],
            runners: vec![echo],
            presets: vec![],
            profiles: vec![],
            connectors: vec![],
            components: vec![],
            bind: "127.0.0.1:0".into(),
            read_only,
            config_path: None,
            data_dir: dir.join("data"),
            local_max_parallel: 4,
            webrtc: false,
            ice_servers: vec![],
            nodes: vec![],
            workflows: vec![],
        };
        let app = App::new(settings, TOKEN.into(), true);
        (app, dir)
    }

    async fn call(
        app: &Arc<App>,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, String) {
        let mut b = Request::builder().method(method).uri(uri);
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            b = b.header("host", "127.0.0.1:7878");
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let res = router(app.clone())
            .oneshot(b.body(body).unwrap())
            .await
            .unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    const AUTH: (&str, &str) = ("authorization", "Bearer 0123456789abcdef0123456789abcdef");

    #[tokio::test]
    async fn token_host_and_headers() {
        let (app, dir) = fixture("auth", false);
        assert_eq!(
            call(&app, "GET", "/api/overview", &[], None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/overview",
                &[("authorization", "Bearer nope")],
                None
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        let (s, h, body) = call(&app, "GET", "/api/overview", &[AUTH], None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'self'"));
        assert!(body.contains("\"name\":\"t\""));
        let cookie = format!("{}={}", COOKIE, TOKEN);
        assert_eq!(
            call(&app, "GET", "/api/overview", &[("cookie", &cookie)], None)
                .await
                .0,
            StatusCode::OK
        );
        // DNS rebinding: a foreign Host is refused even with the token.
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/overview",
                &[AUTH, ("host", "evil.example:7878")],
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "GET", "/", &[("host", "evil.example")], None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "GET", "/", &[("host", "localhost:7878")], None)
                .await
                .0,
            StatusCode::OK
        );

        // The URL token is traded for an HttpOnly, SameSite=Strict cookie.
        let (s, h, _) = call(&app, "GET", &format!("/?token={}", TOKEN), &[], None).await;
        assert_eq!(s, StatusCode::SEE_OTHER);
        let set = h["set-cookie"].to_str().unwrap();
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict"));
        assert_eq!(h["location"], "/");
        let (s, h, _) = call(&app, "GET", "/?token=wrong", &[], None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h.get("set-cookie").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn definitions_graph_and_safe_saves() {
        let (app, dir) = fixture("defs", false);
        let (_, _, body) = call(&app, "GET", "/api/defs?h=t", &[AUTH], None).await;
        let defs: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(defs.as_array().unwrap().len(), 3);
        assert_eq!(defs[0]["model"], "opus");
        assert_eq!(defs[0]["tools"], json!(["Read", "Task"]));

        let (_, _, body) = call(&app, "GET", "/api/graph", &[AUTH], None).await;
        let g: Value = serde_json::from_str(&body).unwrap();
        let edges: Vec<(String, String, String)> = g["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["source"].as_str().unwrap().into(),
                    e["target"].as_str().unwrap().into(),
                    e["rel"].as_str().unwrap().into(),
                )
            })
            .collect();
        assert!(edges.contains(&("agent:alpha".into(), "agent:beta".into(), "spawns".into())));
        assert!(
            edges.contains(&("agent:alpha".into(), "skill:kb-one".into(), "loads".into())),
            "family reference kb-*"
        );
        assert!(edges.contains(&("agent:beta".into(), "skill:kb-one".into(), "invokes".into())));
        assert!(edges.contains(&("stage:plan".into(), "agent:alpha".into(), "runs".into())));

        let (_, _, body) = call(&app, "GET", "/api/def?id=agent:beta", &[AUTH], None).await;
        let d: Value = serde_json::from_str(&body).unwrap();
        let hash = d["hash"].as_str().unwrap().to_string();
        assert_eq!(d["used_by"][0]["id"], "agent:alpha");

        let save = |content: &str, hash: &str| json!({ "id": "agent:beta", "content": content, "hash": hash });
        let origin = ("origin", "http://127.0.0.1:7878");
        let (s, _, _) = call(
            &app,
            "PUT",
            "/api/def",
            &[AUTH, origin],
            Some(save("new", "stale")),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);
        let (s, _, _) = call(
            &app,
            "PUT",
            "/api/def",
            &[AUTH, ("origin", "http://evil.example")],
            Some(save("new", &hash)),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _, _) = call(
            &app,
            "PUT",
            "/api/def",
            &[AUTH, ("sec-fetch-site", "cross-site")],
            Some(save("new", &hash)),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _, _) = call(
            &app,
            "PUT",
            "/api/def",
            &[AUTH, origin],
            Some(json!({ "id": "agent:../../secret", "content": "x", "hash": hash })),
        )
        .await;
        assert_eq!(
            s,
            StatusCode::NOT_FOUND,
            "only discovered files can be written"
        );
        let (s, _, _) = call(
            &app,
            "PUT",
            "/api/def",
            &[AUTH, origin],
            Some(save("---\nname: beta\n---\nnew body\n", &hash)),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert!(std::fs::read_to_string(dir.join("agents/beta.md"))
            .unwrap()
            .contains("new body"));

        let (ro, ro_dir) = fixture("defs-ro", true);
        let (s, _, _) = call(&ro, "PUT", "/api/def", &[AUTH], Some(save("x", &hash))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(ro_dir);
    }

    #[tokio::test]
    async fn runs_and_confined_files() {
        let (app, dir) = fixture("runs", false);
        let (_, _, body) = call(&app, "GET", "/api/runs", &[AUTH], None).await;
        let runs: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(runs[0]["key"], "0:run-1");
        assert_eq!(runs[0]["stages"][0]["state"], "done");
        assert_eq!(runs[0]["counts"]["by_group"]["beta"], 1);
        let (s, _, body) = call(
            &app,
            "GET",
            "/api/run/file?key=0:run-1&rel=plan.md",
            &[AUTH],
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("the plan"));
        for rel in [
            "../../../secret.txt",
            "%2E%2E/%2E%2E/%2E%2E/secret.txt",
            "/etc/passwd",
        ] {
            let (s, _, _) = call(
                &app,
                "GET",
                &format!("/api/run/file?key=0:run-1&rel={}", rel),
                &[AUTH],
                None,
            )
            .await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{}", rel);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The Settings page: edits keep the file's comments, apply at once, and
    /// a bad, stale or locked edit writes nothing.
    #[tokio::test]
    async fn settings_are_edited_checked_and_applied() {
        let dir = std::env::temp_dir().join(format!("indra-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("work")).unwrap();
        let path = dir.join("indra.toml");
        let original = "# my comment\nbuiltins = false\n\n[[harness]]\nname = \"h\"\nroot = \".\"\ntargets = [\"work\"]\n\n# runners\n[[runner]]\nname = \"say\"\ncommand = \"echo\"\nargs = [\"{prompt}\"]\n";
        std::fs::write(&path, original).unwrap();
        let opts = config::LoadOptions {
            config: Some(path.clone()),
            targets: vec![],
            bind: None,
            read_only: false,
            cwd: dir.clone(),
        };
        let mk = |locked: Option<String>| {
            let s = config::load(opts.clone()).unwrap();
            App::with_config(
                s,
                TOKEN.into(),
                true,
                Some(ConfigAccess {
                    opts: opts.clone(),
                    path: path.clone(),
                    locked,
                    saving: Mutex::new(()),
                }),
            )
        };
        let app = mk(None);
        let origin = ("origin", "http://127.0.0.1:7878");
        let get = |app: Arc<App>| async move {
            let (s, _, body) = call(&app, "GET", "/api/config", &[AUTH], None).await;
            assert_eq!(s, StatusCode::OK, "{}", body);
            serde_json::from_str::<Value>(&body).unwrap()
        };
        let c = get(app.clone()).await;
        assert_eq!(c["editable"], true);
        assert_eq!(c["config"]["runner"][0]["name"], "say");
        let v1 = c["version"].as_str().unwrap().to_string();

        // Add a runner and a workflow that uses it: both apply without a restart.
        let (s, _, body) = call(&app, "PUT", "/api/config", &[AUTH, origin], Some(json!({
            "version": v1, "section": "runner",
            "value": [{ "name": "say", "command": "echo", "args": ["{prompt}"] }, { "name": "shout", "command": "echo", "args": ["LOUD {prompt}"] }]
        }))).await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let c: Value = serde_json::from_str(&body).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my comment") && text.contains("# runners"),
            "comments kept:\n{}",
            text
        );
        assert!(
            app.settings().runners.iter().any(|r| r.name == "shout"),
            "applied at once"
        );
        let (s, _, body) = call(&app, "PUT", "/api/config", &[AUTH, origin], Some(json!({
            "version": c["version"], "section": "workflow",
            "value": [{ "name": "hello", "task": [{ "id": "one", "target": "work", "runner": "shout", "prompt": "hi" }] }]
        }))).await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let c: Value = serde_json::from_str(&body).unwrap();
        let (s, _, body) = call(
            &app,
            "POST",
            "/api/workflows",
            &[AUTH, origin],
            Some(json!({ "name": "hello" })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "the new workflow runs: {}", body);
        let wf = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        let mut status = String::new();
        for _ in 0..200 {
            status = app.engine.get(wf).unwrap().status.to_string();
            if status != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(status, "succeeded");

        // A stale page cannot overwrite a newer file.
        let (s, _, _) = call(
            &app,
            "PUT",
            "/api/config",
            &[AUTH, origin],
            Some(json!({ "version": v1, "section": "preset", "value": [] })),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);
        // A change that would not load is refused, and nothing is written.
        let before = std::fs::read_to_string(&path).unwrap();
        let (s, _, body) = call(&app, "PUT", "/api/config", &[AUTH, origin], Some(json!({
            "version": c["version"], "section": "runner", "value": [{ "name": "x", "command": "echo", "args": ["no prompt"] }]
        }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(body.contains("{prompt}"), "{}", body);
        let (s, _, body) = call(&app, "PUT", "/api/config", &[AUTH, origin], Some(json!({
            "version": c["version"], "section": "preset", "value": [{ "name": "p", "prompt": "a" }, { "name": "p", "prompt": "b" }]
        }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "names must be unique: {}", body);
        assert!(body.contains("used twice"), "{}", body);
        let (s, _, _) = call(
            &app,
            "PUT",
            "/api/config",
            &[AUTH, origin],
            Some(json!({ "version": c["version"], "text": "[[runner]\n" })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _, _) = call(&app, "PUT", "/api/config", &[AUTH, origin], Some(json!({
            "version": c["version"], "section": "workflow", "value": [{ "name": "loop", "task": [
                { "id": "a", "target": ".", "prompt": "x", "needs": ["b"] }, { "id": "b", "target": ".", "prompt": "x", "needs": ["a"] }] }]
        }))).await;
        assert_eq!(
            s,
            StatusCode::BAD_REQUEST,
            "a dependency cycle is caught before saving"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        // Check without saving.
        let (s, _, body) = call(
            &app,
            "POST",
            "/api/config/check",
            &[AUTH, origin],
            Some(json!({ "text": before })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        assert!(body.contains("\"workflows\":1"), "{}", body);
        // The whole text can be replaced too.
        let edited = before.replace("LOUD", "QUIET");
        let (s, _, body) = call(
            &app,
            "PUT",
            "/api/config",
            &[AUTH, origin],
            Some(json!({ "version": c["version"], "text": edited })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        assert!(app
            .settings()
            .runners
            .iter()
            .any(|r| r.args.iter().any(|a| a.contains("QUIET"))));
        // A cross-site page cannot do any of this.
        let (s, _, _) = call(
            &app,
            "PUT",
            "/api/config",
            &[AUTH, ("origin", "http://evil.test")],
            Some(json!({ "version": "x", "text": "" })),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        // Locked (read-only, --lock-config, or a non-loopback bind): reads only.
        let locked = mk(Some("locked for the test".into()));
        let c = get(locked.clone()).await;
        assert_eq!(c["editable"], false);
        let (s, _, _) = call(
            &locked,
            "PUT",
            "/api/config",
            &[AUTH, origin],
            Some(json!({ "version": c["version"], "section": "preset", "value": [] })),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        // Without a config file to edit, the API says so.
        let (plain, plain_dir) = fixture("cfg-none", false);
        let (s, _, _) = call(&plain, "GET", "/api/config", &[AUTH], None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(plain_dir);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn launch_stream_and_validation() {
        let (app, dir) = fixture("procs", false);
        let origin = ("origin", "http://127.0.0.1:7878");
        let (s, _, _) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "nope", "prompt": "x", "target": 0 })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _, _) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "echo", "prompt": "x", "target": 9 })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _, _) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "echo", "prompt": "  ", "target": 0 })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        // The prompt reaches the program as one argument; `;` is not a shell.
        let (s, _, body) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "echo", "prompt": "hello {target}; touch pwned", "target": 0 })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        let mut status = String::new();
        for _ in 0..100 {
            let (_, _, body) =
                call(&app, "GET", &format!("/api/procs/{}", id), &[AUTH], None).await;
            let v: Value = serde_json::from_str(&body).unwrap();
            status = v["status"].as_str().unwrap().to_string();
            if status != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(status, "exited");
        assert!(!dir.join("target/pwned").exists());

        // The event stream replays history, then stays open for live events.
        let req = Request::builder()
            .uri(format!("/api/procs/{}/events", id))
            .header("host", "127.0.0.1")
            .header(AUTH.0, AUTH.1)
            .body(Body::empty())
            .unwrap();
        let res = router(app.clone()).oneshot(req).await.unwrap();
        assert_eq!(res.headers()["content-type"], "text/event-stream");
        let mut body = res.into_body();
        let mut seen = String::new();
        while !seen.contains("\"kind\":\"exit\"") {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(5), body.frame())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Ok(d) = frame.into_data() {
                seen.push_str(&String::from_utf8_lossy(&d));
            }
        }
        let target = dir.join("target").canonicalize().unwrap();
        assert!(
            seen.contains(&format!("hello {}; touch pwned", target.display())),
            "{}",
            seen
        );
        let (_, _, body) = call(
            &app,
            "POST",
            &format!("/api/procs/{}/stop", id),
            &[AUTH, origin],
            None,
        )
        .await;
        assert!(body.contains("\"stopping\":false"), "already finished");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nodes_fanout_and_workflow_api() {
        let (app, dir) = fixture("wf", false);
        let origin = ("origin", "http://127.0.0.1:7878");
        let (s, _, body) = call(&app, "GET", "/api/nodes", &[AUTH], None).await;
        assert_eq!(s, StatusCode::OK);
        let nodes: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(nodes[0]["name"], "local");
        assert_eq!(nodes[0]["targets"].as_array().unwrap().len(), 1);
        assert!(nodes[0]["info"]["cpus"].as_u64().unwrap() >= 1);

        // Unknown nodes, targets and workflows are refused.
        let (s, _, _) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "node": "nope", "runner": "echo", "prompt": "x", "target": 0 })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _, _) = call(
            &app,
            "POST",
            "/api/workflows",
            &[AUTH, origin],
            Some(json!({ "name": "nope" })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _, _) = call(
            &app,
            "POST",
            "/api/workflows/9/cancel",
            &[AUTH, origin],
            None,
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _, _) = call(&app, "POST", "/api/workflows/fanout", &[AUTH, origin], Some(json!({ "runner": "echo", "prompt": "x", "targets": [{ "node": "local", "target": 5 }] }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _, _) = call(
            &app,
            "POST",
            "/api/workflows/fanout",
            &[AUTH, origin],
            Some(json!({ "runner": "echo", "prompt": "x", "targets": [] })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        // A fan-out over the local target runs to completion.
        let (s, _, body) = call(&app, "POST", "/api/workflows/fanout", &[AUTH, origin], Some(json!({ "runner": "echo", "prompt": "hi {target}", "targets": [{ "node": "local", "target": 0 }] }))).await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        let mut run = Value::Null;
        for _ in 0..60 {
            let (_, _, body) = call(
                &app,
                "GET",
                &format!("/api/workflows/{}", id),
                &[AUTH],
                None,
            )
            .await;
            run = serde_json::from_str(&body).unwrap();
            if run["status"] != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(run["status"], "succeeded", "{}", run);
        assert_eq!(run["tasks"][0]["id"], "local-target");
        let (_, _, body) = call(&app, "GET", "/api/procs", &[AUTH], None).await;
        assert!(body.contains("\"node\":\"local\""));

        let (ro, ro_dir) = fixture("wf-ro", true);
        let (s, _, _) = call(&ro, "POST", "/api/workflows/fanout", &[AUTH, origin], Some(json!({ "runner": "echo", "prompt": "x", "targets": [{ "node": "local", "target": 0 }] }))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(ro_dir);
    }

    #[cfg(unix)]
    async fn wait_done(app: &Arc<App>, id: u64) -> Value {
        for _ in 0..100 {
            let (_, _, body) = call(app, "GET", &format!("/api/procs/{}", id), &[AUTH], None).await;
            let v: Value = serde_json::from_str(&body).unwrap();
            if v["status"] != "running" {
                return v;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        panic!("process {} did not finish", id);
    }

    /// A stand-in for Claude Code's control channel (the protocol the Agent
    /// SDK uses): it asks to run a tool, answers mode and model changes, and
    /// takes follow-up messages.
    #[cfg(unix)]
    const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, os, sys
log = open(os.environ["FAKE_LOG"], "a")
log.write("ARGV " + json.dumps(sys.argv[1:]) + "\n")
log.write("ENV " + os.environ.get("INDRA_TEST_ENV", "-") + "\n")
log.flush()
def out(o):
    print(json.dumps(o), flush=True)
def recv():
    l = sys.stdin.readline()
    if not l:
        sys.exit(0)
    log.write("IN " + l); log.flush()
    return json.loads(l)
m = recv()
out({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {}}})
recv()
out({"type": "system", "subtype": "init", "model": "claude-sonnet", "permissionMode": "manual", "session_id": "s-1"})
out({"type": "control_request", "request_id": "perm-1", "request": {"subtype": "can_use_tool", "tool_name": "Bash",
     "input": {"command": "rm -rf build"},
     "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": "Bash"}], "behavior": "allow", "destination": "session"}]}})
while True:
    m = recv()
    if m["type"] == "control_request":
        sub = m["request"]["subtype"]
        if sub == "set_permission_mode":
            out({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {"mode": m["request"]["mode"]}}})
            out({"type": "system", "subtype": "status", "permissionMode": m["request"]["mode"], "session_id": "s-1"})
        elif sub == "set_model":
            out({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {"model": "claude-opus-x"}}})
    elif m["type"] == "control_response":
        r = m["response"]["response"]
        said = "decision: " + r["behavior"] + (" always" if r.get("updatedPermissions") else "")
        out({"type": "assistant", "message": {"content": [{"type": "text", "text": said}]}, "session_id": "s-1"})
        out({"type": "result", "subtype": "success", "num_turns": 1, "duration_ms": 5, "result": "turn done", "session_id": "s-1"})
    elif m["type"] == "user":
        out({"type": "assistant", "message": {"content": [{"type": "text", "text": "follow-up: " + m["message"]["content"]}]}})
        out({"type": "result", "subtype": "success", "num_turns": 1, "duration_ms": 5, "result": "follow-up done"})
"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn agents_are_approved_steered_and_finished_from_the_console() {
        use std::os::unix::fs::PermissionsExt;
        let (base, dir) = fixture("control", false);
        let script = dir.join("fake-claude.py");
        let log = dir.join("fake.log");
        std::fs::write(&script, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("FAKE_LOG", &log);
        let mut settings = (*base.settings()).clone();
        settings.runners.push(Runner {
            name: "fake".into(),
            command: script.display().to_string(),
            args: vec![
                "-p".into(),
                "{prompt}".into(),
                "--output-format".into(),
                "stream-json".into(),
            ],
            format: "claude-stream-json".into(),
            resume_args: vec!["--resume".into(), "{session}".into()],
            fork_args: vec![],
            model_args: vec![],
            program: Some(script.clone()),
        });
        settings.profiles.push(config::ProfileCfg {
            name: "careful".into(),
            runner: Some("fake".into()),
            model: Some("sonnet".into()),
            permission_mode: Some("manual".into()),
            effort: Some("high".into()),
            allowed_tools: vec!["Read".into(), "Grep".into()],
            env: [("INDRA_TEST_ENV".to_string(), "yes".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        });
        let app = App::new(settings, TOKEN.into(), true);
        let origin = ("origin", "http://127.0.0.1:7878");
        let post = |path: String, body: Value| {
            let app = app.clone();
            async move { call(&app, "POST", &path, &[AUTH, origin], Some(body)).await }
        };
        let view = |id: u64| {
            let app = app.clone();
            async move {
                let (_, _, body) =
                    call(&app, "GET", &format!("/api/procs/{}", id), &[AUTH], None).await;
                serde_json::from_str::<Value>(&body).unwrap()
            }
        };
        async fn until<F, Fut>(what: &str, mut f: F) -> Value
        where
            F: FnMut() -> Fut,
            Fut: std::future::Future<Output = Option<Value>>,
        {
            for _ in 0..200 {
                if let Some(v) = f().await {
                    return v;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            panic!("timed out waiting for {}", what);
        }

        let (s, _, body) = post(
            "/api/procs".into(),
            json!({ "profile": "careful", "prompt": "clean up", "target": 0 }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();

        // It asks before running a command, and the console shows the request.
        let v = until("an approval request", || async {
            let v = view(id).await;
            (v["pending"].as_array().map(|a| a.len()) == Some(1)).then_some(v)
        })
        .await;
        assert_eq!(v["pending"][0]["tool"], "Bash");
        assert_eq!(v["pending"][0]["input"]["command"], "rm -rf build");
        assert_eq!(v["interactive"], true);
        assert_eq!(v["open"], true);
        assert_eq!(v["mode"], "manual");
        let (_, _, overview) = call(&app, "GET", "/api/overview", &[AUTH], None).await;
        assert!(overview.contains("\"approvals\":1"), "{}", overview);

        // Change its mode and model while it waits.
        let (s, _, body) = post(
            format!("/api/procs/{}/control", id),
            json!({ "mode": "acceptEdits", "model": "opus" }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        until("the new mode and model", || async {
            let v = view(id).await;
            (v["mode"] == "acceptEdits" && v["model"] == "claude-opus-x").then_some(v)
        })
        .await;
        let (s, _, _) = post(
            format!("/api/procs/{}/control", id),
            json!({ "mode": "yolo" }),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "unknown modes are refused");

        // A follow-up, then the answer: allow, and always for this session.
        let (s, _, body) = post(
            format!("/api/procs/{}/message", id),
            json!({ "text": "also update the docs" }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let (s, _, body) = post(
            format!("/api/procs/{}/permission", id),
            json!({ "request_id": "perm-1", "allow": true, "always": true }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let (s, _, _) = post(
            format!("/api/procs/{}/permission", id),
            json!({ "request_id": "perm-1", "allow": true }),
        )
        .await;
        assert_eq!(
            s,
            StatusCode::BAD_REQUEST,
            "an answered request cannot be answered again"
        );

        // Both turns finish, its input is closed, and it exits.
        let v = until("the agent to exit", || async {
            let v = view(id).await;
            (v["status"] != "running").then_some(v)
        })
        .await;
        assert_eq!(v["status"], "exited", "{}", v);
        assert_eq!(v["open"], false);
        let p = app.procs.get(id).unwrap();
        let (events, _) = p.subscribe(0);
        let text: Vec<String> = events
            .iter()
            .map(|e| format!("{}: {}", e.kind, e.text))
            .collect();
        let all = text.join("\n");
        for want in [
            "permission: asks to use Bash: rm -rf build",
            "decision: allow always",
            "follow-up: also update the docs",
            "permission mode is now acceptEdits",
            "model is now claude-opus-x",
            "permission: allowed Bash (always, for this session)",
        ] {
            assert!(all.contains(want), "missing {:?} in:\n{}", want, all);
        }
        let seen = std::fs::read_to_string(&log).unwrap();
        for want in [
            "\"--model\", \"sonnet\"",
            "\"--permission-mode\", \"manual\"",
            "\"--effort\", \"high\"",
            "\"--allowed-tools\", \"Read,Grep\"",
            "\"--input-format\", \"stream-json\"",
            "ENV yes",
            "\"updatedPermissions\"",
        ] {
            assert!(seen.contains(want), "missing {:?} in:\n{}", want, seen);
        }
        assert!(
            !seen.contains("\"clean up\", "),
            "the prompt goes over stdin, not argv:\n{}",
            seen
        );

        // Once it has finished, it takes no more input.
        let (s, body) = {
            let (s, _, b) = post(
                format!("/api/procs/{}/message", id),
                json!({ "text": "more" }),
            )
            .await;
            (s, b)
        };
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", body);
        // An unknown profile is refused before anything starts.
        let (s, _, _) = post(
            "/api/procs".into(),
            json!({ "profile": "nope", "prompt": "x", "target": 0 }),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The board against a stand-in tracker that speaks GitHub's and Jira's
    /// REST APIs: import, run with a profile, report the result back, and
    /// report "done" when a person accepts it.
    #[cfg(unix)]
    #[tokio::test]
    async fn board_items_flow_from_trackers_to_agents_and_back() {
        use axum::extract::{Path as P, State as St};
        type Log = Arc<Mutex<Vec<String>>>;
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        fn rec(log: &Log, h: &HeaderMap, what: String) {
            let auth = h
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_string();
            log.lock().unwrap().push(format!("{} [{}]", what, auth));
        }
        let mock = Router::new()
            .route("/repos/o/r/issues", get(|St(l): St<Log>, h: HeaderMap| async move {
                rec(&l, &h, "gh list".into());
                Json(json!([
                    { "number": 7, "title": "Fix login", "body": "The login form accepts empty passwords.", "html_url": "http://gh/7", "state": "open" },
                    { "number": 8, "title": "a PR", "pull_request": {}, "html_url": "http://gh/8", "state": "open" }
                ]))
            }))
            .route("/repos/o/r/issues/{n}/comments", post(|St(l): St<Log>, h: HeaderMap, P(n): P<String>, Json(b): Json<Value>| async move {
                rec(&l, &h, format!("gh comment {} {}", n, b["body"].as_str().unwrap_or("")));
                Json(json!({}))
            }))
            .route("/repos/o/r/issues/{n}/labels", post(|St(l): St<Log>, h: HeaderMap, P(n): P<String>, Json(b): Json<Value>| async move {
                rec(&l, &h, format!("gh label {} {}", n, b["labels"][0].as_str().unwrap_or("")));
                Json(json!([]))
            }))
            .route("/repos/o/r/issues/{n}/labels/{label}", axum::routing::delete(|St(l): St<Log>, h: HeaderMap, P((n, label)): P<(String, String)>| async move {
                rec(&l, &h, format!("gh unlabel {} {}", n, label));
                StatusCode::NOT_FOUND
            }))
            .route("/repos/o/r/issues/{n}", axum::routing::patch(|St(l): St<Log>, h: HeaderMap, P(n): P<String>, Json(b): Json<Value>| async move {
                rec(&l, &h, format!("gh state {} {}", n, b["state"].as_str().unwrap_or("")));
                Json(json!({}))
            }))
            .route("/rest/api/3/search/jql", get(|St(l): St<Log>, h: HeaderMap| async move {
                rec(&l, &h, "jira search".into());
                Json(json!({ "issues": [{ "key": "SEC-1", "fields": { "summary": "XSS in search", "status": { "name": "To Do" },
                    "description": { "type": "doc", "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": "Reflected in q=" }] }] } } }] }))
            }))
            .route("/rest/api/3/issue/{k}/transitions", get(|St(l): St<Log>, h: HeaderMap, P(k): P<String>| async move {
                rec(&l, &h, format!("jira transitions {}", k));
                Json(json!({ "transitions": [
                    { "id": "21", "name": "Start", "to": { "name": "In Progress" } },
                    { "id": "31", "name": "Send for review", "to": { "name": "In Review" } }] }))
            }).post(|St(l): St<Log>, h: HeaderMap, P(k): P<String>, Json(b): Json<Value>| async move {
                rec(&l, &h, format!("jira move {} {}", k, b["transition"]["id"].as_str().unwrap_or("")));
                StatusCode::NO_CONTENT
            }))
            .route("/rest/api/3/issue/{k}/comment", post(|St(l): St<Log>, h: HeaderMap, P(k): P<String>, Json(b): Json<Value>| async move {
                rec(&l, &h, format!("jira comment {} {}", k, b["body"]["content"][0]["content"][0]["text"].as_str().unwrap_or("")));
                Json(json!({}))
            }))
            .with_state(log.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });

        let (fx, dir) = fixture("board", false);
        let mut settings = (*fx.settings()).clone();
        settings.runners.push(Runner {
            name: "say".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "echo \"did: $0\"".into(), "{prompt}".into()],
            format: "text".into(),
            resume_args: vec![],
            fork_args: vec![],
            model_args: vec![],
            program: crate::deck::config::which("sh"),
        });
        settings.profiles.push(config::ProfileCfg {
            name: "w".into(),
            runner: Some("say".into()),
            ..Default::default()
        });
        let states = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        settings.connectors.push(config::ConnectorCfg {
            name: "gh".into(),
            kind: "github".into(),
            url: Some(base.clone()),
            project: "o/r".into(),
            token: "secret:ghtok".into(),
            profile: Some("w".into()),
            states: states(&[
                ("running", "in-progress"),
                ("review", "needs-review"),
                ("done", "closed"),
            ]),
            ..Default::default()
        });
        settings.connectors.push(config::ConnectorCfg {
            name: "jira".into(),
            kind: "jira".into(),
            url: Some(base.clone()),
            project: "SEC".into(),
            email: Some("me@x.com".into()),
            token: "secret:jt".into(),
            profile: Some("w".into()),
            states: states(&[("running", "In Progress"), ("review", "In Review")]),
            ..Default::default()
        });
        let app = App::new(settings, TOKEN.into(), true);
        let set = |v: &str, scope: &str| crate::deck::secrets::Change {
            value: Some(v.into()),
            profiles: Some(vec![scope.into()]),
            ..Default::default()
        };
        app.procs
            .vault()
            .set("ghtok", set("gh-token-1", "connector:gh"))
            .unwrap();
        app.procs
            .vault()
            .set("jt", set("jira-token-2", "connector:somebody-else"))
            .unwrap();
        let origin = ("origin", "http://127.0.0.1:7878");

        // Import: the issue, not the pull request.
        let (s, _, body) = call(
            &app,
            "POST",
            "/api/board/import",
            &[AUTH, origin],
            Some(json!({ "connector": "gh" })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        assert!(body.contains("\"added\":1"), "{}", body);
        // A secret scoped to another connector is not handed to this one.
        let (s, _, body) = call(
            &app,
            "POST",
            "/api/board/import",
            &[AUTH, origin],
            Some(json!({ "connector": "jira" })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", body);
        app.procs
            .vault()
            .set("jt", set("jira-token-2", "connector:jira"))
            .unwrap();
        let (s, _, body) = call(
            &app,
            "POST",
            "/api/board/import",
            &[AUTH, origin],
            Some(json!({ "connector": "jira" })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);

        let board = |app: Arc<App>| async move {
            let (_, _, b) = call(&app, "GET", "/api/board", &[AUTH], None).await;
            serde_json::from_str::<Value>(&b).unwrap()
        };
        let b = board(app.clone()).await;
        let item = |b: &Value, key: &str| {
            b["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["source"]["key"] == key)
                .cloned()
                .unwrap()
        };
        let gh = item(&b, "7");
        assert_eq!(gh["column"], "todo");
        assert_eq!(item(&b, "SEC-1")["body"], "Reflected in q=");

        // Run it with the connector's profile; the agent finishes; the item goes to review.
        for key in ["7", "SEC-1"] {
            let id = item(&b, key)["id"].as_u64().unwrap();
            let (s, _, body) = call(
                &app,
                "POST",
                &format!("/api/board/items/{}/run", id),
                &[AUTH, origin],
                Some(json!({})),
            )
            .await;
            assert_eq!(s, StatusCode::OK, "{}", body);
        }
        let mut b = Value::Null;
        for _ in 0..200 {
            b = board(app.clone()).await;
            if b["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["column"] == "review")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let gh = item(&b, "7");
        assert_eq!(gh["column"], "review", "{}", b);
        assert!(
            gh["result"]
                .as_str()
                .unwrap()
                .contains("did: Work on this task."),
            "{}",
            gh
        );
        assert!(
            gh["result"].as_str().unwrap().contains("Fix login"),
            "{}",
            gh
        );

        // A person accepts it: done goes back to the tracker.
        let id = gh["id"].as_u64().unwrap();
        let (s, _, body) = call(
            &app,
            "PATCH",
            &format!("/api/board/items/{}", id),
            &[AUTH, origin],
            Some(json!({ "column": "done" })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let (s, _, _) = call(
            &app,
            "PATCH",
            &format!("/api/board/items/{}", id),
            &[AUTH, origin],
            Some(json!({ "column": "running" })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "only Run makes an item running");

        let seen = log.lock().unwrap().join("\n");
        for want in [
            "gh list [Bearer gh-token-1]",
            "gh comment 7 indra started an agent on this (process #",
            "gh label 7 in-progress",
            "gh comment 7 indra agent process #",
            "gh label 7 needs-review",
            "gh unlabel 7 in-progress",
            "gh state 7 closed",
            "jira search [Basic bWVAeC5jb206amlyYS10b2tlbi0y]",
            "jira move SEC-1 21",
            "jira move SEC-1 31",
            "jira comment SEC-1 indra started an agent",
        ] {
            assert!(seen.contains(want), "missing {:?} in:\n{}", want, seen);
        }
        assert!(
            !seen.contains("jira-token-2]") || seen.contains("Basic"),
            "tokens only travel in the auth header"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Secrets: set write-only, delivered to the agent that may have them,
    /// redacted from everything it prints, refused outside their scope.
    #[cfg(unix)]
    #[tokio::test]
    async fn secrets_reach_only_their_agents_and_never_show() {
        let (base, dir) = fixture("secrets", false);
        let mut settings = (*base.settings()).clone();
        settings.runners.push(Runner {
            name: "env".into(),
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "echo \"token=$INDRA_TOK\"".into(),
                "{prompt}".into(),
            ],
            format: "text".into(),
            resume_args: vec![],
            fork_args: vec![],
            model_args: vec![],
            program: crate::deck::config::which("sh"),
        });
        let prof = |name: &str| config::ProfileCfg {
            name: name.into(),
            runner: Some("env".into()),
            env: [("INDRA_TOK".to_string(), "secret:tok".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        settings.profiles.push(prof("deploy"));
        settings.profiles.push(prof("other"));
        let app = App::new(settings, TOKEN.into(), true);
        let origin = ("origin", "http://127.0.0.1:7878");
        const VALUE: &str = "sk-live-0123456789";

        let launch = |profile: &'static str| {
            let app = app.clone();
            async move {
                call(
                    &app,
                    "POST",
                    "/api/procs",
                    &[AUTH, origin],
                    Some(json!({ "profile": profile, "prompt": "x", "target": 0 })),
                )
                .await
            }
        };
        // Not set yet: refused before anything starts.
        let (s, _, body) = launch("deploy").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(
            body.contains("secret 'tok'") && body.contains("is not set"),
            "{}",
            body
        );

        let (s, _, body) = call(
            &app,
            "PUT",
            "/api/secrets/tok",
            &[AUTH, origin],
            Some(json!({ "value": VALUE, "description": "deploy token", "profiles": ["deploy"] })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let (_, _, body) = call(&app, "GET", "/api/secrets", &[AUTH], None).await;
        assert!(
            body.contains("deploy token") && !body.contains(VALUE),
            "never listed: {}",
            body
        );

        // The allowed profile gets it; its output shows only the name.
        let (s, _, body) = launch("deploy").await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        let v = wait_done(&app, id).await;
        assert_eq!(v["status"], "exited", "{}", v);
        let (_, _, view) = call(&app, "GET", &format!("/api/procs/{}", id), &[AUTH], None).await;
        assert!(!view.contains(VALUE), "{}", view);
        assert!(
            view.contains("secret:tok"),
            "the process shows the reference: {}",
            view
        );
        let (events, _) = app.procs.get(id).unwrap().subscribe(0);
        let feed: Vec<&str> = events.iter().map(|e| e.text.as_str()).collect();
        assert!(feed.contains(&"token=[secret:tok]"), "{:?}", feed);
        let log = std::fs::read_to_string(v["log"].as_str().unwrap()).unwrap();
        assert!(
            log.contains("token=[secret:tok]") && !log.contains(VALUE),
            "{}",
            log
        );
        let (_, _, body) = call(&app, "GET", "/api/secrets", &[AUTH], None).await;
        assert!(body.contains("\"uses\":1"), "{}", body);

        // Another profile may not have it.
        let (s, _, body) = launch("other").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(body.contains("may only be used by"), "{}", body);

        let (s, _, _) = call(&app, "DELETE", "/api/secrets/tok", &[AUTH, origin], None).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _, _) = call(
            &app,
            "PUT",
            "/api/secrets/..%2Fx",
            &[AUTH, origin],
            Some(json!({ "value": "v" })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (ro, ro_dir) = fixture("secrets-ro", true);
        let (s, _, _) = call(
            &ro,
            "PUT",
            "/api/secrets/tok",
            &[AUTH, origin],
            Some(json!({ "value": "v" })),
        )
        .await;
        assert_eq!(
            s,
            StatusCode::FORBIDDEN,
            "read-only consoles cannot change secrets"
        );
        let _ = std::fs::remove_dir_all(ro_dir);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pivot_continues_the_session_or_starts_fresh_with_context() {
        use std::os::unix::fs::PermissionsExt;
        let (base, dir) = fixture("pivot", false);
        // A stand-in for Claude Code: records its argv, reports a session and
        // a result, then records what it was sent on stdin (the prompt arrives
        // there, as stream-json) until indra closes it after the result.
        let script = dir.join("fake-claude");
        let argv = dir.join("argv.log");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '--call--' \"$@\" >> '{log}'\n\
                 echo '{{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"m\",\"session_id\":\"sess-123\"}}'\n\
                 echo '{{\"type\":\"result\",\"subtype\":\"success\",\"num_turns\":1,\"duration_ms\":5,\"result\":\"found 2 issues in auth\"}}'\n\
                 cat >> '{log}'\n",
                log = argv.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut settings = (*base.settings()).clone();
        settings.runners.push(Runner {
            name: "fake".into(),
            command: script.display().to_string(),
            args: vec!["-p".into(), "{prompt}".into()],
            format: "claude-stream-json".into(),
            resume_args: vec!["--resume".into(), "{session}".into()],
            fork_args: vec!["--fork-session".into()],
            model_args: vec![],
            program: Some(script.clone()),
        });
        let app = App::new(settings, TOKEN.into(), true);
        let origin = ("origin", "http://127.0.0.1:7878");

        let (s, _, body) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "fake", "prompt": "review it", "target": 0 })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let first = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        let v = wait_done(&app, first).await;
        assert_eq!(v["session"], "sess-123");

        // Continue: same session, new direction.
        let (s, _, body) = call(
            &app,
            "POST",
            &format!("/api/procs/{}/pivot", first),
            &[AUTH, origin],
            Some(json!({ "prompt": "now focus on the JWT code" })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let r: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(r["mode"], "continue");
        let second = r["id"].as_u64().unwrap();
        let v = wait_done(&app, second).await;
        assert_eq!(v["parent"], first);
        assert_eq!(v["resumed"], "sess-123");
        let log = std::fs::read_to_string(&argv).unwrap();
        assert!(
            log.contains("--call--\n-p\n--resume\nsess-123\n--input-format\nstream-json\n"),
            "{}",
            log
        );
        assert!(
            log.contains("\"content\":\"now focus on the JWT code\""),
            "{}",
            log
        );

        // Fresh: a new session, given the previous outcome as context.
        let (s, _, body) = call(
            &app,
            "POST",
            &format!("/api/procs/{}/pivot", first),
            &[AUTH, origin],
            Some(json!({ "prompt": "write the fix plan", "mode": "fresh" })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let third = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        let v = wait_done(&app, third).await;
        assert!(v["resumed"].is_null());
        let log = std::fs::read_to_string(&argv).unwrap();
        let last = log.rsplit("--call--").next().unwrap();
        assert!(
            last.contains("write the fix plan")
                && last.contains("found 2 issues in auth")
                && last.contains("review it"),
            "{}",
            last
        );
        assert!(!last.contains("--resume"));

        // Refusals.
        let (s, _, _) = call(
            &app,
            "POST",
            &format!("/api/procs/{}/pivot", first),
            &[AUTH, origin],
            Some(json!({ "prompt": "x", "mode": "sideways" })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (_, _, body) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "echo", "prompt": "plain", "target": 0 })),
        )
        .await;
        let plain = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        wait_done(&app, plain).await;
        let (s, _, body) = call(
            &app,
            "POST",
            &format!("/api/procs/{}/pivot", plain),
            &[AUTH, origin],
            Some(json!({ "prompt": "x" })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(body.contains("fresh start"), "{}", body);
        let (s, _, _) = call(
            &app,
            "POST",
            "/api/procs/999/pivot",
            &[AUTH, origin],
            Some(json!({ "prompt": "x" })),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_pivot_leaves_the_agent_running() {
        let (app, dir) = fixture("pivot-refused", false);
        let sleep = Runner {
            name: "sleep".into(),
            command: "sleep".into(),
            args: vec!["{prompt}".into()],
            format: "text".into(),
            resume_args: vec![],
            fork_args: vec![],
            model_args: vec![],
            program: crate::deck::config::which("sleep"),
        };
        let h = &app.settings().harnesses[0];
        let id = app
            .procs
            .start(Launch::Local {
                runner: &sleep,
                harness: "t",
                prompt: "30",
                target: &h.targets[0],
            })
            .unwrap();
        let origin = ("origin", "http://127.0.0.1:7878");
        let (s, _, body) = call(
            &app,
            "POST",
            &format!("/api/procs/{}/pivot", id),
            &[AUTH, origin],
            Some(json!({ "prompt": "new direction" })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(body.contains("cannot continue a session"), "{}", body);
        assert!(
            app.procs.get(id).unwrap().is_running(),
            "checks happen before the agent is stopped"
        );
        app.procs.get(id).unwrap().request_stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_kills_a_running_process() {
        let (app, dir) = fixture("stop", false);
        let sleep = Runner {
            name: "sleep".into(),
            command: "sleep".into(),
            args: vec!["{prompt}".into()],
            format: "text".into(),
            resume_args: vec![],
            fork_args: vec![],
            model_args: vec![],
            program: crate::deck::config::which("sleep"),
        };
        let h = &app.settings().harnesses[0];
        let id = app
            .procs
            .start(Launch::Local {
                runner: &sleep,
                harness: "t",
                prompt: "30",
                target: &h.targets[0],
            })
            .unwrap();
        let origin = ("origin", "http://127.0.0.1:7878");
        let (_, _, body) = call(
            &app,
            "POST",
            &format!("/api/procs/{}/stop", id),
            &[AUTH, origin],
            None,
        )
        .await;
        assert!(body.contains("\"stopping\":true"), "{}", body);
        let started = std::time::Instant::now();
        while app.procs.get(id).unwrap().is_running() {
            assert!(started.elapsed().as_secs() < 8, "process did not stop");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(app.procs.get(id).unwrap().view().status, "stopped");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_terminal_is_off_unless_enabled_and_checks_what_it_starts() {
        let (app, dir) = fixture("term", false);
        let check = |q: &str| format!("/api/term/check?{}", q);
        // Off by default.
        let (st, _, body) = call(&app, "GET", &check("what=shell"), &[AUTH], None).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{}", body);
        assert!(body.contains("--shell"));
        let (_, _, ov) = call(&app, "GET", "/api/overview", &[AUTH], None).await;
        assert_eq!(serde_json::from_str::<Value>(&ov).unwrap()["shell"], false);
        app.shell.store(true, std::sync::atomic::Ordering::SeqCst);
        let (_, _, ov) = call(&app, "GET", "/api/overview", &[AUTH], None).await;
        assert_eq!(serde_json::from_str::<Value>(&ov).unwrap()["shell"], true);
        // A token is still needed.
        assert_eq!(
            call(&app, "GET", &check("what=shell"), &[], None).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (st, _, body) = call(&app, "GET", &check("what=shell&target=0"), &[AUTH], None).await;
        assert_eq!(st, StatusCode::OK, "{}", body);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v["cwd"].as_str().unwrap().ends_with("target"));
        assert_eq!(
            call(&app, "GET", &check("what=shell&target=7"), &[AUTH], None)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(&app, "GET", &check("what=rm"), &[AUTH], None).await.0,
            StatusCode::BAD_REQUEST
        );
        // No Claude Code runner in this fixture.
        let (st, _, body) = call(&app, "GET", &check("what=claude"), &[AUTH], None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{}", body);
        assert_eq!(
            call(&app, "GET", &check("what=agent&proc=99"), &[AUTH], None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        // The page gets its own CSP; the console keeps the strict one.
        let (_, h, _) = call(&app, "GET", "/term", &[], None).await;
        let csp = h["content-security-policy"].to_str().unwrap();
        assert!(
            csp.contains("style-src 'self' 'unsafe-inline'") && csp.contains("script-src 'self';")
        );
        let (_, h, _) = call(&app, "GET", "/", &[], None).await;
        assert!(!h["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("unsafe-inline"));
        // Never beyond loopback, never read-only.
        let (ro, dir2) = fixture("term-ro", true);
        ro.shell.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(!ro.shell_on());
        let wide = App::new((*app.settings()).clone(), TOKEN.into(), false);
        wide.shell.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(!wide.shell_on());
        // WebSockets skip CORS, so the terminal wants its own origin.
        let hm = |o: Option<&str>| {
            let mut m = HeaderMap::new();
            m.insert(header::HOST, "127.0.0.1:7878".parse().unwrap());
            if let Some(o) = o {
                m.insert(header::ORIGIN, o.parse().unwrap());
            }
            m
        };
        assert!(same_origin(&hm(Some("http://127.0.0.1:7878"))));
        assert!(!same_origin(&hm(Some("http://evil.example"))));
        assert!(!same_origin(&hm(Some("http://127.0.0.1:7879"))));
        assert!(!same_origin(&hm(None)));
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_browser_terminal_runs_a_real_shell() {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as WsMsg};
        let (app, dir) = fixture("pty", false);
        app.shell.store(true, std::sync::atomic::Ordering::SeqCst);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = router(app.clone());
        tokio::spawn(async move { axum::serve(listener, served).await });
        let open = |origin: &str| {
            let mut r = format!("ws://{}/api/term?what=shell&target=0", addr)
                .into_client_request()
                .unwrap();
            let h = r.headers_mut();
            h.insert("origin", origin.parse().unwrap());
            h.insert("authorization", AUTH.1.parse().unwrap());
            r
        };
        // Another site's page is refused.
        assert!(
            tokio_tungstenite::connect_async(open("http://evil.example"))
                .await
                .is_err()
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(open(&format!("http://{}", addr)))
            .await
            .unwrap();
        ws.send(WsMsg::Text(r#"{"t":"resize","cols":91,"rows":27}"#.into()))
            .await
            .unwrap();
        ws.send(WsMsg::Binary(
            b"echo indra-$((40+2)); stty size; pwd\n".to_vec().into(),
        ))
        .await
        .unwrap();
        let mut seen = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !(seen.contains("indra-42") && seen.contains("27 91") && seen.contains("/target")) {
            assert!(std::time::Instant::now() < deadline, "got: {}", seen);
            match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(WsMsg::Binary(b)))) => seen.push_str(&String::from_utf8_lossy(&b)),
                Ok(Some(Ok(_))) => {}
                other => panic!(
                    "terminal closed early: {:?}; got: {}",
                    other.map(|x| x.map(|y| y.is_ok())),
                    seen
                ),
            }
        }
        ws.send(WsMsg::Binary(b"exit\n".to_vec().into()))
            .await
            .unwrap();
        while !seen.contains("ended") {
            assert!(std::time::Instant::now() < deadline, "got: {}", seen);
            match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(WsMsg::Binary(b)))) => seen.push_str(&String::from_utf8_lossy(&b)),
                Ok(Some(Ok(_))) => {}
                _ => break,
            }
        }
        assert!(seen.contains("ended"), "got: {}", seen);
        let log = std::fs::read_to_string(dir.join("data/logs/terminal.log")).unwrap();
        assert!(log.contains("\"what\":\"shell in "));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The fixture, with a memory vault (`vault/` under its root) on its harness.
    fn fixture_with_memory(name: &str, read_only: bool) -> (Arc<App>, PathBuf) {
        let (base, dir) = fixture(name, read_only);
        let mut settings = (*base.settings()).clone();
        settings.harnesses[0].memory = vec!["vault".into()];
        (App::new(settings, TOKEN.into(), true), dir)
    }

    #[tokio::test]
    async fn knowledge_is_managed_from_the_console() {
        let (app, dir) = fixture_with_memory("knowledge", false);
        let origin = ("origin", "http://127.0.0.1:7878");
        let get = |uri: String| {
            let app = app.clone();
            async move {
                let (s, _, b) = call(&app, "GET", &uri, &[AUTH], None).await;
                (s, serde_json::from_str::<Value>(&b).unwrap_or(Value::Null))
            }
        };
        let send = |method: &'static str, uri: &'static str, body: Value| {
            let app = app.clone();
            async move {
                let (s, _, b) = call(&app, method, uri, &[AUTH, origin], Some(body)).await;
                (s, serde_json::from_str::<Value>(&b).unwrap_or(Value::Null))
            }
        };
        let (_, k) = get("/api/knowledge".into()).await;
        assert_eq!(k["vaults"][0]["exists"], false);
        assert_eq!(
            send("POST", "/api/knowledge/init", json!({})).await.0,
            StatusCode::OK
        );
        let (s, a) = send(
            "POST",
            "/api/knowledge/notes",
            json!({ "title": "Session fixation", "body": "Login kept the old session id.", "type": "finding", "tags": ["auth"] }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", a);
        let a = a["id"].as_str().unwrap().to_string();
        let (_, b) = send(
            "POST",
            "/api/knowledge/notes",
            json!({ "title": "Rotate the session", "body": "Rotate after login.", "links": ["Session fixation"] }),
        )
        .await;
        let b = b["id"].as_str().unwrap().to_string();
        send(
            "POST",
            "/api/knowledge/notes",
            json!({ "title": "Lonely", "body": "x" }),
        )
        .await;
        // A duplicate title is refused rather than overwritten.
        assert_eq!(
            send("POST", "/api/knowledge/notes", json!({ "title": "Lonely" }))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let (_, found) = get("/api/knowledge/notes?q=fixation".into()).await;
        assert_eq!(found["notes"][0]["id"], a.as_str());
        assert_eq!(found["total"], 3);
        // Edit with optimistic concurrency.
        let (_, n) = get(format!("/api/knowledge/note?id={}", a)).await;
        let text = n["text"]
            .as_str()
            .unwrap()
            .replace("old session id", "old session id (fixed)");
        let stale = json!({ "id": a, "text": text, "hash": "0000" });
        assert_eq!(
            send("PUT", "/api/knowledge/note", stale).await.0,
            StatusCode::CONFLICT
        );
        let ok = json!({ "id": a, "text": text, "hash": n["hash"] });
        assert_eq!(
            send("PUT", "/api/knowledge/note", ok).await.0,
            StatusCode::OK
        );
        let (_, n) = get(format!("/api/knowledge/note?id={}", a)).await;
        assert!(n["body"].as_str().unwrap().contains("(fixed)"));
        assert_eq!(n["relations"][0]["dir"], "in");
        // Links: once, then a no-op.
        let link = json!({ "from": b, "to": "Session fixation", "rel": "mitigates" });
        assert_eq!(
            send("POST", "/api/knowledge/link", link.clone()).await.1["changed"],
            true
        );
        assert_eq!(
            send("POST", "/api/knowledge/link", link).await.1["changed"],
            false
        );
        let (_, h) = get("/api/knowledge/health".into()).await;
        assert_eq!(h["orphans"][0]["title"], "Lonely");
        // What an agent would be given for a prompt.
        let (_, r) = get("/api/knowledge/recall?q=fix%20the%20session%20fixation".into()).await;
        assert_eq!(r["notes"][0]["id"], a.as_str());
        assert!(r["text"].as_str().unwrap().contains("Relevant memory"));
        let (s, _, _) = call(
            &app,
            "DELETE",
            "/api/knowledge/note?id=lonely",
            &[AUTH, origin],
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        // Learnings: proposed, edited, accepted into memory once.
        let (_, l) = send(
            "POST",
            "/api/learnings",
            json!({ "title": "Tests need Postgres", "body": "Run make db.", "tags": ["#Tests"] }),
        )
        .await;
        let lid = l["id"].as_u64().unwrap();
        let (_, ov) = get("/api/overview".into()).await;
        assert_eq!(ov["learnings"], 1);
        let (s, _, _) = call(
            &app,
            "PATCH",
            &format!("/api/learnings/{}", lid),
            &[AUTH, origin],
            Some(json!({ "body": "Run make db first." })),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let accept = format!("/api/learnings/{}/accept", lid);
        let (s, _, body) = call(
            &app,
            "POST",
            &accept,
            &[AUTH, origin],
            Some(json!({ "links": ["Rotate the session"] })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let (s, _, _) = call(&app, "POST", &accept, &[AUTH, origin], Some(json!({}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (_, n) = get("/api/knowledge/note?id=tests-need-postgres".into()).await;
        assert_eq!(n["type"], "learning");
        assert_eq!(n["tags"][0], "tests");
        assert!(n["body"].as_str().unwrap().contains("Run make db first."));
        assert_eq!(n["source"], "indra console");
        let (_, ls) = get("/api/learnings".into()).await;
        assert_eq!(ls[0]["status"], "accepted");
        // Read-only: nothing changes.
        let (ro, dir2) = fixture_with_memory("knowledge-ro", true);
        let (s, _, _) = call(
            &ro,
            "POST",
            "/api/knowledge/init",
            &[AUTH, origin],
            Some(json!({})),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _, _) = call(
            &ro,
            "POST",
            "/api/learnings",
            &[AUTH, origin],
            Some(json!({ "title": "x" })),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    #[cfg(unix)]
    const REFLECTOR: &str = r#"#!/usr/bin/env python3
import json, os, sys
log = open(os.environ["FAKE_REFLECT_LOG"], "a")
def out(o):
    print(json.dumps(o), flush=True)
def recv():
    l = sys.stdin.readline()
    if not l:
        sys.exit(0)
    log.write("IN " + l); log.flush()
    return json.loads(l)
m = recv()
out({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {}}})
while True:
    m = recv()
    if m["type"] != "user":
        continue
    c = m["message"]["content"]
    if "Before you finish" in c:
        ans = '[{"title": "Use make db", "body": "The tests need Postgres.", "tags": ["tests"]}]'
        out({"type": "assistant", "message": {"content": [{"type": "text", "text": ans}], "usage": {"input_tokens": 10, "cache_read_input_tokens": 3000, "output_tokens": 40}}})
        out({"type": "result", "subtype": "success", "num_turns": 1, "duration_ms": 5, "result": ans, "modelUsage": {"m": {"contextWindow": 123456}}})
    else:
        out({"type": "system", "subtype": "init", "model": "m", "session_id": "s-9"})
        out({"type": "assistant", "message": {"content": [{"type": "text", "text": "done"}], "usage": {"input_tokens": 1000, "cache_read_input_tokens": 2000, "output_tokens": 50}}, "session_id": "s-9"})
        out({"type": "result", "subtype": "success", "num_turns": 1, "duration_ms": 5, "result": "done", "session_id": "s-9"})
"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn agents_start_with_memory_and_propose_learnings() {
        use std::os::unix::fs::PermissionsExt;
        let (base, dir) = fixture_with_memory("reflect", false);
        let vault = dir.join("vault");
        crate::mem::vault::init(&vault).unwrap();
        crate::mem::cli::add_note(
            &vault,
            &crate::mem::cli::NewNote {
                title: "Session handling lives in auth".into(),
                body: "Sessions are created in src/auth/session.rs.".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let script = dir.join("reflector.py");
        let log = dir.join("reflect.log");
        std::fs::write(&script, REFLECTOR).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("FAKE_REFLECT_LOG", &log);
        let mut settings = (*base.settings()).clone();
        settings.runners.push(Runner {
            name: "reflector".into(),
            command: script.display().to_string(),
            args: vec![
                "-p".into(),
                "{prompt}".into(),
                "--output-format".into(),
                "stream-json".into(),
            ],
            format: "claude-stream-json".into(),
            resume_args: vec![],
            fork_args: vec![],
            model_args: vec![],
            program: Some(script.clone()),
        });
        let app = App::new(settings, TOKEN.into(), true);
        let origin = ("origin", "http://127.0.0.1:7878");
        let (s, _, body) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "reflector", "prompt": "fix the session handling", "target": 0, "memory": true, "reflect": true })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        let mut v = json!(null);
        for _ in 0..400 {
            v = json!(app.procs.get(id).unwrap().view());
            if v["status"] != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(v["status"], "exited", "{}", v);
        assert_eq!(v["reflection"], "received");
        assert_eq!(v["memory"][0]["title"], "Session handling lives in auth");
        assert_eq!(v["context_tokens"], 3050);
        assert_eq!(v["context_window"], 123456);
        let sent = std::fs::read_to_string(&log).unwrap();
        // The prompt comes first, then the memory; the last turn asks for learnings.
        let p = sent.find("fix the session handling").unwrap();
        let m = sent.find("Relevant memory").unwrap();
        assert!(
            p < m && sent.contains("src/auth/session.rs") && sent.contains("Before you finish")
        );
        crate::deck::knowledge::harvest(&app);
        crate::deck::knowledge::harvest(&app); // once only
        let l = app.learnings.list();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].title, "Use make db");
        assert_eq!(l[0].origin.proc, Some(id));
        assert_eq!(l[0].origin.session.as_deref(), Some("s-9"));
        assert_eq!(l[0].origin.by, "agent");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn vaults_learnings_and_definitions_move_in_and_out() {
        let (app, dir) = fixture_with_memory("transfer", false);
        let origin = ("origin", "http://127.0.0.1:7878");
        let send = |method: &'static str, uri: String, body: Option<Value>| {
            let app = app.clone();
            async move {
                let (s, h, b) = call(&app, method, &uri, &[AUTH, origin], body).await;
                (s, h, b)
            }
        };
        // Import a small Markdown folder, previewing first.
        let files = json!([
            { "path": "kb/Auth.md", "text": "# Auth\n\nSee [Sessions](Sessions.md).\n" },
            { "path": "kb/Sessions.md", "text": "# Sessions\n\nRotate on login.\n" },
            { "path": "../escape.md", "text": "x" }
        ]);
        let (s, _, b) = send(
            "POST",
            "/api/knowledge/import".into(),
            Some(json!({ "files": files, "dry_run": true })),
        )
        .await;
        assert_eq!(
            s,
            StatusCode::BAD_REQUEST,
            "a path outside the upload is refused: {}",
            b
        );
        let files = json!([
            { "path": "kb/Auth.md", "text": "# Auth\n\nSee [Sessions](Sessions.md).\n" },
            { "path": "kb/Sessions.md", "text": "# Sessions\n\nRotate on login.\n" }
        ]);
        let (s, _, b) = send(
            "POST",
            "/api/knowledge/import".into(),
            Some(json!({ "files": files.clone(), "dry_run": true })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let r: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(r["imported"].as_array().unwrap().len(), 2);
        assert!(
            !dir.join("vault").join(".indra").exists()
                || std::fs::read_dir(dir.join("vault")).unwrap().count() <= 1
        );
        let (s, _, b) = send(
            "POST",
            "/api/knowledge/import".into(),
            Some(json!({ "files": files.clone(), "tags": ["kb"] })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        assert_eq!(
            serde_json::from_str::<Value>(&b).unwrap()["links_converted"],
            1
        );
        let (s, _, b) = send(
            "POST",
            "/api/knowledge/import".into(),
            Some(json!({ "files": files })),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<Value>(&b).unwrap()["skipped_existing"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        // Export as a bundle, then import that bundle's notes into another vault.
        let (s, h, b) = send("GET", "/api/knowledge/export?format=bundle".into(), None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h["content-disposition"]
            .to_str()
            .unwrap()
            .contains("attachment"));
        let bundle: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(bundle["format"], crate::deck::knowledge::BUNDLE);
        assert_eq!(bundle["notes"].as_array().unwrap().len(), 2);
        for f in ["html", "json", "mermaid", "dot"] {
            assert_eq!(
                send("GET", format!("/api/knowledge/export?format={}", f), None)
                    .await
                    .0,
                StatusCode::OK
            );
        }
        let (other, dir2) = fixture_with_memory("transfer-2", false);
        let (s, _, b) = call(
            &other,
            "POST",
            "/api/knowledge/import",
            &[AUTH, origin],
            Some(json!({ "files": bundle["notes"], "from": "obsidian" })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let (_, _, b) = call(&other, "GET", "/api/knowledge/notes", &[AUTH], None).await;
        assert_eq!(serde_json::from_str::<Value>(&b).unwrap()["total"], 2);
        // Learnings in bulk.
        let (s, _, b) = send(
            "POST",
            "/api/learnings/import".into(),
            Some(json!({ "items": [{ "title": "One", "body": "a" }, { "title": "", "body": "skipped" }, { "title": "Two", "tags": ["x"] }] })),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(serde_json::from_str::<Value>(&b).unwrap()["added"], 2);
        assert_eq!(app.learnings.list()[0].origin.by, "import");
        // Agents: create, refuse a duplicate or a bad name, delete.
        let new = json!({ "kind": "agent", "name": "gamma", "content": "---\nname: gamma\ndescription: New one\n---\nHi\n" });
        let (s, _, b) = send("POST", "/api/def".into(), Some(new.clone())).await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let id = serde_json::from_str::<Value>(&b).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(dir.join("agents/gamma.md").is_file());
        assert_eq!(
            send("POST", "/api/def".into(), Some(new)).await.0,
            StatusCode::CONFLICT
        );
        let bad_name = json!({ "kind": "agent", "name": "../x", "content": "x" });
        assert_eq!(
            send("POST", "/api/def".into(), Some(bad_name)).await.0,
            StatusCode::BAD_REQUEST
        );
        let skill = json!({ "kind": "skill", "name": "kb-two", "content": "---\nname: kb-two\ndescription: d\n---\nbody\n" });
        assert_eq!(
            send("POST", "/api/def".into(), Some(skill)).await.0,
            StatusCode::OK
        );
        assert!(dir.join("skills/kb-two/SKILL.md").is_file());
        let (s, _, _) = send("DELETE", format!("/api/def?id={}", id), None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(!dir.join("agents/gamma.md").exists());
        let (s, _, _) = send("DELETE", "/api/def?id=skill:kb-two".into(), None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(!dir.join("skills/kb-two").exists());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }
}
