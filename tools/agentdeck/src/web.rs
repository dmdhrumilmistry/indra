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

use crate::config::{Harness, Settings};
use crate::discover::{self, Def};
use crate::procs::{Launch, Procs};
use crate::runs;
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
use std::convert::Infallible;
use std::sync::Arc;

pub struct App {
    pub settings: Settings,
    pub token: String,
    pub procs: Arc<Procs>,
    /// Bound to a loopback address: enforce a loopback Host header.
    pub loopback: bool,
}

const COOKIE: &str = "agentdeck_token";
const INDEX: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const STYLE: &str = include_str!("../ui/style.css");
const FAVICON: &str = include_str!("../ui/favicon.svg");
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

// ---- errors ------------------------------------------------------------------

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
fn not_found(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}
fn forbidden(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, msg.into())
}

type ApiResult<T> = Result<T, ApiError>;

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
                "missing or wrong token: open the URL agentdeck printed when it started".into(),
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
    hd.insert("content-security-policy", HeaderValue::from_static(CSP));
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
    fn harness(&self, name: Option<&str>) -> ApiResult<&Harness> {
        match name.filter(|n| !n.is_empty()) {
            Some(n) => self
                .settings
                .harnesses
                .iter()
                .find(|h| h.name == n)
                .ok_or_else(|| not_found(format!("no harness named '{}'", n))),
            None => self
                .settings
                .harnesses
                .first()
                .ok_or_else(|| not_found("no harness configured; see `agentdeck init`")),
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
    let s = &app.settings;
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "read_only": s.read_only,
        "config": s.config_path.as_ref().map(|p| p.display().to_string()),
        "data_dir": s.data_dir.display().to_string(),
        "harnesses": s.harnesses.iter().map(harness_json).collect::<Vec<_>>(),
        "runners": s.runners.iter().map(|r| json!({
            "name": r.name, "command": r.command, "args": r.args, "format": r.format,
            "available": r.program.is_some(),
            "program": r.program.as_ref().map(|p| p.display().to_string()),
        })).collect::<Vec<_>>(),
        "presets": s.presets,
        "procs": app.procs.list(),
        "running": app.procs.running(),
    }))
}

async fn defs(State(app): State<Arc<App>>, Query(q): Query<HQ>) -> ApiResult<Json<Value>> {
    let h = app.harness(q.h.as_deref())?;
    Ok(Json(json!(discover::discover(h))))
}

#[derive(Deserialize)]
struct DefQ {
    h: Option<String>,
    id: String,
}

async fn def_get(State(app): State<Arc<App>>, Query(q): Query<DefQ>) -> ApiResult<Json<Value>> {
    let h = app.harness(q.h.as_deref())?;
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
    if app.settings.read_only {
        return Err(forbidden("agentdeck is running read-only"));
    }
    if r.content.len() > 1_000_000 {
        return Err(bad("content is larger than 1 MB"));
    }
    let h = app.harness(r.h.as_deref())?;
    let d = def_by_id(h, &r.id)?;
    let current = discover::read_body(&d).map_err(|e| not_found(e.to_string()))?;
    if fingerprint(&current) != r.hash {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "the file changed on disk since you opened it; reload to see the new version".into(),
        ));
    }
    memex::vault::write_atomic(&d.abs, &r.content)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(
        json!({ "saved": d.path, "hash": fingerprint(&r.content) }),
    ))
}

async fn graph(State(app): State<Arc<App>>, Query(q): Query<HQ>) -> ApiResult<Json<Value>> {
    let h = app.harness(q.h.as_deref())?;
    let defs = discover::discover(h);
    Ok(Json(json!(discover::graph(h, &defs))))
}

async fn memory(State(app): State<Arc<App>>, Query(q): Query<HQ>) -> ApiResult<Json<Value>> {
    let h = app.harness(q.h.as_deref())?;
    let list: Vec<Value> = h
        .memory_paths()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (notes, links) = match memex::vault::Vault::open(p) {
                Ok(v) if p.is_dir() => {
                    let g = memex::graph::Graph::build(&v);
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
    let h = app.harness(q.h.as_deref())?;
    let p = h
        .memory_paths()
        .get(q.i)
        .cloned()
        .ok_or_else(|| not_found("no such memory vault"))?;
    let v = memex::vault::Vault::open(&p).map_err(not_found)?;
    let g = memex::graph::Graph::build(&v);
    Ok(Json(json!(memex::export::data(&v, &g, true))))
}

async fn runs_list(State(app): State<Arc<App>>, Query(q): Query<HQ>) -> ApiResult<Json<Value>> {
    let h = app.harness(q.h.as_deref())?;
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
    let h = app.harness(q.h.as_deref())?;
    let dir = runs::find(h, &q.key).ok_or_else(|| not_found("no such run"))?;
    Ok(Json(json!(runs::summarize(h, &q.key, &dir, true))))
}

async fn run_file(State(app): State<Arc<App>>, Query(q): Query<RunQ>) -> ApiResult<Json<Value>> {
    let h = app.harness(q.h.as_deref())?;
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
    runner: String,
    prompt: String,
    target: usize,
}

async fn proc_start(
    State(app): State<Arc<App>>,
    Json(r): Json<LaunchReq>,
) -> ApiResult<Json<Value>> {
    if app.settings.read_only {
        return Err(forbidden("agentdeck is running read-only"));
    }
    let prompt = r.prompt.trim();
    if prompt.is_empty() || prompt.len() > 20_000 {
        return Err(bad("the prompt must be 1 to 20000 characters"));
    }
    let h = app.harness(r.h.as_deref())?;
    let runner = app
        .settings
        .runners
        .iter()
        .find(|x| x.name == r.runner)
        .ok_or_else(|| bad("unknown runner"))?;
    let target = h
        .targets
        .get(r.target)
        .ok_or_else(|| bad("unknown target"))?;
    let id = app
        .procs
        .start(Launch {
            runner,
            harness: &h.name,
            prompt,
            target,
        })
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
    if app.settings.read_only {
        return Err(forbidden("agentdeck is running read-only"));
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
    let to_sse = |e: &crate::procs::Event| {
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
        .route("/api/overview", get(overview))
        .route("/api/defs", get(defs))
        .route("/api/def", get(def_get).put(def_put))
        .route("/api/graph", get(graph))
        .route("/api/memory", get(memory))
        .route("/api/memory/graph", get(memory_graph))
        .route("/api/runs", get(runs_list))
        .route("/api/run", get(run_get))
        .route("/api/run/file", get(run_file))
        .route("/api/procs", get(procs_list).post(proc_start))
        .route("/api/procs/{id}", get(proc_get))
        .route("/api/procs/{id}/stop", post(proc_stop))
        .route("/api/procs/{id}/events", get(proc_events))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Harness, Runner, StageCfg};
    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn fixture(name: &str, read_only: bool) -> (Arc<App>, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("agentdeck-web-{}-{}", std::process::id(), name));
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
            program: crate::config::which("echo"),
        };
        let settings = Settings {
            harnesses: vec![h],
            runners: vec![echo],
            presets: vec![],
            bind: "127.0.0.1:0".into(),
            read_only,
            config_path: None,
            data_dir: dir.join("data"),
        };
        let app = Arc::new(App {
            settings,
            token: TOKEN.into(),
            procs: Procs::new(dir.join("data/logs")),
            loopback: true,
        });
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
    async fn stop_kills_a_running_process() {
        let (app, dir) = fixture("stop", false);
        let sleep = Runner {
            name: "sleep".into(),
            command: "sleep".into(),
            args: vec!["{prompt}".into()],
            format: "text".into(),
            program: crate::config::which("sleep"),
        };
        let h = &app.settings.harnesses[0];
        let id = app
            .procs
            .start(Launch {
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
}
