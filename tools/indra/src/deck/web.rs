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
use std::collections::{BTreeMap, HashMap};
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
    /// Plan-and-delegate runs: a planner's plan carried out by workers.
    pub delegations: crate::deck::delegate::Delegations,
    /// Plugin and skill marketplaces, and what was installed from them.
    pub market: crate::deck::market::Market,
    /// Console users and their sessions (none: the token is the key).
    pub users: crate::deck::users::Users,
    /// Enrollment keys: many machines join with one (MDM roll-outs).
    pub enroll: crate::deck::enroll::Keys,
    /// Engagements: tests of live systems inside a written scope.
    pub engagements: crate::deck::engage::Store,
}

/// What the console needs to edit the config file and apply the result.
pub struct ConfigAccess {
    /// How the settings were loaded, to load them again after an edit.
    pub opts: config::LoadOptions,
    /// The file edits are written to (it may not exist yet).
    pub path: std::path::PathBuf,
    /// Why editing is turned off, if it is.
    pub locked: Option<String>,
    /// Why machines cannot add themselves with a join code (read-only, or
    /// --lock-config). Listening on the network does not stop that: a
    /// joining machine adds only its name.
    pub join_locked: Option<String>,
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
        let market = crate::deck::market::Market::open(&settings.data_dir);
        let users = crate::deck::users::Users::open(&settings.data_dir);
        let enroll = crate::deck::enroll::Keys::open(&settings.data_dir);
        let engagements = crate::deck::engage::Store::open(&settings.data_dir);
        let data_dir = settings.data_dir.clone();
        let shared = config::Shared::new(settings);
        procs.set_accounts(Arc::new(crate::deck::accounts::Accounts::new(
            shared.clone(),
            data_dir,
        )));
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
            delegations: Default::default(),
            market,
            users,
            enroll,
            engagements,
        })
    }

    /// The settings now. Take one snapshot per request: an edit in the
    /// console replaces them.
    pub fn settings(&self) -> Arc<Settings> {
        self.settings.get()
    }
}

const COOKIE: &str = "indra_token";
/// A signed-in user's session (once users exist).
const SESSION: &str = "indra_session";
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
/// Nerd Fonts' symbols (v3.4.0, MIT): the icons shell prompts draw
/// (Powerlevel10k, Starship, oh-my-posh), as a fallback after the text font.
const NERD_FONT: &[u8] = include_bytes!("../../ui/vendor/symbols-nerd-font-mono.woff2");
const NERD_FONT_LICENSE: &str = include_str!("../../ui/vendor/LICENSE-nerd-fonts");
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

fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    for c in h.get_all(header::COOKIE) {
        let Ok(c) = c.to_str() else { continue };
        for part in c.split(';') {
            if let Some(v) = part.trim().strip_prefix(&format!("{}=", name)) {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Who sent a request: a signed-in user, or the access token (an admin
/// credential; once users exist, only as `Authorization: Bearer`).
fn identify(app: &App, h: &HeaderMap) -> Option<crate::deck::users::Who> {
    use crate::deck::users::{Role, Who};
    if let Some(w) = cookie(h, SESSION).and_then(|t| app.users.session(&t)) {
        return Some(w);
    }
    let bearer = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string());
    let token = if app.users.any() {
        bearer
    } else {
        presented_token(h)
    };
    token
        .filter(|t| ct_eq(t.as_bytes(), app.token.as_bytes()))
        .map(|_| Who {
            user: "token".into(),
            role: Role::Admin,
        })
}

/// The person behind a request (set by the guard).
pub(crate) fn who(h: &axum::http::Extensions) -> crate::deck::users::Who {
    h.get::<crate::deck::users::Who>()
        .cloned()
        .unwrap_or(crate::deck::users::Who {
            user: "token".into(),
            role: crate::deck::users::Role::Admin,
        })
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

async fn guard(State(app): State<Arc<App>>, mut req: Request, next: Next) -> Response {
    let h = req.headers();
    if app.loopback && !host_name(h).map(|n| is_loopback_host(&n)).unwrap_or(false) {
        return forbidden("unexpected Host header").into_response();
    }
    let path = req.uri().path().to_string();
    let mut audit: Option<crate::deck::users::Who> = None;
    if path.starts_with("/api/") {
        // Signing in and out, and asking who you are, need no one yet.
        let open = matches!(path.as_str(), "/api/session" | "/api/login" | "/api/logout");
        let who = identify(&app, h);
        if !open {
            let Some(who) = who.clone() else {
                let msg = if app.users.any() {
                    "sign in first"
                } else {
                    "missing or wrong token: open the URL `indra serve` printed when it started"
                };
                return ApiError(StatusCode::UNAUTHORIZED, msg.into()).into_response();
            };
            let need = crate::deck::users::needed(req.method().as_str(), &path);
            if who.role < need {
                return forbidden(format!(
                    "{} is a {} here; this needs {}",
                    who.user,
                    who.role.as_str(),
                    need.as_str()
                ))
                .into_response();
            }
        }
        if let Some(w) = who {
            if req.method() != Method::GET && req.method() != Method::HEAD {
                audit = Some(w.clone());
            }
            req.extensions_mut().insert(w);
        }
        let h = req.headers();
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
    let method = req.method().to_string();
    let mut res = next.run(req).await;
    // Who changed what: one line per state-changing call.
    if let Some(w) = audit {
        let line = json!({
            "at": crate::deck::runs::now(), "user": w.user, "role": w.role.as_str(),
            "method": method, "path": path, "status": res.status().as_u16(),
        });
        let p = app.settings().data_dir.join("logs").join("audit.jsonl");
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
        {
            use std::io::Write;
            let _ = writeln!(f, "{}", line);
        }
    }
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

// ---- users and sessions ------------------------------------------------------------

fn session_cookie(h: &HeaderMap, value: &str, max_age: Option<u64>) -> String {
    // Secure when the console is reached over HTTPS (directly or behind a proxy).
    let https = h
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
        || h.get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(|o| o.starts_with("https://"))
            .unwrap_or(false);
    let mut c = format!("{}={}; HttpOnly; SameSite=Strict; Path=/", SESSION, value);
    if let Some(a) = max_age {
        c.push_str(&format!("; Max-Age={}", a));
    }
    if https {
        c.push_str("; Secure");
    }
    c
}

/// Who you are, and whether this console has users (the login page asks).
async fn session_get(State(app): State<Arc<App>>, headers: HeaderMap) -> Json<Value> {
    let who = identify(&app, &headers);
    Json(json!({
        "users": app.users.any(),
        "user": who.as_ref().map(|w| w.user.clone()),
        "role": who.as_ref().map(|w| w.role.as_str()),
    }))
}

#[derive(Deserialize)]
struct LoginReq {
    name: String,
    password: String,
}

async fn login(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(r): Json<LoginReq>,
) -> Response {
    if !app.users.any() {
        return bad("this console has no users yet: open the URL `indra serve` printed, then add one in Settings > Users").into_response();
    }
    let (name, password) = (r.name.trim().to_string(), r.password);
    // Argon2 is slow on purpose; keep it off the async workers.
    let a = app.clone();
    let res = tokio::task::spawn_blocking(move || a.users.sign_in(&name, &password))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    match res {
        Ok((token, who)) => (
            [(header::SET_COOKIE, session_cookie(&headers, &token, None))],
            Json(json!({ "user": who.user, "role": who.role.as_str() })),
        )
            .into_response(),
        Err(e) => ApiError(StatusCode::UNAUTHORIZED, e).into_response(),
    }
}

async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(t) = cookie(&headers, SESSION) {
        app.users.sign_out(&t);
    }
    (
        [(header::SET_COOKIE, session_cookie(&headers, "", Some(0)))],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct OwnPasswordReq {
    current: String,
    new: String,
}

/// Change your own password (the current one is asked again).
async fn own_password(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(r): Json<OwnPasswordReq>,
) -> ApiResult<Json<Value>> {
    let who = cookie(&headers, SESSION)
        .and_then(|t| app.users.session(&t))
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "sign in first".into()))?;
    let a = app.clone();
    tokio::task::spawn_blocking(move || {
        a.users
            .sign_in(&who.user, &r.current)
            .map_err(|_| forbidden("the current password is wrong"))?;
        a.users.set_password(&who.user, &r.new).map_err(bad)
    })
    .await
    .map_err(|e| internal(e.to_string()))??;
    Ok(Json(json!({ "ok": true })))
}

async fn users_list(State(app): State<Arc<App>>) -> Json<Value> {
    Json(json!(app
        .users
        .list()
        .into_iter()
        .map(|(n, r, c)| json!({ "name": n, "role": r.as_str(), "created": c }))
        .collect::<Vec<_>>()))
}

#[derive(Deserialize)]
struct UserReq {
    name: Option<String>,
    role: Option<String>,
    password: Option<String>,
}

async fn user_add(State(app): State<Arc<App>>, Json(r): Json<UserReq>) -> ApiResult<Json<Value>> {
    let role =
        crate::deck::users::Role::parse(r.role.as_deref().unwrap_or("viewer")).map_err(bad)?;
    let name = r.name.unwrap_or_default();
    let pw = r.password.unwrap_or_default();
    let a = app.clone();
    tokio::task::spawn_blocking(move || a.users.add(name.trim(), role, &pw))
        .await
        .map_err(|e| internal(e.to_string()))?
        .map_err(bad)?;
    Ok(users_list(State(app)).await)
}

async fn user_update(
    State(app): State<Arc<App>>,
    UrlPath(name): UrlPath<String>,
    Json(r): Json<UserReq>,
) -> ApiResult<Json<Value>> {
    if let Some(role) = r.role.as_deref() {
        let role = crate::deck::users::Role::parse(role).map_err(bad)?;
        app.users.set_role(&name, role).map_err(bad)?;
    }
    if let Some(pw) = r.password.clone() {
        let (a, n) = (app.clone(), name.clone());
        tokio::task::spawn_blocking(move || a.users.set_password(&n, &pw))
            .await
            .map_err(|e| internal(e.to_string()))?
            .map_err(bad)?;
    }
    Ok(users_list(State(app)).await)
}

async fn user_remove(
    State(app): State<Arc<App>>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    app.users.remove(&name).map_err(bad)?;
    Ok(users_list(State(app)).await)
}

// ---- static ------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenQ {
    token: Option<String>,
}

async fn index(State(app): State<Arc<App>>, Query(q): Query<TokenQ>) -> Response {
    // Once users exist, people sign in; a token in a URL no longer does.
    if let Some(t) = q.token.filter(|_| !app.users.any()) {
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

async fn overview(State(app): State<Arc<App>>, req: Request) -> Json<Value> {
    let s = &app.settings();
    let me = who(req.extensions());
    use crate::deck::users::Role;
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "update": crate::update::summary(),
        // A viewer sees the console as read-only.
        "read_only": s.read_only || me.role == Role::Viewer,
        "user": me.user, "role": me.role.as_str(), "users": app.users.any(),
        "is_admin": me.role == Role::Admin,
        "shell": app.shell_on(),
        // The shells a terminal can start (the first is the default).
        "shells": if app.shell_on() && me.role == Role::Admin {
            json!(crate::deck::term::shells(&s.terminal).iter().map(|p| p.display().to_string()).collect::<Vec<_>>())
        } else {
            json!([])
        },
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
        "teams": s.teams,
        "approvals": app.procs.list().iter().map(|p| p.pending.len()).sum::<usize>(),
        "learnings": app.learnings.proposed(),
        "procs": app.procs.list(),
        "nodes": std::iter::once(LOCAL.to_string()).chain(s.nodes.iter().map(|n| n.name.clone())).collect::<Vec<_>>(),
        "workflows": s.workflows.iter().map(|w| w.name.clone()).collect::<Vec<_>>(),
        "running": app.procs.running(),
        "settings": app.config.as_ref().map(|c| json!({
            "path": c.path.display().to_string(), "editable": c.locked.is_none(), "locked": c.locked,
            // Machines can be added and removed with a code (Machines page).
            "join": c.join_locked.is_none() && me.role == Role::Admin, "join_locked": c.join_locked,
        })),
        // Other machines can reach this console (it listens beyond loopback).
        "reachable": !app.loopback,
        // The organization's settings (MDM): what managed.toml enforces.
        "managed": crate::deck::managed::view(),
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

/// A run of the harness, or a copy fetched from a node (`n:<node>:<run>`).
pub(crate) fn find_run(app: &App, h: &Harness, key: &str) -> ApiResult<std::path::PathBuf> {
    runs::find(h, key)
        .or_else(|| runs::fetched_dir(&app.settings().data_dir, key))
        .ok_or_else(|| not_found("no such run"))
}

async fn run_get(State(app): State<Arc<App>>, Query(q): Query<RunQ>) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    let dir = find_run(&app, h, &q.key)?;
    Ok(Json(json!(runs::summarize(h, &q.key, &dir, true))))
}

async fn run_file(State(app): State<Arc<App>>, Query(q): Query<RunQ>) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    let dir = find_run(&app, h, &q.key)?;
    let rel = q.rel.ok_or_else(|| bad("rel is required"))?;
    let p = runs::confined(&dir, &rel)
        .ok_or_else(|| forbidden("that path is not a file inside this run"))?;
    let max = q.max.unwrap_or(256 * 1024).clamp(1024, 4 * 1024 * 1024);
    let (text, truncated) = runs::tail(&p, max).map_err(|e| not_found(e.to_string()))?;
    Ok(Json(
        json!({ "rel": rel, "text": text, "truncated": truncated, "modified": discover::mtime(&p) }),
    ))
}

/// `GET /api/run/surface?key=`: the run's attack surface as a graph, and
/// which of its vulnerabilities an attacker can reach.
async fn run_surface(State(app): State<Arc<App>>, Query(q): Query<RunQ>) -> ApiResult<Json<Value>> {
    let h = &app.harness(q.h.as_deref())?;
    let dir = find_run(&app, h, &q.key)?;
    crate::deck::surface::build(&dir)
        .map(Json)
        .ok_or_else(|| not_found("this run has no codebase map (codebase-map.json) to draw"))
}

#[derive(Deserialize)]
pub(crate) struct LaunchReq {
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
    /// The engagement it runs in (its scope, window and machine apply).
    #[serde(default)]
    engagement: Option<String>,
    /// MCP & tools to give it, on top of those of its profile.
    #[serde(default)]
    tools: Vec<String>,
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
        // Folders set in the console (Machines) replace the harness targets.
        if let Some(list) = local_folders(app) {
            return Ok(list);
        }
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

/// The folders set for this machine on the Machines page, if any.
fn local_folders(app: &App) -> Option<Vec<String>> {
    let p = app.settings().data_dir.join("local-folders.json");
    let list: Vec<String> = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
    (!list.is_empty()).then_some(list)
}

/// WebRTC on or off, and its ICE servers, for nodes that dial in.
fn apply_link_settings(procs: &Procs, s: &Settings) {
    let l = procs.links();
    l.webrtc.store(
        s.webrtc && cfg!(feature = "webrtc"),
        std::sync::atomic::Ordering::SeqCst,
    );
    // A TURN credential may be a secret reference: resolved here, on the hub,
    // and sent to nodes over their authenticated link.
    let ice = s
        .ice_servers
        .iter()
        .map(|x| {
            let mut x = x.clone();
            if crate::deck::secrets::reference(&x.credential).is_some() {
                let mut m = std::collections::BTreeMap::new();
                m.insert("TURN".to_string(), x.credential.clone());
                match procs.vault().resolve(&m, None, LOCAL) {
                    Ok((v, _)) => x.credential = v.get("TURN").cloned().unwrap_or_default(),
                    Err(e) => {
                        eprintln!("indra: TURN server {}: {}", x.urls.join(" "), e);
                        x.credential.clear();
                    }
                }
            }
            x
        })
        .collect();
    *l.ice_servers.lock().unwrap() = ice;
}

fn check_prompt(p: &str) -> ApiResult<&str> {
    let p = p.trim();
    if p.is_empty() || p.len() > 20_000 {
        return Err(bad("the prompt must be 1 to 20000 characters"));
    }
    Ok(p)
}

/// `POST /api/procs`: start an agent. Running one that never asks
/// (bypassPermissions, from the launch or its profile) is as good as a shell
/// on that machine, so it takes an admin, like the browser terminal.
async fn proc_launch(
    State(app): State<Arc<App>>,
    axum::Extension(who): axum::Extension<crate::deck::users::Who>,
    Json(r): Json<LaunchReq>,
) -> ApiResult<Json<Value>> {
    if who.role != crate::deck::users::Role::Admin {
        let settings = app.settings();
        let (_, agent) = resolve_agent(&settings, r.profile.as_deref(), &r.runner, &r.opts)?;
        if agent.permission_mode.as_deref() == Some("bypassPermissions") {
            return Err(forbidden(
                "only admins can start an agent in bypassPermissions mode (it runs everything without asking)",
            ));
        }
    }
    let engagement = r.engagement.clone();
    let prompt = r.prompt.clone();
    let node = r.node.clone().unwrap_or_else(|| LOCAL.to_string());
    let out = proc_start(State(app.clone()), Json(r)).await?;
    if let Some(e) = engagement {
        hub_audit(
            &app,
            &e,
            json!({ "event": "launch", "user": who.user, "proc": out.0["id"], "machine": node,
                    "prompt": crate::mem::search::truncate(&prompt, 2000) }),
        );
    }
    Ok(out)
}

/// The console's own entries in an engagement's audit log: launches,
/// approvals, the end. (Its agents' tool calls are logged by indra guard on
/// the machine they run on.)
fn hub_audit(app: &App, id: &str, entry: Value) {
    if crate::deck::engage::valid_id(id) {
        let mut e = entry;
        e["engagement"] = json!(id);
        let _ = crate::deck::engage::audit(
            &crate::deck::engage::dir_of(&app.settings().data_dir, id),
            e,
        );
    }
}

/// The engagement a launch asks for, checked: it exists, it is active, the
/// agent runs on its machine with a Claude Code runner, and that machine's
/// indra can enforce it.
fn engagement_for(
    app: &App,
    id: &str,
    node: &str,
    runner: Option<&config::Runner>,
) -> ApiResult<crate::deck::engage::Spec> {
    let e = app
        .engagements
        .get(id)
        .ok_or_else(|| bad("no such engagement"))?;
    let t = crate::deck::engage::now();
    if !e.spec.active(t) {
        return Err(bad(format!(
            "the engagement '{}' is not active now (its window is over, or has not started)",
            e.spec.name
        )));
    }
    if e.machine != node {
        return Err(bad(format!(
            "the engagement '{}' runs on {} only",
            e.spec.name, e.machine
        )));
    }
    if let Some(r) = runner {
        if r.format != "claude-stream-json" {
            return Err(bad(
                "agents in an engagement must be Claude Code: its hooks are what enforce the scope",
            ));
        }
    }
    if node != LOCAL {
        // An older indra there would ignore the engagement: refuse.
        let info = app
            .procs
            .links()
            .get(node)
            .and_then(|l| l.info())
            .or_else(|| {
                app.node_cache
                    .lock()
                    .unwrap()
                    .get(node)
                    .and_then(|(_, r)| r.as_ref().ok().map(|i| json!(i)))
            });
        let v = info
            .as_ref()
            .and_then(|i| i["version"].as_str())
            .unwrap_or("0");
        if !version_at_least(v, (0, 13, 0)) {
            return Err(bad(format!(
                "{} runs indra {}, which cannot enforce an engagement: update it there (`indra update`, then `indra node restart`)",
                node, v
            )));
        }
    }
    Ok(e.spec)
}

fn version_at_least(v: &str, min: (u64, u64, u64)) -> bool {
    let n: Vec<u64> = v
        .trim_start_matches('v')
        .split(['.', '-'])
        .take(3)
        .map(|x| x.parse().unwrap_or(0))
        .collect();
    let got = (
        n.first().copied().unwrap_or(0),
        n.get(1).copied().unwrap_or(0),
        n.get(2).copied().unwrap_or(0),
    );
    got >= min
}

pub(crate) async fn proc_start(
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
    let (runner_name, mut agent) =
        resolve_agent(&settings, r.profile.as_deref(), &r.runner, &r.opts)?;
    // Only the console's store sets an engagement, never the request.
    agent.engagement = match r.engagement.as_deref().filter(|e| !e.is_empty()) {
        Some(id) => {
            let runner = if node == LOCAL {
                Some(
                    settings
                        .runners
                        .iter()
                        .find(|x| x.name == runner_name)
                        .ok_or_else(|| bad(format!("unknown runner '{}'", runner_name)))?,
                )
            } else {
                None
            };
            Some(engagement_for(&app, id, node, runner)?)
        }
        None => None,
    };
    // MCP & tools: its profile's, and the ones the launch picked. Only the
    // console's configuration sets MCP servers, never the request.
    agent.mcp_servers = crate::deck::integrations::servers_for(
        &settings.tools,
        r.profile.as_deref().filter(|p| !p.is_empty()),
        &r.tools,
        node == LOCAL,
        settings.config_path.as_deref(),
    )
    .map_err(bad)?;
    // On Ollama here: a model the server cannot load fails now, clearly.
    // (A remote machine checks its own Ollama when the job arrives.)
    if node == LOCAL {
        crate::deck::ollama::ready(&agent).await.map_err(bad)?;
    }
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

/// Run git in `dir` (never through a shell), with a time limit; its output.
async fn git(dir: &str, args: &[&str]) -> Option<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// `GET /api/procs/{id}/workspace`: where an agent on this machine works:
/// its git branch and the files changed there (another machine: not shown).
async fn proc_workspace(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    let p = app
        .procs
        .get(id)
        .ok_or_else(|| not_found("no such process"))?;
    if p.node_name() != LOCAL {
        return Ok(Json(json!({ "local": false })));
    }
    let dir = p.target().to_string();
    let Some(branch) = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await else {
        return Ok(Json(json!({ "local": true, "git": false })));
    };
    let status = git(&dir, &["status", "--porcelain", "--untracked-files=all"])
        .await
        .unwrap_or_default();
    let changed: Vec<Value> = status
        .lines()
        .take(300)
        .filter(|l| l.len() > 3)
        .map(|l| json!({ "status": l[..2].trim(), "path": l[3..].trim_matches('"') }))
        .collect();
    Ok(Json(
        json!({ "local": true, "git": true, "branch": branch.trim(), "changed": changed }),
    ))
}

#[derive(Deserialize)]
struct FileQ {
    #[serde(default)]
    path: String,
}

/// `GET /api/procs/{id}/files?path=` and `/file?path=`: browse and read the
/// folder an agent works in, on this machine or over a machine's link (that
/// machine checks the folder is one of its own). Nothing outside it.
async fn proc_files(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Query(q): Query<FileQ>,
) -> ApiResult<Json<Value>> {
    proc_fs(&app, id, "ls", &q.path).await
}

async fn proc_file(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Query(q): Query<FileQ>,
) -> ApiResult<Json<Value>> {
    proc_fs(&app, id, "cat", &q.path).await
}

async fn proc_fs(app: &App, id: u64, what: &str, path: &str) -> ApiResult<Json<Value>> {
    let p = app
        .procs
        .get(id)
        .ok_or_else(|| not_found("no such process"))?;
    let root = p.target().to_string();
    if p.node_name() == LOCAL {
        let r = if what == "ls" {
            nodes::list_files(&root, path)
        } else {
            nodes::read_file(&root, path)
        };
        return r.map(Json).map_err(bad);
    }
    let link = app
        .procs
        .links()
        .get(p.node_name())
        .filter(|l| l.connected())
        .ok_or_else(|| bad("that machine is not connected"))?;
    link.request(what, json!({ "root": root, "path": path }), 20)
        .await
        .map(Json)
        .map_err(bad)
}

#[derive(Deserialize)]
struct DiffQ {
    path: String,
}

/// `GET /api/procs/{id}/diff?path=`: one changed file's diff in the agent's
/// folder (a file git lists as changed there; nothing else is read).
async fn proc_diff(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Query(q): Query<DiffQ>,
) -> ApiResult<Json<Value>> {
    let p = app
        .procs
        .get(id)
        .ok_or_else(|| not_found("no such process"))?;
    if p.node_name() != LOCAL {
        return Err(bad("diffs are shown for agents on this machine"));
    }
    let dir = p.target().to_string();
    let status = git(&dir, &["status", "--porcelain", "--untracked-files=all"])
        .await
        .ok_or_else(|| bad("not a git repository"))?;
    let entry = status
        .lines()
        .find(|l| l.len() > 3 && l[3..].trim_matches('"') == q.path)
        .ok_or_else(|| not_found("that file is not changed there"))?;
    let mut diff = if entry.starts_with("??") {
        // New file: all of it is added.
        git(&dir, &["diff", "--no-index", "--", "/dev/null", &q.path])
            .await
            .unwrap_or_default()
    } else {
        git(&dir, &["diff", "HEAD", "--", &q.path])
            .await
            .unwrap_or_default()
    };
    if diff.is_empty() && entry.starts_with("??") {
        // git diff --no-index exits 1 when files differ: read it directly.
        let out = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["diff", "--no-index", "--", "/dev/null", &q.path])
            .output()
            .await
            .map_err(|e| internal(e.to_string()))?;
        diff = String::from_utf8_lossy(&out.stdout).to_string();
    }
    let cut = diff.len() > 200_000;
    if cut {
        let mut end = 200_000;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff.truncate(end);
    }
    Ok(Json(
        json!({ "path": q.path, "diff": diff, "truncated": cut }),
    ))
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
    /// A fresh start may switch profile and runner; either may change the
    /// model, permission mode and the rest (`opts`).
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    runner: Option<String>,
    #[serde(default)]
    opts: nodes::AgentOpts,
}

/// Give an agent a new direction. It runs on the same node and target. A
/// continued session keeps its runner (it lives in that runner's history);
/// a fresh start may take another profile, runner or model.
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
    let settings = app.settings();
    let profile = r.profile.clone().filter(|x| !x.is_empty());
    let runner = r.runner.clone().filter(|x| !x.is_empty());
    if cont
        && (runner.as_deref().is_some_and(|x| x != p.runner_name())
            || profile.as_deref().is_some_and(|x| Some(x) != p.profile()))
    {
        return Err(bad(
            "a continued session stays with its runner and profile; pivot with a fresh start to switch",
        ));
    }
    // The runner and options it runs with: the original's, or a new profile's,
    // with this pivot's changes on top.
    let (runner_name, agent) = match (&profile, cont) {
        (Some(pf), false) => resolve_agent(
            &settings,
            Some(pf),
            runner.as_deref().unwrap_or(""),
            &r.opts,
        )?,
        _ => {
            let a = p.opts().with(&r.opts);
            a.validate().map_err(bad)?;
            (
                runner
                    .clone()
                    .unwrap_or_else(|| p.runner_name().to_string()),
                a,
            )
        }
    };
    // An agent in an engagement stays in it, under its current terms.
    let mut agent = agent;
    agent.mcp_servers = p.opts().mcp_servers.clone();
    agent.engagement = p.opts().engagement.as_ref().map(|e| {
        app.engagements
            .get(&e.id)
            .map(|x| x.spec)
            .unwrap_or_else(|| e.clone())
    });
    let new_profile = if cont {
        p.profile().map(String::from)
    } else {
        profile.clone().or_else(|| p.profile().map(String::from))
    };
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
        if !settings.runners.iter().any(|x| x.name == runner_name) {
            return Err(bad(format!("runner '{}' is not configured", runner_name)));
        }
    } else if settings.node(p.node_name()).is_none() {
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
        // The same model, mode and environment as the agent it continues,
        // unless this pivot changes them.
        agent,
        profile: new_profile,
        // A continuation already has the memory it was given; it reflects
        // at the end if the original would have.
        memory: vec![],
        reflect: view.reflect,
    };
    let harness = p.harness_name().to_string();
    let new = if p.node_name() == LOCAL {
        let runner = settings
            .runners
            .iter()
            .find(|x| x.name == runner_name)
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
        let node = settings
            .node(p.node_name())
            .ok_or_else(|| bad("its node is no longer configured"))?;
        nodes::validate_name(&runner_name).map_err(bad)?;
        app.procs.start_with(
            Launch::Remote {
                node,
                runner: &runner_name,
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
    // Machines are listed even before any harness exists.
    let h = app.harness(q.h.as_deref()).ok();
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
        "targets": match &h { Some(h) => node_targets(&app, h, LOCAL)?, None => local_folders(&app).unwrap_or_default() }, "info": nodes::local_info(&app.settings()), "error": null, "checked": now,
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
    // A node that dials in reports its own targets when the config lists none.
    let link = app.procs.links().get(&q.node).filter(|l| l.connected());
    let targets = match (&link, n.targets.is_empty()) {
        (Some(l), true) => l.targets(),
        _ => n.targets.clone(),
    };
    let Some(spec) = RunsSpec::for_node(h, &targets) else {
        return Ok(Json(json!([])));
    };
    // Over its link when it is connected (the only way for a node without
    // SSH), else over SSH.
    let v = if let Some(l) = link {
        l.request("runs", json!(spec), 30).await
    } else if n.ssh.is_empty() {
        Err(format!(
            "node '{}' dials in and is not connected now",
            n.name
        ))
    } else {
        nodes::remote_runs(n, &spec).await
    }
    .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct FetchRunReq {
    h: Option<String>,
    node: String,
    name: String,
}

/// Copy a run from a node to the hub, so its page (findings, triage,
/// comparison, next steps) works on it. Key: `n:<node>:<run>`. The copy is a
/// copy: triage changes stay in it.
async fn node_run_fetch(
    State(app): State<Arc<App>>,
    Json(r): Json<FetchRunReq>,
) -> ApiResult<Json<Value>> {
    let h = &app.harness(r.h.as_deref())?;
    let settings = app.settings();
    let n = settings
        .node(&r.node)
        .ok_or_else(|| not_found("unknown node"))?;
    nodes::validate_name(&n.name).map_err(bad)?;
    if r.name.is_empty()
        || r.name.starts_with('.')
        || !r
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.+@".contains(c))
    {
        return Err(bad("bad run name"));
    }
    let link = app.procs.links().get(&r.node).filter(|l| l.connected());
    let targets = match (&link, n.targets.is_empty()) {
        (Some(l), true) => l.targets(),
        _ => n.targets.clone(),
    };
    let spec = RunsSpec::for_node(h, &targets).ok_or_else(|| bad("this harness has no runs"))?;
    let req = nodes::RunFilesReq {
        spec,
        name: r.name.clone(),
    };
    let v = if let Some(l) = link {
        l.request("run_files", json!(req), 120).await
    } else if n.ssh.is_empty() {
        Err(format!(
            "node '{}' dials in and is not connected now",
            n.name
        ))
    } else {
        nodes::remote_run_files(n, &req).await
    }
    .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e))?;
    let dir = settings
        .data_dir
        .join("fetched")
        .join(&n.name)
        .join(&r.name);
    let _ = std::fs::remove_dir_all(&dir);
    let mut written = 0usize;
    for f in v
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let rel = f.get("rel").and_then(Value::as_str).unwrap_or("");
        let p = std::path::Path::new(rel);
        // Only plain relative paths: nothing absolute, no `..`.
        if rel.is_empty()
            || !p
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            continue;
        }
        let out = dir.join(p);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| internal(e.to_string()))?;
        }
        std::fs::write(&out, f.get("text").and_then(Value::as_str).unwrap_or(""))
            .map_err(|e| internal(e.to_string()))?;
        written += 1;
    }
    std::fs::create_dir_all(&dir).map_err(|e| internal(e.to_string()))?;
    Ok(Json(json!({
        "key": format!("n:{}:{}", n.name, r.name), "files": written,
        "skipped": v.get("skipped").cloned().unwrap_or(json!([])),
    })))
}

// ---- workflows -------------------------------------------------------------------

/// Workflow files in repositories change under a running console (a `git
/// pull`): what was seen last, as names, sizes and times.
fn repo_fingerprint(s: &Settings) -> String {
    let mut out = String::new();
    // indra.toml too: `indra mcp` (Claude) may save a workflow into it.
    if let Some(m) = s
        .config_path
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
    {
        let t = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        out.push_str(&format!("config|{}|{}\n", m.len(), t));
    }
    for d in &s.repo_dirs {
        for f in config::repo_workflow_files(d) {
            let m = std::fs::metadata(&f).ok();
            let t = m
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            out.push_str(&format!(
                "{}|{}|{}\n",
                f.display(),
                m.map(|m| m.len()).unwrap_or(0),
                t
            ));
        }
    }
    out
}

/// Load the settings again when a repository's workflow files changed.
fn refresh_repo_workflows(app: &App) {
    static SEEN: Mutex<Option<String>> = Mutex::new(None);
    let Some(c) = &app.config else {
        return;
    };
    let fp = repo_fingerprint(&app.settings());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.as_deref() == Some(fp.as_str()) {
        return;
    }
    *seen = Some(fp);
    drop(seen);
    let _one = c.saving.lock().unwrap_or_else(|e| e.into_inner());
    let _ = reload_settings(app, c);
}

/// Run workflows that have a `schedule` when it comes due (checked every
/// 15 s, at most once a minute each, in the hub's local time). A run that
/// cannot start (an input with no default, say) is logged.
pub fn start_scheduler(app: Arc<App>) {
    tokio::spawn(async move {
        let mut last: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            if app.settings().read_only {
                continue;
            }
            refresh_repo_workflows(&app);
            let minute = runs::now() / 60;
            let when = crate::deck::cron::now_local();
            for w in app.settings().workflows.iter() {
                let Some(sc) = &w.schedule else { continue };
                let Ok(c) = crate::deck::cron::Cron::parse(sc) else {
                    continue;
                };
                if !c.matches(&when) || last.get(&w.name) == Some(&minute) {
                    continue;
                }
                last.insert(w.name.clone(), minute);
                match app.engine.start_with_inputs(&w.name, &Default::default()) {
                    Ok(id) => eprintln!(
                        "indra: started workflow '{}' (#{}) on its schedule",
                        w.name, id
                    ),
                    Err(e) => eprintln!(
                        "indra: workflow '{}' did not start on its schedule: {}",
                        w.name, e
                    ),
                }
            }
        }
    });
}

async fn workflows_list(State(app): State<Arc<App>>) -> Json<Value> {
    refresh_repo_workflows(&app);
    let defs: Vec<Value> = app
        .settings()
        .workflows
        .iter()
        .map(|w| {
            let mut v = json!(w);
            v["builtin"] = json!(w.builtin);
            v["source"] = json!(w.source.as_ref().map(|p| p.display().to_string()));
            if let Some(sc) = &w.schedule {
                v["schedule_text"] = json!(crate::deck::cron::describe(sc));
            }
            v
        })
        .collect();
    let s = app.settings();
    Json(json!({ "defs": defs, "runs": app.engine.list(), "errors": s.workflow_errors }))
}

#[derive(Deserialize)]
struct RunWfReq {
    name: String,
    /// The workflow's inputs, by name.
    #[serde(default)]
    inputs: std::collections::BTreeMap<String, String>,
}

async fn workflow_run(
    State(app): State<Arc<App>>,
    Json(r): Json<RunWfReq>,
) -> ApiResult<Json<Value>> {
    refresh_repo_workflows(&app);
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let id = app
        .engine
        .start_with_inputs(&r.name, &r.inputs)
        .map_err(bad)?;
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
    /// For a question (AskUserQuestion): each question's text to the answer.
    answers: Option<Value>,
}

/// Approve or deny a tool request the agent is waiting on.
async fn proc_permission(
    State(app): State<Arc<App>>,
    axum::Extension(who): axum::Extension<crate::deck::users::Who>,
    UrlPath(id): UrlPath<u64>,
    Json(r): Json<PermissionReq>,
) -> ApiResult<Json<Value>> {
    let p = running_proc(&app, id)?;
    p.answer(
        &r.request_id,
        r.allow,
        r.always,
        r.message.as_deref(),
        r.answers.as_ref(),
    )
    .map_err(bad)?;
    if let Some(e) = &p.opts().engagement {
        hub_audit(
            &app,
            &e.id,
            json!({ "event": "approval", "user": who.user, "proc": id, "request": r.request_id,
                    "decision": if r.allow { "allow" } else { "deny" }, "always": r.always }),
        );
    }
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
        "boards": app.board.boards(),
        "connectors": crate::deck::board::connectors_view(&s),
    }))
}

/// Accounts and the machines' own logins: what each used today and this
/// month, its limits, and whether routing can give it runs.
async fn accounts_get(State(app): State<Arc<App>>) -> Json<Value> {
    match app.procs.accounts() {
        Some(a) => {
            a.sync(&app.procs, crate::deck::runs::now());
            Json(a.view(crate::deck::runs::now()))
        }
        None => Json(json!({ "accounts": [] })),
    }
}

/// Let an account that hit a usage limit take runs again now.
async fn accounts_wake(
    State(app): State<Arc<App>>,
    UrlPath(key): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    let a = app
        .procs
        .accounts()
        .ok_or_else(|| not_found("no accounts here"))?;
    if !a.wake(&key) {
        return Err(not_found("no such account"));
    }
    Ok(Json(a.view(crate::deck::runs::now())))
}

#[derive(Deserialize)]
struct NewBoard {
    name: String,
    #[serde(default)]
    description: String,
}

async fn boards_create(
    State(app): State<Arc<App>>,
    Json(r): Json<NewBoard>,
) -> ApiResult<Json<Value>> {
    board_writable(&app)?;
    let id = app.board.add_board(&r.name, &r.description).map_err(bad)?;
    Ok(Json(json!({ "id": id, "boards": app.board.boards() })))
}

#[derive(Deserialize)]
struct BoardEdit {
    name: Option<String>,
    description: Option<String>,
    connectors: Option<Vec<String>>,
}

async fn boards_update(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<String>,
    Json(r): Json<BoardEdit>,
) -> ApiResult<Json<Value>> {
    board_writable(&app)?;
    if let Some(cs) = &r.connectors {
        let s = app.settings();
        for c in cs {
            if s.connector(c).is_none() {
                return Err(bad(format!("no connector named '{}'", c)));
            }
        }
    }
    let b = app
        .board
        .update_board(&id, r.name, r.description, r.connectors)
        .map_err(bad)?;
    Ok(Json(json!(b)))
}

#[derive(Deserialize)]
struct BoardDel {
    /// Move its items to this board; without one they are deleted.
    move_to: Option<String>,
}

async fn boards_delete(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<BoardDel>,
) -> ApiResult<Json<Value>> {
    board_writable(&app)?;
    let n = app
        .board
        .delete_board(&id, q.move_to.as_deref().filter(|x| !x.is_empty()))
        .map_err(bad)?;
    Ok(Json(json!({ "items": n, "boards": app.board.boards() })))
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
/// link key or a one-time code. A code from Machines > Add a machine is not
/// tied to a name: the node suggests one (its host name), and the hub adds it
/// to indra.toml under a free name. Outside /api: nodes have no console
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
    let refuse = |msg: &str| {
        let msg = msg.to_string();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            ApiError(StatusCode::UNAUTHORIZED, msg).into_response()
        }
    };
    let mut name = h("x-indra-node");
    if nodes::validate_name(&name).is_err() {
        return refuse("bad node name").await;
    }
    let vault = app.procs.vault();
    let key = h("authorization")
        .strip_prefix("Bearer ")
        .map(str::to_string);
    let code = h("x-indra-enroll");
    let links = app.procs.links().clone();
    let known = app.settings().node(&name).is_some();
    let enrolled = match key {
        Some(k) if known && vault.check_node_key(&name, &k) => None,
        // Removed in the console: tell it to stop (410), not to retry.
        Some(_) if !known && removed_nodes(&app).contains(&name) => {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            return ApiError(StatusCode::GONE, "removed in the console".into()).into_response();
        }
        Some(_) => return refuse("wrong key or unknown node").await,
        None if code.is_empty() => return refuse("no key and no code").await,
        None if known && links.redeem(&name, &code) => match new_node_key(&app, &name) {
            Ok(k) => Some(k),
            Err(e) => return e.into_response(),
        },
        None => match links.take_join(&code) {
            // A join code: any name, and the machine is added here.
            Some(None) => match join_node(&app, Claim::Code(&code), &name) {
                Ok((n, k)) => {
                    name = n;
                    Some(k)
                }
                Err(e) => return e.into_response(),
            },
            // An enrollment key (Machines > Add many machines).
            _ => match app.enroll.check(&code) {
                Some(id) => match join_node(&app, Claim::Key(&id), &name) {
                    Ok((n, k)) => {
                        name = n;
                        Some(k)
                    }
                    Err(e) => return e.into_response(),
                },
                None => return refuse("wrong, used or expired code or key").await,
            },
        },
    };
    ws.on_upgrade(move |socket| links.session(name, enrolled, socket))
}

/// Machines removed on the Machines page: their links are told to stop
/// rather than retry (a machine the hub merely does not know keeps trying,
/// in case the hub runs with another config for a while).
fn removed_path(app: &App) -> std::path::PathBuf {
    app.settings().data_dir.join("removed-nodes.json")
}

fn removed_nodes(app: &App) -> Vec<String> {
    std::fs::read_to_string(removed_path(app))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn set_removed(app: &App, name: &str, removed: bool) {
    let mut list = removed_nodes(app);
    list.retain(|n| n != name);
    if removed {
        list.push(name.to_string());
    }
    let p = removed_path(app);
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, serde_json::to_string(&list).unwrap_or_default());
}

/// A fresh link key for `node`, kept in the encrypted secret store.
fn new_node_key(app: &App, node: &str) -> ApiResult<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|_| internal("no OS randomness"))?;
    let k: String = b.iter().map(|x| format!("{:02x}", x)).collect();
    app.procs.vault().set_node_key(node, &k).map_err(internal)?;
    Ok(k)
}

/// A machine joins with a code: add it to indra.toml under `wanted` (or
/// `wanted-2`, ... when that is taken) and give it a key. Only a name goes
/// into the file (no command, no SSH), so this works even when the console
/// cannot edit settings because it listens on the network; not when indra
/// runs read-only or with --lock-config.
/// What a joining machine presented: a one-time code or an enrollment key.
enum Claim<'a> {
    Code(&'a str),
    Key(&'a str),
}

fn join_node(app: &App, claim: Claim, wanted: &str) -> ApiResult<(String, String)> {
    let c = config_access(app)?;
    if let Some(why) = c.join_locked.as_ref() {
        return Err(forbidden(why.clone()));
    }
    let _one = c.saving.lock().unwrap_or_else(|e| e.into_inner());
    let (current, _) = confedit::read(&c.path).map_err(internal)?;
    let cf = confedit::parse(&current).map_err(|e| {
        forbidden(format!(
            "the hub's indra.toml has an error, so it cannot add machines: {}",
            e
        ))
    })?;
    let taken = |n: &str| n == LOCAL || cf.node.iter().any(|x| x.name == n);
    let mut name = wanted.to_string();
    let mut i = 2;
    while taken(&name) {
        let base: String = wanted.chars().take(60).collect();
        name = format!("{}-{}", base, i);
        i += 1;
    }
    let claimed = match claim {
        Claim::Code(code) => app.procs.links().finish_join(code, &name, Value::Null),
        Claim::Key(id) => app.enroll.record_use(id, &name),
    };
    if !claimed {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "wrong, used or expired code".into(),
        ));
    }
    let mut list = serde_json::to_value(&cf.node).map_err(|e| internal(e.to_string()))?;
    list.as_array_mut()
        .unwrap()
        .push(json!({ "name": name, "description": "joined with a code" }));
    let next = confedit::set_section(&current, "node", &list).map_err(bad)?;
    let settings = check_config(app, c, &next)?;
    confedit::write_atomic(&c.path, &next).map_err(internal)?;
    app.settings.set(settings);
    set_removed(app, &name, false);
    let key = new_node_key(app, &name)?;
    Ok((name, key))
}

#[derive(Deserialize, Default)]
struct EnrolReq {
    /// A name ties the code to one node (the older flow). Without one, any
    /// machine can join with it and names itself.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    targets: Vec<String>,
}

/// The address other machines should use for this console: the one the
/// browser used, unless that is this machine only (then its LAN address).
fn hub_url(app: &App, headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let local = host_name(headers)
        .map(|n| is_loopback_host(&n))
        .unwrap_or(true);
    if local && !app.loopback {
        let port = host.rsplit_once(':').map(|(_, p)| p).unwrap_or("7878");
        if let Some(ip) = lan_ip() {
            return format!("{}://{}:{}", scheme, ip, port);
        }
    }
    format!(
        "{}://{}",
        scheme,
        if host.is_empty() { "HUB:7878" } else { &host }
    )
}

/// This machine's address on its network (the one it would use to reach
/// the internet; nothing is sent).
pub(crate) fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// `POST /api/nodes/enrol`: a one-time code (and the commands to use it)
/// for adding a machine. With no name, the machine names itself and is
/// added when it joins.
async fn node_enrol(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    let r: EnrolReq = if body.iter().all(u8::is_ascii_whitespace) {
        EnrolReq::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| bad(e.to_string()))?
    };
    let c = config_access(&app)?;
    let hub = hub_url(&app, &headers);
    let Some(name) = r.name.filter(|n| !n.trim().is_empty()) else {
        if let Some(why) = &c.join_locked {
            return Err(forbidden(why.clone()));
        }
        let code = app.procs.links().join_code();
        return Ok(Json(json!({
            "code": code, "expires_in": crate::deck::link::JOIN_TTL, "hub": hub,
            "reachable": !app.loopback,
            "command": format!("indra join {} {}", hub, code),
        })));
    };
    if let Some(why) = &c.locked {
        return Err(forbidden(why.clone()));
    }
    nodes::validate_name(&name).map_err(bad)?;
    if name == LOCAL {
        return Err(bad("'local' is this machine"));
    }
    if app.settings().node(&name).is_none() {
        let _one = c.saving.lock().unwrap_or_else(|e| e.into_inner());
        let (current, _) = confedit::read(&c.path).map_err(internal)?;
        let cf = confedit::parse(&current).map_err(bad)?;
        let mut list = serde_json::to_value(&cf.node).map_err(|e| internal(e.to_string()))?;
        let mut entry = json!({ "name": name });
        if !r.targets.is_empty() {
            entry["targets"] = json!(r.targets);
        }
        list.as_array_mut().unwrap().push(entry);
        let next = confedit::set_section(&current, "node", &list).map_err(bad)?;
        let settings = check_config(&app, c, &next)?;
        confedit::write_atomic(&c.path, &next).map_err(internal)?;
        app.settings.set(settings);
    }
    let code = app.procs.links().enrol_code(&name);
    Ok(Json(json!({
        "code": code, "expires_in": 900, "hub": hub, "reachable": !app.loopback,
        "command": format!("indra node connect {} --name {} --code {}", hub, name, code),
    })))
}

/// `DELETE /api/nodes/{name}`: remove a machine that dials in: its entry in
/// indra.toml and its key, and close its link. Allowed whenever machines may
/// join (a node reached over SSH is edited in Settings).
async fn node_remove(
    State(app): State<Arc<App>>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    let c = config_access(&app)?;
    if let Some(why) = &c.join_locked {
        return Err(forbidden(why.clone()));
    }
    let n = app
        .settings()
        .node(&name)
        .cloned()
        .ok_or_else(|| not_found("no such machine"))?;
    if !n.ssh.is_empty() && c.locked.is_some() {
        return Err(forbidden(
            "this machine is reached over SSH: remove it in Settings > Nodes",
        ));
    }
    {
        let _one = c.saving.lock().unwrap_or_else(|e| e.into_inner());
        let (current, _) = confedit::read(&c.path).map_err(internal)?;
        let cf = confedit::parse(&current).map_err(bad)?;
        let list: Vec<_> = cf.node.iter().filter(|x| x.name != name).collect();
        let list = serde_json::to_value(&list).map_err(|e| internal(e.to_string()))?;
        let next = confedit::set_section(&current, "node", &list).map_err(bad)?;
        let settings = check_config(&app, c, &next).map_err(|e| {
            ApiError(
                e.0,
                format!(
                    "{} (is the machine still used by a workflow or profile?)",
                    e.1
                ),
            )
        })?;
        confedit::write_atomic(&c.path, &next).map_err(internal)?;
        app.settings.set(settings);
    }
    let _ = app
        .procs
        .vault()
        .remove(&crate::deck::link::key_name(&name));
    set_removed(&app, &name, true);
    app.procs.links().remove(&name);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct DirsQ {
    path: Option<String>,
}

/// `GET /api/nodes/{name}/dirs?path=`: the folder browser for picking where
/// agents work: folder names only, from this machine or over a machine's
/// link (admins only, like changing the folders).
async fn node_dirs(
    State(app): State<Arc<App>>,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<DirsQ>,
) -> ApiResult<Json<Value>> {
    if name == LOCAL {
        return nodes::list_dirs(q.path.as_deref()).map(Json).map_err(bad);
    }
    let link = app
        .procs
        .links()
        .get(&name)
        .filter(|l| l.connected())
        .ok_or_else(|| {
            bad("browsing needs the machine to be connected; type the folder's path instead")
        })?;
    link.request("dirs", json!({ "path": q.path }), 20)
        .await
        .map(Json)
        .map_err(bad)
}

#[derive(Deserialize)]
struct FoldersReq {
    folders: Vec<String>,
}

/// `PUT /api/nodes/{name}/folders`: the folders agents may work in on a
/// machine. This machine keeps them in its data folder; a machine that
/// joined checks and keeps them itself (`indra node folders` shows them);
/// one listed with `targets` in indra.toml gets them there. Allowed whenever
/// machines may join (they are paths, not commands).
async fn node_folders(
    State(app): State<Arc<App>>,
    UrlPath(name): UrlPath<String>,
    Json(r): Json<FoldersReq>,
) -> ApiResult<Json<Value>> {
    let c = config_access(&app)?;
    if let Some(why) = &c.join_locked {
        return Err(forbidden(why.clone()));
    }
    if name == LOCAL {
        let list = nodes::check_folders(&r.folders).map_err(bad)?;
        let p = app.settings().data_dir.join("local-folders.json");
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| internal(e.to_string()))?;
        }
        std::fs::write(&p, serde_json::to_string_pretty(&list).unwrap_or_default())
            .map_err(|e| internal(e.to_string()))?;
        return Ok(Json(json!({ "folders": list })));
    }
    let n = app
        .settings()
        .node(&name)
        .cloned()
        .ok_or_else(|| not_found("no such machine"))?;
    if n.targets.is_empty() {
        if let Some(l) = app.procs.links().get(&name).filter(|l| l.connected()) {
            let v = l
                .request("set_folders", json!({ "folders": r.folders }), 20)
                .await
                .map_err(bad)?;
            let list: Vec<String> = serde_json::from_value(v).unwrap_or_default();
            l.set_targets(&list);
            return Ok(Json(json!({ "folders": list })));
        }
        if n.ssh.is_empty() {
            return Err(bad(
                "the machine is not connected: on it, run `indra node folders <folder>`",
            ));
        }
    }
    // Listed in indra.toml (reached over SSH, or set up by hand).
    let list: Vec<String> = r
        .folders
        .iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    let _one = c.saving.lock().unwrap_or_else(|e| e.into_inner());
    let (current, _) = confedit::read(&c.path).map_err(internal)?;
    let cf = confedit::parse(&current).map_err(bad)?;
    let mut nodes_v = serde_json::to_value(&cf.node).map_err(|e| internal(e.to_string()))?;
    for x in nodes_v.as_array_mut().unwrap() {
        if x["name"] == name {
            x["targets"] = json!(list);
        }
    }
    let next = confedit::set_section(&current, "node", &nodes_v).map_err(bad)?;
    let settings = check_config(&app, c, &next)?;
    confedit::write_atomic(&c.path, &next).map_err(internal)?;
    app.settings.set(settings);
    Ok(Json(json!({ "folders": list })))
}

#[derive(Deserialize)]
struct NewKey {
    #[serde(default)]
    label: String,
    #[serde(default = "thirty")]
    days: u32,
    max_uses: Option<u32>,
}

fn thirty() -> u32 {
    30
}

/// `POST /api/nodes/keys`: an enrollment key, shown this once.
async fn enroll_key_create(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(r): Json<NewKey>,
) -> ApiResult<Json<Value>> {
    let c = config_access(&app)?;
    if let Some(why) = &c.join_locked {
        return Err(forbidden(why.clone()));
    }
    let (key, mut v) = app
        .enroll
        .create(&r.label, r.days, r.max_uses)
        .map_err(bad)?;
    v["key"] = json!(key);
    v["hub"] = json!(hub_url(&app, &headers));
    v["reachable"] = json!(!app.loopback);
    Ok(Json(v))
}

/// `GET /api/engagements`: every engagement, newest first.
async fn engagements_list(State(app): State<Arc<App>>) -> Json<Value> {
    let t = crate::deck::engage::now();
    Json(json!(app
        .engagements
        .list()
        .iter()
        .map(|e| crate::deck::engage::view(e, t))
        .collect::<Vec<_>>()))
}

/// `POST /api/engagements` (admins): a new engagement.
async fn engagement_create(
    State(app): State<Arc<App>>,
    axum::Extension(who): axum::Extension<crate::deck::users::Who>,
    Json(n): Json<crate::deck::engage::New>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    if n.machine != LOCAL && app.settings().node(&n.machine).is_none() {
        return Err(bad(format!(
            "{} is not one of this hub's machines",
            n.machine
        )));
    }
    let e = app.engagements.create(n, &who.user).map_err(bad)?;
    hub_audit(
        &app,
        &e.spec.id,
        json!({ "event": "created", "user": who.user, "name": e.spec.name, "machine": e.machine,
                "scope": e.spec.scope, "exclude": e.spec.exclude, "starts": e.spec.starts,
                "ends": e.spec.ends, "rate": e.spec.rate, "authorization": e.authorization }),
    );
    Ok(Json(crate::deck::engage::view(
        &e,
        crate::deck::engage::now(),
    )))
}

/// `POST /api/engagements/{id}/end` (admins): end it now and stop its agents.
async fn engagement_end(
    State(app): State<Arc<App>>,
    axum::Extension(who): axum::Extension<crate::deck::users::Who>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let e = app.engagements.end(&id).map_err(not_found)?;
    let mut stopped = vec![];
    for p in app.procs.list() {
        if p.status == "running"
            && p.opts
                .engagement
                .as_ref()
                .map(|x| x.id == id)
                .unwrap_or(false)
        {
            if let Some(proc_) = app.procs.get(p.id) {
                if proc_.request_stop() {
                    stopped.push(p.id);
                }
            }
        }
    }
    hub_audit(
        &app,
        &id,
        json!({ "event": "ended", "user": who.user, "stopped": stopped }),
    );
    let mut v = crate::deck::engage::view(&e, crate::deck::engage::now());
    v["stopped"] = json!(stopped);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct AuditQ {
    #[serde(default)]
    format: Option<String>,
}

/// `GET /api/engagements/{id}/audit[?format=csv|jsonl]`: the console's
/// entries and those of the machine its agents ran on, merged by time, and
/// whether both chains are intact.
async fn engagement_audit(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<AuditQ>,
) -> ApiResult<axum::response::Response> {
    let e = app
        .engagements
        .get(&id)
        .ok_or_else(|| not_found("no such engagement"))?;
    let (entries, intact, machine) = merged_audit(&app, &e).await;
    let name = format!("engagement-{}-audit", id);
    match q.format.as_deref() {
        Some("jsonl") => {
            let body: String = entries.iter().map(|x| x.to_string() + "\n").collect();
            Ok(download(
                &format!("{}.jsonl", name),
                "application/x-ndjson",
                body,
            ))
        }
        Some("csv") => {
            let cols = [
                "ts",
                "event",
                "user",
                "proc",
                "session",
                "tool",
                "targets",
                "decision",
                "reason",
                "waited_ms",
                "ok",
                "input",
                "hash",
            ];
            Ok(download(
                &format!("{}.csv", name),
                "text/csv",
                csv(&cols, &entries),
            ))
        }
        _ => Ok(
            Json(json!({ "entries": entries, "intact": intact, "machine": machine }))
                .into_response(),
        ),
    }
}

/// CSV with every cell quoted, and cells a spreadsheet would run as a
/// formula neutralised.
fn csv(cols: &[&str], rows: &[Value]) -> String {
    let cell = |v: &Value| -> String {
        let s = match v {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
            format!("'{}", s)
        } else {
            s
        };
        format!("\"{}\"", s.replace('"', "\"\""))
    };
    let mut body = cols.join(",") + "\n";
    for x in rows {
        body.push_str(
            &cols
                .iter()
                .map(|c| cell(&x[*c]))
                .collect::<Vec<_>>()
                .join(","),
        );
        body.push('\n');
    }
    body
}

/// An engagement's audit log: the console's entries and those of its
/// machine, merged by time; whether every chain is intact; and whether the
/// machine could be read.
async fn merged_audit(
    app: &App,
    e: &crate::deck::engage::Engagement,
) -> (Vec<Value>, bool, &'static str) {
    use crate::deck::engage;
    let id = e.spec.id.clone();
    let (mut entries, mut intact) =
        engage::read_audit(&engage::dir_of(&app.settings().data_dir, &id), 20_000);
    let mut machine = "read";
    if e.machine == LOCAL {
        let local = engage::local_dir(&id);
        if local != engage::dir_of(&app.settings().data_dir, &id) {
            let (more, ok) = engage::read_audit(&local, 20_000);
            entries.extend(more);
            intact &= ok;
        }
    } else {
        match app.procs.links().get(&e.machine).filter(|l| l.connected()) {
            Some(l) => match l.request("audit", json!({ "id": id }), 30).await {
                Ok(v) => {
                    entries.extend(v["entries"].as_array().cloned().unwrap_or_default());
                    intact &= v["intact"].as_bool().unwrap_or(false);
                }
                Err(_) => machine = "unreachable",
            },
            None => machine = "unreachable",
        }
    }
    let mut seen = std::collections::HashSet::new();
    entries.retain(|x| seen.insert(x["hash"].as_str().unwrap_or("").to_string()));
    entries.sort_by_key(|x| x["ts"].as_u64().unwrap_or(0));
    (entries, intact, machine)
}

#[derive(Deserialize, Default)]
struct DetectReq {
    /// SIEM sources to ask (default: all).
    #[serde(default)]
    sources: Vec<String>,
    /// How long after a step an alert still counts (seconds, default 900).
    #[serde(default)]
    slack: Option<u64>,
}

/// `POST /api/engagements/{id}/detections`: which of the engagement's
/// steps (the tool calls the guard allowed) the SIEM saw. Each source is
/// asked for its events over the engagement's activity; a step counts as
/// detected when an event from a minute before it to `slack` after it
/// names one of its hosts.
async fn engagement_detections(
    State(app): State<Arc<App>>,
    axum::Extension(who): axum::Extension<crate::deck::users::Who>,
    UrlPath(id): UrlPath<String>,
    body: Option<Json<DetectReq>>,
) -> ApiResult<Json<Value>> {
    use crate::deck::{engage, siem};
    let r = body.map(|b| b.0).unwrap_or_default();
    let e = app
        .engagements
        .get(&id)
        .ok_or_else(|| not_found("no such engagement"))?;
    let settings = app.settings();
    let chosen: Vec<&siem::SiemCfg> = settings
        .siem
        .iter()
        .filter(|c| r.sources.is_empty() || r.sources.contains(&c.name))
        .collect();
    if chosen.is_empty() {
        return Err(bad("no data source to ask: add a Wazuh, Elastic, OpenSearch, Splunk or log-file tool under Settings > MCP & tools"));
    }
    let slack = r.slack.unwrap_or(900).clamp(60, 86_400);
    let (entries, intact, machine) = merged_audit(&app, &e).await;
    let steps: Vec<&Value> = entries
        .iter()
        .filter(|x| {
            x["event"] == "tool"
                && x["decision"] == "allow"
                && x["targets"]
                    .as_array()
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
        })
        .collect();
    if steps.is_empty() {
        return Err(bad("the engagement has no steps that reached a host yet"));
    }
    let first = steps
        .iter()
        .filter_map(|x| x["ts"].as_u64())
        .min()
        .unwrap_or(0)
        / 1000;
    let last = steps
        .iter()
        .filter_map(|x| x["ts"].as_u64())
        .max()
        .unwrap_or(0)
        / 1000;
    let q = siem::Query {
        text: String::new(),
        from: first.saturating_sub(60),
        to: last + slack,
        limit: 2000,
        min_level: None,
    };
    let vault = crate::deck::secrets::Vault::open(settings.data_dir.join("secrets"));
    let mut events: Vec<siem::Event> = Vec::new();
    let mut sources = Vec::new();
    for c in chosen {
        let res = match siem::open(c, &vault) {
            Ok(src) => siem::search(&src, &q).await,
            Err(e) => Err(e),
        };
        match res {
            Ok(evs) => {
                sources.push(
                    json!({ "name": c.name, "kind": c.kind, "events": evs.len(), "error": null }),
                );
                events.extend(evs);
            }
            Err(err) => {
                sources.push(json!({ "name": c.name, "kind": c.kind, "events": 0, "error": err }))
            }
        }
    }
    let mut techniques = std::collections::BTreeSet::new();
    let mut detected = 0;
    let out_steps: Vec<Value> = steps
        .iter()
        .map(|x| {
            let ts = x["ts"].as_u64().unwrap_or(0);
            let targets: Vec<String> = x["targets"]
                .as_array()
                .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let alerts: Vec<Value> = events
                .iter()
                .filter(|ev| ev.ts + 60_000 >= ts && ev.ts <= ts + slack * 1000)
                .filter(|ev| targets.iter().any(|t| engage::mentions(t, &ev.raw)))
                .take(20)
                .map(|ev| {
                    techniques.extend(ev.mitre.iter().cloned());
                    json!({ "source": ev.source, "ts": ev.ts, "rule": ev.rule, "level": ev.level, "mitre": ev.mitre, "host": ev.host })
                })
                .collect();
            if !alerts.is_empty() {
                detected += 1;
            }
            json!({ "ts": ts, "tool": x["tool"], "targets": targets, "session": x["session"],
                    "input": crate::mem::search::truncate(x["input"].as_str().unwrap_or(""), 400),
                    "detected": !alerts.is_empty(), "alerts": alerts })
        })
        .collect();
    let report = json!({
        "engagement": id, "name": e.spec.name, "checked": engage::now(), "by": who.user, "slack": slack,
        "audit_intact": intact, "machine": machine, "sources": sources,
        "summary": { "steps": out_steps.len(), "detected": detected, "missed": out_steps.len() - detected },
        "techniques": techniques, "steps": out_steps,
    });
    let dir = engage::dir_of(&settings.data_dir, &id);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(
        dir.join("detections.json"),
        serde_json::to_string_pretty(&report).unwrap_or_default(),
    );
    hub_audit(
        &app,
        &id,
        json!({ "event": "detections", "user": who.user, "steps": out_steps.len(), "detected": detected }),
    );
    Ok(Json(report))
}

/// `GET /api/integrations`: MCP & tools (no secret values).
async fn integrations_list(State(app): State<Arc<App>>) -> Json<Value> {
    Json(json!(app
        .settings()
        .tools
        .iter()
        .map(crate::deck::integrations::ToolCfg::view)
        .collect::<Vec<_>>()))
}

/// `POST /api/integrations/{name}/test` (admins): a data source answers
/// with its credentials; an MCP server starts (or answers at its URL) and
/// lists its tools.
async fn integration_test(
    State(app): State<Arc<App>>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    let settings = app.settings();
    let t = settings
        .tools
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| not_found("no such tool"))?;
    if t.is_source() {
        let src = crate::deck::siem::source(&settings, &name).map_err(bad)?;
        let msg = crate::deck::siem::check(&src).await.map_err(bad)?;
        return Ok(Json(json!({ "ok": true, "message": msg })));
    }
    let vault = crate::deck::secrets::Vault::open(settings.data_dir.join("secrets"));
    let tools = crate::deck::integrations::probe(t, &vault)
        .await
        .map_err(bad)?;
    Ok(Json(json!({ "ok": true, "tools": tools,
        "message": format!("{} tool(s): {}", tools.len(), tools.join(", ")) })))
}

fn download(name: &str, kind: &str, body: String) -> axum::response::Response {
    (
        [
            (axum::http::header::CONTENT_TYPE, kind.to_string()),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", name),
            ),
        ],
        body,
    )
        .into_response()
}

/// `GET /api/nodes/keys`: the enrollment keys, without their secrets.
async fn enroll_key_list(State(app): State<Arc<App>>) -> Json<Value> {
    Json(json!(app.enroll.list()))
}

/// `DELETE /api/nodes/keys/{id}`: revoke a key (machines that joined stay).
async fn enroll_key_revoke(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    app.enroll.revoke(&id).map_err(not_found)?;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/nodes/enrol/{code}`: has the machine joined yet, and what is it.
async fn node_enrol_status(
    State(app): State<Arc<App>>,
    UrlPath(code): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    let links = app.procs.links();
    let mut v = links
        .code_status(&code)
        .ok_or_else(|| not_found("no such code (it may have expired)"))?;
    if let Some(n) = v["joined"].as_str().map(String::from) {
        if let Some(info) = links.get(&n).and_then(|l| l.info()) {
            v["info"] = info;
        }
        if let Some(l) = links.get(&n) {
            v["targets"] = json!(l.targets());
            v["path"] = l.view()["path"].clone();
        }
    }
    Ok(Json(v))
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

/// The repositories workflows can be kept in, and the workflow files found
/// there (as written, for the editor), with any that could not be used.
fn repo_files_payload(s: &Settings) -> Value {
    let mut files = Vec::new();
    for d in &s.repo_dirs {
        for f in config::repo_workflow_files(d) {
            let (wfs, comps, error) = match config::read_repo_file(&f) {
                Ok(r) => (json!(r.workflow), json!(r.component), Value::Null),
                Err(e) => (json!([]), json!([]), json!(e)),
            };
            files.push(json!({
                "repo": d.display().to_string(), "file": f.display().to_string(),
                "workflows": wfs, "components": comps, "error": error,
            }));
        }
    }
    json!({
        "repos": s.repo_dirs.iter().map(|d| json!({
            "dir": d.display().to_string(),
            "folder": config::repo_workflow_dir(d).display().to_string(),
        })).collect::<Vec<_>>(),
        "files": files,
        "errors": s.workflow_errors,
    })
}

/// A workflow file in a known repository's `.indra/workflows/`, or an error.
fn repo_file_in(s: &Settings, file: &str) -> ApiResult<std::path::PathBuf> {
    let f = std::path::PathBuf::from(file);
    let name_ok = f
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".toml") && !n.starts_with('.'));
    let dir_ok = s.repo_dirs.iter().any(|d| {
        let w = config::repo_workflow_dir(d);
        f.parent().is_some_and(|p| {
            p == w || std::fs::canonicalize(p).ok() == std::fs::canonicalize(&w).ok() && p.exists()
        })
    });
    if !name_ok || !dir_ok {
        return Err(bad(
            "not a workflow file of a repository this console knows",
        ));
    }
    Ok(f)
}

fn write_repo_file(f: &std::path::Path, rf: &config::RepoFile) -> ApiResult<()> {
    config::write_repo_file(f, rf).map_err(internal)
}

/// Load the settings again (a repository file changed).
fn reload_settings(app: &App, c: &ConfigAccess) -> ApiResult<()> {
    let (current, _) = confedit::read(&c.path).map_err(internal)?;
    let settings = check_config(app, c, &current)?;
    app.settings.set(settings);
    Ok(())
}

#[derive(Deserialize)]
struct RepoWfSave {
    /// The repository (one of the known ones).
    repo: String,
    workflow: config::WorkflowCfg,
    /// Where it was before: a repository file, and its name there.
    #[serde(default)]
    original_file: Option<String>,
    #[serde(default)]
    original_name: Option<String>,
    /// Saved tasks to keep in the same file, so the file stands on its own.
    #[serde(default)]
    components: Vec<config::ComponentCfg>,
}

/// Save a workflow into a repository, as `.indra/workflows/<name>.toml` (or
/// back into the file it came from), so it can be committed and shared.
async fn repo_wf_save(
    State(app): State<Arc<App>>,
    Json(r): Json<RepoWfSave>,
) -> ApiResult<Json<Value>> {
    let c = config_access(&app)?;
    if let Some(why) = &c.locked {
        return Err(forbidden(why.clone()));
    }
    let _one = c.saving.lock().unwrap_or_else(|e| e.into_inner());
    let s = app.settings();
    let repo = s
        .repo_dirs
        .iter()
        .find(|d| d.display().to_string() == r.repo)
        .ok_or_else(|| bad("not a repository this console knows (a harness root or target)"))?
        .clone();
    let w = r.workflow;
    nodes::validate_name(&w.name).map_err(|e| bad(format!("workflow name: {}", e)))?;
    if w.tasks.is_empty() {
        return Err(bad("add at least one step"));
    }
    let old_name = r.original_name.clone().unwrap_or_else(|| w.name.clone());
    let original = match &r.original_file {
        Some(f) => Some(repo_file_in(&s, f)?),
        None => None,
    };
    let dir = config::repo_workflow_dir(&repo);
    // Back into its own file when it stays in that repository.
    let target = match &original {
        Some(f) if f.parent() == Some(dir.as_path()) => f.clone(),
        _ => dir.join(format!("{}.toml", w.name)),
    };
    let mut rf = if target.exists() {
        config::read_repo_file(&target).map_err(|e| bad(format!("{}: {}", target.display(), e)))?
    } else {
        config::RepoFile::default()
    };
    let same_file = original.as_ref() == Some(&target);
    if same_file {
        rf.workflow.retain(|x| x.name != old_name);
    }
    if rf.workflow.iter().any(|x| x.name == w.name) {
        return Err(bad(format!(
            "{} already has a workflow named '{}'",
            target.display(),
            w.name
        )));
    }
    rf.workflow.push(w.clone());
    for comp in r.components {
        nodes::validate_name(&comp.name).map_err(|e| bad(format!("saved task name: {}", e)))?;
        rf.component.retain(|x| x.name != comp.name);
        rf.component.push(comp);
    }
    write_repo_file(&target, &rf)?;
    // Moved out of another file: take it out of there.
    if let Some(f) = original.filter(|f| f != &target) {
        if let Ok(mut old) = config::read_repo_file(&f) {
            old.workflow.retain(|x| x.name != old_name);
            write_repo_file(&f, &old)?;
        }
    }
    reload_settings(&app, c)?;
    let now = app.settings();
    let problem = now
        .workflow_errors
        .iter()
        .find(|e| e.starts_with(&target.display().to_string()))
        .cloned();
    Ok(Json(json!({
        "file": target.display().to_string(),
        "problem": problem,
        "config": config_payload(&app)?,
    })))
}

#[derive(Deserialize)]
struct GenReq {
    /// What the workflow should do, in the person's words.
    description: String,
    /// "config" or one of the known repositories.
    #[serde(default)]
    location: String,
}

/// Describe a workflow and let Claude Code build it: it starts with the
/// `indra mcp` server (the format, this machine's profiles, nodes and
/// skills, validate and save) and saves the result where asked. The console
/// sees the saved workflow on its next look.
async fn wf_generate(State(app): State<Arc<App>>, Json(r): Json<GenReq>) -> ApiResult<Json<Value>> {
    let c = config_access(&app)?;
    if let Some(why) = &c.locked {
        return Err(forbidden(why.clone()));
    }
    let s = app.settings();
    if s.read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let want = r.description.trim();
    if want.is_empty() || want.len() > 8000 {
        return Err(bad("describe the workflow (up to 8000 characters)"));
    }
    let loc = match r.location.trim() {
        "" | "config" => None,
        x => Some(
            s.repo_dirs
                .iter()
                .find(|d| d.display().to_string() == x)
                .cloned()
                .ok_or_else(|| bad("not a repository this console knows"))?,
        ),
    };
    let claude = s
        .runners
        .iter()
        .find(|x| x.format == "claude-stream-json" && x.program.is_some())
        .ok_or_else(|| bad("Claude Code is not installed on this machine (the claude runner)"))?;
    let mcp =
        crate::deck::mcp::client_config(&s.data_dir.join("mcp"), &c.opts).map_err(internal)?;
    let mut runner = claude.clone();
    runner.args.extend([
        "--mcp-config".to_string(),
        mcp.display().to_string(),
        "--strict-mcp-config".to_string(),
    ]);
    let target = loc.clone().unwrap_or_else(|| {
        s.harnesses
            .first()
            .and_then(|h| h.targets.first().cloned())
            .unwrap_or_else(|| c.opts.cwd.clone())
    });
    let harness = s
        .harnesses
        .first()
        .map(|h| h.name.clone())
        .unwrap_or_default();
    let location = loc
        .as_ref()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "config".into());
    let prompt = format!(
        "Create an indra workflow for this request, then save it.\n\n\
         Request:\n{want}\n\n\
         How:\n\
         1. Call the workflow_guide tool (the indra MCP server) to learn the format and what this machine has: profiles, nodes, repositories, skills, saved tasks.\n\
         2. If the request depends on this repository (its build, test or lint commands, its layout), look with Read, Glob and Grep.\n\
         3. Write the workflow as TOML. Use shell steps (run) for exact commands and agent steps (prompt) for judgement; add inputs for what the person running it should choose; use conditions (when, on), allow_failure, foreach and until where the request needs branching, error handling or loops.\n\
         4. Call validate_workflow with location \"{location}\" and fix every error it reports.\n\
         5. Call save_workflow with location \"{location}\".\n\
         6. Reply with the workflow's name and one line per step.\n\
         Do not run the workflow, and change no other file."
    );
    let agent = crate::deck::nodes::AgentOpts {
        allowed_tools: ["mcp__indra", "Read", "Glob", "Grep"]
            .map(String::from)
            .to_vec(),
        ..Default::default()
    };
    let pid = app
        .procs
        .start_with(
            Launch::Local {
                runner: &runner,
                harness: &harness,
                prompt: &prompt,
                target: &target,
            },
            crate::deck::procs::StartOpts {
                agent,
                ..Default::default()
            },
        )
        .map_err(bad)?;
    Ok(Json(json!({ "proc": pid, "location": location })))
}

#[derive(Deserialize)]
struct RepoWfDelete {
    file: String,
    name: String,
}

async fn repo_wf_delete(
    State(app): State<Arc<App>>,
    Json(r): Json<RepoWfDelete>,
) -> ApiResult<Json<Value>> {
    let c = config_access(&app)?;
    if let Some(why) = &c.locked {
        return Err(forbidden(why.clone()));
    }
    let _one = c.saving.lock().unwrap_or_else(|e| e.into_inner());
    let f = repo_file_in(&app.settings(), &r.file)?;
    let mut rf = config::read_repo_file(&f).map_err(bad)?;
    rf.workflow.retain(|x| x.name != r.name);
    write_repo_file(&f, &rf)?;
    reload_settings(&app, c)?;
    Ok(Json(config_payload(&app)?))
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
        "repo": repo_files_payload(&s),
        "builtin": {
            "runners": config::default_runners(),
            "presets": config::default_presets(),
            "profiles": config::default_profiles(),
            "teams": config::default_teams(),
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
    // Checked with the organization's settings around it, as it will load.
    let file = config::parse_layered(text).map_err(|e| bad(format!("not saved: {}", e)))?;
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
    refresh_repo_workflows(&app);
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

// ---- plan and delegate -------------------------------------------------------------

#[derive(Deserialize)]
struct DelegateReq {
    h: Option<String>,
    node: Option<String>,
    #[serde(default)]
    target: usize,
    prompt: String,
    /// A [[team]] by name...
    team: Option<String>,
    /// ...or the roles spelled out.
    roles: Option<crate::deck::delegate::Roles>,
    /// Override the team's choice of waiting for approval.
    auto_run: Option<bool>,
}

async fn delegate_start(
    State(app): State<Arc<App>>,
    Json(r): Json<DelegateReq>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let prompt = check_prompt(&r.prompt)?;
    let h = app.harness(r.h.as_deref())?;
    let node = r.node.clone().unwrap_or_else(|| LOCAL.to_string());
    let targets = node_targets(&app, &h, &node)?;
    let target = targets
        .get(r.target)
        .ok_or_else(|| bad("unknown target"))?
        .clone();
    let settings = app.settings();
    let team = r.team.clone().filter(|t| !t.is_empty());
    let mut roles = match (&team, r.roles) {
        (Some(t), _) => settings
            .team(t)
            .map(crate::deck::delegate::Roles::from)
            .ok_or_else(|| bad(format!("no team named '{}'", t)))?,
        (None, Some(roles)) => roles,
        (None, None) => return Err(bad("pick a team, or a planner and a worker profile")),
    };
    if let Some(a) = r.auto_run {
        roles.auto_run = a;
    }
    let id = app
        .delegations
        .start(&app, prompt, &h.name, &node, &target, team, roles)
        .map_err(bad)?;
    Ok(Json(json!({ "id": id })))
}

/// A delegation with its agents and what each role cost.
fn delegation_json(app: &App, d: &crate::deck::delegate::Delegation) -> Value {
    let planner = d
        .planner_proc
        .and_then(|p| app.procs.get(p))
        .map(|p| p.view());
    let run = d.workflow.and_then(|w| app.engine.get(w));
    let mut cost: BTreeMap<&str, f64> = BTreeMap::new();
    if let Some(c) = planner.as_ref().and_then(|v| v.cost_usd) {
        *cost.entry("planner").or_default() += c;
    }
    let mut steps: Vec<Value> = Vec::new();
    if let Some(run) = &run {
        for t in &run.tasks {
            let role = if t.id == "review" {
                "reviewer"
            } else if d
                .plan
                .as_ref()
                .and_then(|p| p.steps.iter().find(|s| s.id == t.id))
                .map(|s| s.size == "light")
                .unwrap_or(false)
                && d.roles.light_worker.is_some()
            {
                "light_worker"
            } else {
                "worker"
            };
            let spent: f64 = t
                .procs
                .iter()
                .filter_map(|p| app.procs.get(*p))
                .filter_map(|p| p.view().cost_usd)
                .sum();
            *cost.entry(role).or_default() += spent;
            steps.push(
                json!({ "id": t.id, "role": role, "profile": t.profile, "state": t.state,
                               "proc": t.proc_id, "cost_usd": spent, "error": t.error }),
            );
        }
    }
    let total: f64 = cost.values().sum();
    json!({
        "delegation": d,
        "planner": planner,
        "steps": steps,
        "cost": cost, "total_cost_usd": total,
    })
}

async fn delegations_list(State(app): State<Arc<App>>) -> Json<Value> {
    Json(json!(app
        .delegations
        .list()
        .iter()
        .map(|d| delegation_json(&app, d))
        .collect::<Vec<_>>()))
}

async fn delegation_get(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    let d = app
        .delegations
        .get(id)
        .ok_or_else(|| not_found("no such plan"))?;
    Ok(Json(delegation_json(&app, &d)))
}

#[derive(Deserialize)]
struct ApproveReq {
    plan: Option<crate::deck::delegate::Plan>,
}

async fn delegation_approve(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
    Json(r): Json<ApproveReq>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    let run = app.delegations.approve(&app, id, r.plan).map_err(bad)?;
    Ok(Json(json!({ "workflow": run })))
}

async fn delegation_cancel(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    app.delegations.cancel(&app, id).map_err(bad)?;
    Ok(Json(json!({ "cancelled": true })))
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
    /// For `claude`: a profile to start it with (an Ollama one included).
    profile: Option<String>,
    /// For `shell`: which one (from the offered list); default: the configured one.
    shell: Option<String>,
}

/// Claude Code's arguments, its environment, and where it runs (for the label).
type TermClaude = (Vec<String>, Vec<(String, String)>, Option<String>);

/// Claude Code's flags and environment for a profile's options in a
/// terminal: the same provider setup and secrets an agent launch gets.
/// `flags` adds the model, mode and tool rules (a new session; a resumed one
/// keeps its own).
fn claude_term_env(
    app: &App,
    r: &config::Runner,
    o: &nodes::AgentOpts,
    profile: Option<&str>,
    flags: bool,
) -> ApiResult<TermClaude> {
    o.validate().map_err(bad)?;
    let mut args: Vec<String> = Vec::new();
    if flags {
        let mut flag = |f: &str, v: &Option<String>| {
            if let Some(v) = v {
                args.push(f.into());
                args.push(v.clone());
            }
        };
        flag("--model", &o.model);
        flag("--permission-mode", &o.permission_mode);
        flag("--effort", &o.effort);
        if !o.allowed_tools.is_empty() {
            args.push("--allowed-tools".into());
            args.push(o.allowed_tools.join(","));
        }
        if !o.disallowed_tools.is_empty() {
            args.push("--disallowed-tools".into());
            args.push(o.disallowed_tools.join(","));
        }
    }
    let env = nodes::provider_env(r, true, o, &mut args).map_err(bad)?;
    let (env, _) = app
        .procs
        .vault()
        .resolve(&env, profile, LOCAL)
        .map_err(bad)?;
    let on = if o.provider.as_deref() == Some("ollama") {
        Some(format!(
            "on {} (Ollama)",
            o.model.clone().unwrap_or_default()
        ))
    } else if o.subagent_provider.as_deref() == Some("ollama") {
        Some("with its subagents on Ollama".to_string())
    } else {
        None
    };
    Ok((args, env.into_iter().collect(), on))
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
            let cfg = &app.settings().terminal;
            let (default, default_args) = term::default_shell(cfg);
            let (program, args, name) = match q.shell.as_deref().filter(|x| !x.is_empty()) {
                None => (default, default_args, "shell".to_string()),
                Some(want) => {
                    let p = term::shells(cfg)
                        .into_iter()
                        .find(|p| p.display().to_string() == want)
                        .ok_or_else(|| bad("that shell is not offered here (see Settings > General > Terminal)"))?;
                    let args = if p == default { default_args } else { vec![] };
                    let name = p
                        .file_stem()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "shell".into());
                    (p, args, name)
                }
            };
            Ok(Spec {
                program,
                args,
                label: format!("{} in {}", name, cwd.display()),
                cwd,
                env: vec![],
            })
        }
        "claude" => {
            let cwd = dir()?;
            let profile = q.profile.as_deref().filter(|p| !p.is_empty());
            let (r, opts) = match profile {
                Some(name) => {
                    let p = s
                        .profile(name)
                        .ok_or_else(|| bad(format!("no profile named '{}'", name)))?;
                    (claude(p.runner.as_deref())?, p.opts())
                }
                None => (claude(None)?, nodes::AgentOpts::default()),
            };
            let (args, env, on) = claude_term_env(app, r, &opts, profile, true)?;
            Ok(Spec {
                program: r.program.clone().unwrap(),
                args,
                label: format!(
                    "Claude Code{}{} in {}",
                    profile.map(|p| format!(" ({})", p)).unwrap_or_default(),
                    on.map(|o| format!(" {}", o)).unwrap_or_default(),
                    cwd.display()
                ),
                cwd,
                env,
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
            // Back where it ran: an agent on Ollama resumes on Ollama, with
            // its profile's environment and secrets.
            let (extra, env, _) = claude_term_env(app, r, p.opts(), p.profile(), false)?;
            args.extend(extra);
            Ok(Spec {
                program: r.program.clone().unwrap(),
                env,
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
        // Names only: values may be secrets.
        "env": spec.env.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
    })))
}

async fn term_ws(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    me: Option<axum::Extension<crate::deck::users::Who>>,
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
    let user = me.map(|m| m.0.user).unwrap_or_else(|| "token".into());
    ws.on_upgrade(move |socket| crate::deck::term::bridge(spec, user, socket))
}

/// The terminals open now, on this machine.
async fn term_sessions() -> Json<Value> {
    Json(json!(crate::deck::term::sessions()))
}

/// End one of them (its program and what it started).
async fn term_close(UrlPath(id): UrlPath<u64>) -> ApiResult<Json<Value>> {
    if !crate::deck::term::close(id) {
        return Err(not_found("no terminal with that id (it may have ended)"));
    }
    Ok(Json(json!({ "closed": id })))
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
        .route(
            "/vendor/symbols-nerd-font-mono.woff2",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "font/woff2"),
                        (header::CACHE_CONTROL, "public, max-age=604800"),
                    ],
                    NERD_FONT,
                )
                    .into_response()
            }),
        )
        .route(
            "/vendor/LICENSE-nerd-fonts",
            get(|| async { asset("text/plain; charset=utf-8", NERD_FONT_LICENSE) }),
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
        .route("/api/run/actions", get(crate::deck::tools::actions))
        .route("/api/run/record", post(crate::deck::tools::record))
        .route("/api/run/publish", post(crate::deck::tools::publish))
        .route("/api/run/post", post(crate::deck::tools::post))
        .route("/api/run/findings", get(crate::deck::tools::findings))
        .route("/api/run/triage", post(crate::deck::tools::triage))
        .route("/api/run/compare", get(crate::deck::tools::compare))
        .route("/api/pr-review", post(crate::deck::tools::pr_review))
        .route("/api/ollama", get(crate::deck::tools::ollama))
        .route("/api/ollama/check", get(crate::deck::tools::ollama_check))
        .route("/api/ollama/pull", post(crate::deck::tools::ollama_pull))
        .route(
            "/api/ollama/default",
            axum::routing::put(crate::deck::tools::ollama_default),
        )
        .route("/api/tools", get(crate::deck::tools::overview))
        .route("/api/tools/cache", post(crate::deck::tools::cache))
        .route(
            "/api/tools/metrics/purge",
            post(crate::deck::tools::metrics_purge),
        )
        .route(
            "/api/tools/skills/sync",
            post(crate::deck::tools::skills_sync),
        )
        .route(
            "/api/tools/skills/install",
            post(crate::deck::tools::skills_install),
        )
        .route("/api/tools/site", post(crate::deck::tools::site))
        .route("/api/tools/dashes", get(crate::deck::tools::dashes))
        .route("/api/market", get(crate::deck::market::list))
        .route("/api/market/refresh", post(crate::deck::market::refresh))
        .route("/api/market/plugins", get(crate::deck::market::plugins))
        .route("/api/market/inspect", post(crate::deck::market::inspect))
        .route("/api/market/install", post(crate::deck::market::install))
        .route("/api/market/native", post(crate::deck::market::native))
        .route(
            "/api/market/installed/{id}",
            axum::routing::delete(crate::deck::market::uninstall),
        )
        .route(
            "/api/market/installed/{id}/update",
            post(crate::deck::market::update),
        )
        .route(
            "/api/delegations",
            get(delegations_list).post(delegate_start),
        )
        .route("/api/delegations/{id}", get(delegation_get))
        .route("/api/delegations/{id}/approve", post(delegation_approve))
        .route("/api/delegations/{id}/cancel", post(delegation_cancel))
        .route("/api/term", get(term_ws))
        .route("/api/term/check", get(term_check))
        .route("/api/workflow-files", axum::routing::put(repo_wf_save))
        .route("/api/workflow-files/delete", post(repo_wf_delete))
        .route("/api/workflow-files/generate", post(wf_generate))
        .route("/api/term/sessions", get(term_sessions))
        .route("/api/term/sessions/{id}/close", post(term_close))
        .route("/api/overview", get(overview))
        .route("/api/session", get(session_get))
        .route("/api/session/password", axum::routing::put(own_password))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/users", get(users_list).post(user_add))
        .route(
            "/api/users/{name}",
            axum::routing::patch(user_update).delete(user_remove),
        )
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
        .route("/api/boards", post(boards_create))
        .route("/api/accounts", get(accounts_get))
        .route("/api/accounts/{key}/wake", post(accounts_wake))
        .route(
            "/api/boards/{id}",
            axum::routing::put(boards_update).delete(boards_delete),
        )
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
        .route("/api/memory/graph", get(memory_graph))
        .route("/api/runs", get(runs_list))
        .route("/api/run", get(run_get))
        .route("/api/run/file", get(run_file))
        .route("/api/run/surface", get(run_surface))
        .route("/api/procs", get(procs_list).post(proc_launch))
        .route("/api/nodes", get(nodes_list))
        .route("/api/nodes/enrol", post(node_enrol))
        .route("/api/nodes/enrol/{code}", get(node_enrol_status))
        .route(
            "/api/nodes/keys",
            get(enroll_key_list).post(enroll_key_create),
        )
        .route(
            "/api/nodes/keys/{id}",
            axum::routing::delete(enroll_key_revoke),
        )
        .route(
            "/api/engagements",
            get(engagements_list).post(engagement_create),
        )
        .route("/api/engagements/{id}/end", post(engagement_end))
        .route("/api/engagements/{id}/audit", get(engagement_audit))
        .route(
            "/api/engagements/{id}/detections",
            post(engagement_detections),
        )
        .route("/api/integrations", get(integrations_list))
        .route("/api/integrations/{name}/test", post(integration_test))
        .route("/api/nodes/{name}", axum::routing::delete(node_remove))
        .route("/api/nodes/{name}/dirs", get(node_dirs))
        .route(
            "/api/nodes/{name}/folders",
            axum::routing::put(node_folders),
        )
        .route("/agent/connect", get(agent_connect))
        .route("/api/nodes/runs", get(node_runs))
        .route("/api/nodes/runs/fetch", post(node_run_fetch))
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
        .route("/api/procs/{id}/workspace", get(proc_workspace))
        .route("/api/procs/{id}/diff", get(proc_diff))
        .route("/api/procs/{id}/files", get(proc_files))
        .route("/api/procs/{id}/file", get(proc_file))
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
            teams: vec![],
            marketplaces: vec![],
            terminal: Default::default(),
            accounts: vec![],
            routing: Default::default(),
            tools: vec![],
            siem: vec![],
            shell_steps: true,
            workflow_errors: vec![],
            repo_dirs: vec![],
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

    /// Workflows kept in a repository: saved there from the editor, used at
    /// once, moved back into indra.toml, and only into known repositories.
    #[tokio::test]
    async fn workflows_are_saved_into_repositories() {
        let dir = std::env::temp_dir().join(format!("indra-repowf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("api")).unwrap();
        let path = dir.join("indra.toml");
        std::fs::write(&path, "[[harness]]\nname = \"h\"\nroot = \".\"\ntargets = [\"api\"]\n\n[[component]]\nname = \"lint\"\nprompt = \"lint {target}\"\n").unwrap();
        let opts = config::LoadOptions {
            config: Some(path.clone()),
            targets: vec![],
            bind: None,
            read_only: false,
            cwd: dir.clone(),
        };
        let app = App::with_config(
            config::load(opts.clone()).unwrap(),
            TOKEN.into(),
            true,
            Some(ConfigAccess {
                opts: opts.clone(),
                path: path.clone(),
                locked: None,
                join_locked: None,
                saving: Mutex::new(()),
            }),
        );
        let origin = ("origin", "http://127.0.0.1:7878");
        let (st, _, body) = call(&app, "GET", "/api/config", &[AUTH], None).await;
        assert_eq!(st, StatusCode::OK);
        let c: Value = serde_json::from_str(&body).unwrap();
        // The default profiles and team come with the config payload.
        assert!(c["builtin"]["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "planner"));
        assert_eq!(c["builtin"]["teams"][0]["name"], "plan-and-build");
        let repo = c["repo"]["repos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["dir"].as_str().unwrap().to_string())
            .find(|d| d.ends_with("api"))
            .expect("the target is a repository");
        let wf = json!({ "name": "triage", "task": [
            { "id": "scan", "skill": "sh-security-review", "profile": "planner" },
            { "id": "lint", "component": "lint", "needs": ["scan"] }
        ]});
        let (st, _, body) = call(
            &app,
            "PUT",
            "/api/workflow-files",
            &[AUTH, origin],
            Some(json!({
                "repo": repo, "workflow": wf,
                "components": [{ "name": "lint", "prompt": "lint {target}" }]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", body);
        let r: Value = serde_json::from_str(&body).unwrap();
        let file = r["file"].as_str().unwrap().to_string();
        assert!(
            file.ends_with(".indra/workflows/triage.toml")
                || file.ends_with(".indra\\workflows\\triage.toml")
        );
        assert!(r["problem"].is_null(), "{}", r["problem"]);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# indra workflow: triage."), "{}", text);
        assert!(
            text.contains("skill = \"sh-security-review\"") && text.contains("[[component]]"),
            "{}",
            text
        );
        // Used at once: runs in the repository, the skill as its command.
        let w = app.settings().workflow("triage").cloned().expect("loaded");
        assert_eq!(w.tasks[0].prompt, "/sh-security-review {target}");
        assert!(w.tasks[0].target.ends_with("api"));
        let (_, _, body) = call(&app, "GET", "/api/workflows", &[AUTH], None).await;
        let l: Value = serde_json::from_str(&body).unwrap();
        let d = l["defs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == "triage")
            .unwrap();
        assert!(d["source"].as_str().unwrap().ends_with("triage.toml"));
        // Renamed in place: the same file, the old name gone.
        let mut wf2 = wf.clone();
        wf2["name"] = json!("triage-2");
        let (st, _, body) = call(
            &app,
            "PUT",
            "/api/workflow-files",
            &[AUTH, origin],
            Some(json!({
                "repo": repo, "workflow": wf2, "original_file": file, "original_name": "triage"
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", body);
        assert!(app.settings().workflow("triage").is_none());
        assert!(app.settings().workflow("triage-2").is_some());
        // Only into a repository this console knows, and only its workflow files.
        let (st, _, _) = call(
            &app,
            "PUT",
            "/api/workflow-files",
            &[AUTH, origin],
            Some(json!({
                "repo": "/etc", "workflow": wf
            })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _, _) = call(
            &app,
            "POST",
            "/api/workflow-files/delete",
            &[AUTH, origin],
            Some(json!({
                "file": path.display().to_string(), "name": "x"
            })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(path.exists(), "indra.toml untouched");
        // Deleting the last workflow deletes the file (its saved task goes with it).
        let (st, _, body) = call(
            &app,
            "POST",
            "/api/workflow-files/delete",
            &[AUTH, origin],
            Some(json!({
                "file": file, "name": "triage-2"
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", body);
        let left = config::read_repo_file(std::path::Path::new(&file)).unwrap();
        assert!(left.workflow.is_empty() && left.component.len() == 1);
        assert!(app.settings().workflow("triage-2").is_none());
        let _ = std::fs::remove_dir_all(&dir);
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
                    join_locked: locked.clone(),
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
        // --lock-config (or read-only) also stops machines joining.
        let (s, _, _) = call(
            &locked,
            "POST",
            "/api/nodes/enrol",
            &[AUTH, origin],
            Some(json!({})),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        // Locked only because the console listens on the network: machines
        // still join with a code (they add only their name).
        let lan = {
            let s = config::load(opts.clone()).unwrap();
            App::with_config(
                s,
                TOKEN.into(),
                true,
                Some(ConfigAccess {
                    opts: opts.clone(),
                    path: path.clone(),
                    locked: Some("listens beyond this machine".into()),
                    join_locked: None,
                    saving: Mutex::new(()),
                }),
            )
        };
        let (s, _, body) = call(&lan, "POST", "/api/nodes/enrol", &[AUTH, origin], None).await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let v: Value = serde_json::from_str(&body).unwrap();
        let code = v["code"].as_str().unwrap();
        assert!(v["command"].as_str().unwrap().ends_with(code));
        let (s, _, body) = call(
            &lan,
            "GET",
            &format!("/api/nodes/enrol/{}", code),
            &[AUTH],
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("\"joined\":null"), "{}", body);
        let (s, _, _) = call(&lan, "GET", "/api/nodes/enrol/NOPE-NOPE", &[AUTH], None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let ov: Value =
            serde_json::from_str(&call(&lan, "GET", "/api/overview", &[AUTH], None).await.2)
                .unwrap();
        assert_eq!(ov["settings"]["join"], true);
        assert_eq!(ov["settings"]["editable"], false);
        // Naming the node (the older flow) edits settings: still locked.
        let (s, _, _) = call(
            &lan,
            "POST",
            "/api/nodes/enrol",
            &[AUTH, origin],
            Some(json!({ "name": "box" })),
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
        let (s, _, _) = post(
            format!("/api/procs/{}/permission", id),
            json!({ "request_id": "perm-1", "allow": true, "answers": { "q": "a" } }),
        )
        .await;
        assert_eq!(
            s,
            StatusCode::BAD_REQUEST,
            "a tool request takes no answers, and stays waiting"
        );
        assert_eq!(view(id).await["phase"], "approval");
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

        // Fresh with another profile and model: the new agent runs with them.
        let mut st = (*app.settings()).clone();
        st.profiles.push(
            serde_json::from_value(json!({ "name": "sharp", "runner": "fake", "model": "opus", "permission_mode": "plan" }))
                .unwrap(),
        );
        app.settings.set(st);
        let (s, _, body) = call(
            &app,
            "POST",
            &format!("/api/procs/{}/pivot", first),
            &[AUTH, origin],
            Some(json!({ "prompt": "check it again", "mode": "fresh", "profile": "sharp", "opts": { "model": "haiku" } })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let fourth = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        let v = wait_done(&app, fourth).await;
        assert_eq!(v["opts"]["permission_mode"], "plan", "{}", v);
        let log = std::fs::read_to_string(&argv).unwrap();
        let last = log.rsplit("--call--").next().unwrap();
        assert!(
            last.contains("--model\nhaiku\n") && last.contains("--permission-mode\nplan\n"),
            "{}",
            last
        );
        // A continued session keeps its runner and profile.
        let (s, _, body) = call(
            &app,
            "POST",
            &format!("/api/procs/{}/pivot", first),
            &[AUTH, origin],
            Some(json!({ "prompt": "x", "profile": "sharp" })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(body.contains("fresh start"), "{}", body);

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

    #[cfg(unix)]
    #[tokio::test]
    async fn open_terminals_are_listed_and_closed_from_the_console() {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as WsMsg};
        let (app, dir) = fixture("ptylist", false);
        app.shell.store(true, std::sync::atomic::Ordering::SeqCst);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = router(app.clone());
        tokio::spawn(async move { axum::serve(listener, served).await });
        let mut r = format!("ws://{}/api/term?what=shell&target=0", addr)
            .into_client_request()
            .unwrap();
        r.headers_mut()
            .insert("origin", format!("http://{}", addr).parse().unwrap());
        r.headers_mut()
            .insert("authorization", AUTH.1.parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(r).await.unwrap();
        // The first frame names the terminal.
        let id = match tokio::time::timeout(std::time::Duration::from_secs(10), ws.next()).await {
            Ok(Some(Ok(WsMsg::Text(t)))) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                assert_eq!(v["t"], "session");
                assert!(v["label"].as_str().unwrap().starts_with("shell in "));
                v["id"].as_u64().unwrap()
            }
            _ => panic!("no session frame"),
        };
        let origin = ("origin", "http://127.0.0.1:7878");
        let (st, _, body) = call(&app, "GET", "/api/term/sessions", &[AUTH], None).await;
        assert_eq!(st, StatusCode::OK);
        let list: Value = serde_json::from_str(&body).unwrap();
        let mine = list
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["id"] == id)
            .expect("listed");
        assert_eq!(mine["user"], "token");
        assert!(mine["cwd"].as_str().unwrap().ends_with("target"));
        // Start something in it that would outlive a careless close.
        ws.send(WsMsg::Binary(b"sleep 300 & echo bg-$!\n".to_vec().into()))
            .await
            .unwrap();
        // The job's pid, once the shell has printed it.
        let pid = |seen: &str| -> Option<i32> {
            seen.split("bg-").skip(1).find_map(|x| {
                let d: String = x.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !d.is_empty() && x[d.len()..].starts_with(['\r', '\n']) {
                    d.parse().ok()
                } else {
                    None
                }
            })
        };
        let mut seen = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let bg = loop {
            if let Some(p) = pid(&seen) {
                break p;
            }
            assert!(std::time::Instant::now() < deadline, "got: {}", seen);
            match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(WsMsg::Binary(b)))) => seen.push_str(&String::from_utf8_lossy(&b)),
                Ok(Some(Ok(_))) => {}
                _ => panic!("closed early; got: {}", seen),
            }
        };
        // Close it from the console.
        let close = format!("/api/term/sessions/{}/close", id);
        let (st, _, _) = call(&app, "POST", &close, &[AUTH, origin], None).await;
        assert_eq!(st, StatusCode::OK);
        // The page sees it end, and it leaves the list.
        let mut ended = false;
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(WsMsg::Binary(b)))) => {
                    if String::from_utf8_lossy(&b).contains("ended") {
                        ended = true;
                    }
                }
                Ok(Some(Ok(_))) => {}
                _ => break,
            }
        }
        assert!(ended);
        let (_, _, body) = call(&app, "GET", "/api/term/sessions", &[AUTH], None).await;
        let list: Value = serde_json::from_str(&body).unwrap();
        assert!(!list.as_array().unwrap().iter().any(|x| x["id"] == id));
        // What it started in the background went with it.
        let mut gone = false;
        for _ in 0..40 {
            if unsafe { libc::kill(bg, 0) } != 0 {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(gone, "background job {} survived the close", bg);
        // Closing it again: it is gone.
        let (st, _, _) = call(&app, "POST", &close, &[AUTH, origin], None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The fixture, with a memory vault (`vault/` under its root) on its harness.
    #[tokio::test]
    async fn engagements_bind_agents_to_their_scope() {
        let (base, dir) = fixture("engage", false);
        let mut settings = (*base.settings()).clone();
        settings.runners.push(Runner {
            name: "fakeclaude".into(),
            command: "echo".into(),
            args: vec!["-p".into(), "{prompt}".into()],
            format: "claude-stream-json".into(),
            resume_args: vec![],
            fork_args: vec![],
            model_args: vec![],
            program: crate::deck::config::which("echo"),
        });
        let app = App::new(settings, TOKEN.into(), true);
        let origin = ("origin", "http://127.0.0.1:7878");
        let t = crate::deck::engage::now();
        let new = |m: Value| {
            let mut b = json!({ "name": "Q4 web test", "machine": "local", "scope": ["app.example.com"],
                "starts": t - 10, "ends": t + 3600, "authorization": "SOW #42, signed by the app owner" });
            for (k, v) in m.as_object().unwrap() {
                b[k] = v.clone();
            }
            b
        };
        let (s, _, b) = call(
            &app,
            "POST",
            "/api/engagements",
            &[AUTH, origin],
            Some(new(json!({}))),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let id = serde_json::from_str::<Value>(&b).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        for bad in [
            json!({ "authorization": "ok" }),
            json!({ "machine": "nope" }),
            json!({ "scope": [] }),
            json!({ "ends": t - 20 }),
        ] {
            let (s, _, b) = call(
                &app,
                "POST",
                "/api/engagements",
                &[AUTH, origin],
                Some(new(bad.clone())),
            )
            .await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{} {}", bad, b);
        }
        let launch = |body: Value| {
            let app = app.clone();
            async move { call(&app, "POST", "/api/procs", &[AUTH, origin], Some(body)).await }
        };
        // Only Claude Code, whose hooks enforce it.
        let (s, _, b) =
            launch(json!({ "runner": "echo", "prompt": "go", "target": 0, "engagement": id }))
                .await;
        assert!(
            s == StatusCode::BAD_REQUEST && b.contains("Claude Code"),
            "{}",
            b
        );
        let (s, _, b) = launch(
            json!({ "runner": "fakeclaude", "prompt": "go", "target": 0, "engagement": id }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let pid = serde_json::from_str::<Value>(&b).unwrap()["id"]
            .as_u64()
            .unwrap();
        let v = json!(app.procs.get(pid).unwrap().view());
        assert_eq!(v["opts"]["engagement"]["id"], json!(id));
        let cmd = v["command"].to_string();
        assert!(
            cmd.contains("--settings") && cmd.contains("guard pre") && cmd.contains("guard post"),
            "{}",
            cmd
        );
        // A request cannot bring its own engagement.
        let forged = json!({ "id": "zzzz9999", "name": "x", "scope": ["*.com"], "starts": 0, "ends": t + 9, "rate": 50.0 });
        let (s, _, b) = launch(json!({ "runner": "fakeclaude", "prompt": "go", "target": 0, "opts": { "engagement": forged } })).await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let pid2 = serde_json::from_str::<Value>(&b).unwrap()["id"]
            .as_u64()
            .unwrap();
        assert!(app
            .procs
            .get(pid2)
            .unwrap()
            .view()
            .opts
            .engagement
            .is_none());
        // Not on another machine.
        let (s, _, _) = launch(json!({ "node": "box", "runner": "fakeclaude", "prompt": "go", "target": 0, "engagement": id })).await;
        assert_ne!(s, StatusCode::OK);
        // The audit log: created and launched, chained.
        let (s, _, b) = call(
            &app,
            "GET",
            &format!("/api/engagements/{}/audit", id),
            &[AUTH],
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let a: Value = serde_json::from_str(&b).unwrap();
        let events: Vec<&str> = a["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["event"].as_str())
            .collect();
        assert!(
            events.contains(&"created") && events.contains(&"launch"),
            "{:?}",
            events
        );
        assert_eq!(a["intact"], true);
        let (s, h, b) = call(
            &app,
            "GET",
            &format!("/api/engagements/{}/audit?format=csv", id),
            &[AUTH],
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert!(
            h["content-type"].to_str().unwrap().starts_with("text/csv")
                && b.starts_with("ts,event,")
        );
        // Ended: nothing more starts in it.
        let (s, _, b) = call(
            &app,
            "POST",
            &format!("/api/engagements/{}/end", id),
            &[AUTH, origin],
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        assert_eq!(serde_json::from_str::<Value>(&b).unwrap()["state"], "ended");
        let (s, _, b) = launch(
            json!({ "runner": "fakeclaude", "prompt": "go", "target": 0, "engagement": id }),
        )
        .await;
        assert!(
            s == StatusCode::BAD_REQUEST && b.contains("not active"),
            "{}",
            b
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn detections_match_steps_to_siem_events() {
        let (base, dir) = fixture("detect", false);
        let logs = dir.join("siem");
        std::fs::create_dir_all(&logs).unwrap();
        let mut settings = (*base.settings()).clone();
        let t = crate::deck::integrations::ToolCfg {
            name: "logs".into(),
            kind: "files".into(),
            paths: vec![logs.join("*.log").display().to_string()],
            ..Default::default()
        };
        settings.siem.push(t.source().unwrap());
        settings.tools.push(t);
        let app = App::new(settings, TOKEN.into(), true);
        let origin = ("origin", "http://127.0.0.1:7878");
        let t = crate::deck::engage::now();
        let (s, _, b) = call(&app, "POST", "/api/engagements", &[AUTH, origin], Some(json!({ "name": "purple", "machine": "local",
            "scope": ["app.example.com", "10.20.0.0/24"], "starts": t - 10, "ends": t + 3600, "authorization": "Purple team exercise, ticket SEC-77" }))).await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let id = serde_json::from_str::<Value>(&b).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        // Before any step: nothing to check.
        let (s, _, b) = call(
            &app,
            "POST",
            &format!("/api/engagements/{}/detections", id),
            &[AUTH, origin],
            None,
        )
        .await;
        assert!(
            s == StatusCode::BAD_REQUEST && b.contains("no steps"),
            "{}",
            b
        );
        // Two steps; the SIEM saw the first.
        let d = crate::deck::engage::dir_of(&app.settings().data_dir, &id);
        for (tool, target) in [("Bash", "app.example.com"), ("Bash", "10.20.0.0/24")] {
            crate::deck::engage::audit(&d, json!({ "event": "tool", "tool": tool, "targets": [target], "decision": "allow", "input": "x" })).unwrap();
        }
        crate::deck::engage::audit(&d, json!({ "event": "tool", "tool": "Bash", "targets": ["evil.example.net"], "decision": "deny" })).unwrap();
        let now_ms = t * 1000 + 5_000;
        std::fs::write(logs.join("waf.log"), format!(
            "{{\"@timestamp\":{},\"rule\":{{\"description\":\"SQLi attempt\",\"level\":12,\"mitre\":{{\"id\":[\"T1190\"]}}}},\"message\":\"blocked GET app.example.com/login\"}}\n{{\"@timestamp\":{},\"message\":\"unrelated www.example.org\"}}\n",
            now_ms, now_ms)).unwrap();
        let (s, _, b) = call(
            &app,
            "POST",
            &format!("/api/engagements/{}/detections", id),
            &[AUTH, origin],
            Some(json!({})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let r: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(
            r["summary"],
            json!({ "steps": 2, "detected": 1, "missed": 1 }),
            "{}",
            b
        );
        assert_eq!(r["steps"][0]["alerts"][0]["rule"], "SQLi attempt");
        assert_eq!(r["techniques"], json!(["T1190"]));
        assert_eq!(r["steps"][1]["detected"], false);
        assert!(d.join("detections.json").is_file());
        let (s, _, b) = call(&app, "GET", "/api/integrations", &[AUTH], None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<Value>(&b).unwrap()[0]["name"],
            "logs"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn agents_get_their_tools_and_nothing_else() {
        let (base, dir) = fixture("mcp-tools", false);
        let mut settings = (*base.settings()).clone();
        settings.runners.push(Runner {
            name: "fakeclaude".into(),
            command: "echo".into(),
            args: vec!["-p".into(), "{prompt}".into()],
            format: "claude-stream-json".into(),
            resume_args: vec![],
            fork_args: vec![],
            model_args: vec![],
            program: crate::deck::config::which("echo"),
        });
        let mcp = |name: &str, profiles: &[&str]| crate::deck::integrations::ToolCfg {
            name: name.into(),
            kind: "mcp".into(),
            command: "intel-server".into(),
            profiles: profiles.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        };
        settings.tools = vec![mcp("intel", &["*"]), mcp("tickets", &[])];
        let app = App::new(settings, TOKEN.into(), true);
        let origin = ("origin", "http://127.0.0.1:7878");
        let forged = json!({ "evil": { "command": "sh", "args": ["-c", "id"] } });
        let (s, _, b) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "fakeclaude", "prompt": "go", "target": 0,
            "tools": ["tickets"], "opts": { "mcp_servers": forged } })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let id = serde_json::from_str::<Value>(&b).unwrap()["id"]
            .as_u64()
            .unwrap();
        let p = app.procs.get(id).unwrap();
        let names: Vec<String> = p.view().opts.mcp_servers.keys().cloned().collect();
        assert_eq!(
            names,
            ["intel", "tickets"],
            "its profile's (*) and its pick; never the request's own"
        );
        let cmd = p.view().command;
        let i = cmd
            .iter()
            .position(|a| a == "--mcp-config")
            .expect("an MCP config");
        let file = std::path::PathBuf::from(&cmd[i + 1]);
        assert_eq!(i + 2, cmd.len(), "last");
        for _ in 0..200 {
            if !p.is_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            !file.exists(),
            "the config, with its secrets, is gone once the agent exits"
        );
        let (s, _, b) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "fakeclaude", "prompt": "go", "target": 0, "tools": ["nope"] })),
        )
        .await;
        assert!(s == StatusCode::BAD_REQUEST && b.contains("nope"), "{}", b);
        let (s, _, b) = call(&app, "GET", "/api/integrations", &[AUTH], None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<Value>(&b).unwrap()[0]["transport"],
            "stdio"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

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
bad = False
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
    if "Before you finish" in c and bad:
        out({"type": "result", "subtype": "success", "is_error": True, "num_turns": 1, "duration_ms": 5, "result": "API Error: 500 read GGUF metadata"})
    elif "Before you finish" in c:
        ans = '[{"title": "Use make db", "body": "The tests need Postgres.", "tags": ["tests"]}]'
        out({"type": "assistant", "message": {"content": [{"type": "text", "text": ans}], "usage": {"input_tokens": 10, "cache_read_input_tokens": 3000, "output_tokens": 40}}})
        out({"type": "result", "subtype": "success", "num_turns": 1, "duration_ms": 5, "result": ans, "modelUsage": {"m": {"contextWindow": 123456}}})
    else:
        bad = "BADREFLECT" in c
        out({"type": "system", "subtype": "init", "model": "m", "session_id": "s-9"})
        out({"type": "assistant", "message": {"content": [{"type": "text", "text": "done"}], "usage": {"input_tokens": 1000, "cache_read_input_tokens": 2000, "output_tokens": 50}}, "session_id": "s-9"})
        out({"type": "result", "subtype": "success", "is_error": "FAIL" in c, "num_turns": 1, "duration_ms": 5, "result": "API Error: 500" if "FAIL" in c else "done", "session_id": "s-9"})
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

        // A run that ended in an error is not asked for learnings.
        let before = std::fs::read_to_string(&log).unwrap().len();
        let (s, _, body) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "reflector", "prompt": "FAIL the review", "target": 0, "reflect": true })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        for _ in 0..400 {
            v = json!(app.procs.get(id).unwrap().view());
            if v["status"] != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(v["reflection"], "none", "{}", v);
        let sent = std::fs::read_to_string(&log).unwrap();
        assert!(!sent[before..].contains("Before you finish"));

        // An answer to the learnings question that is an error is not a learning.
        let (s, _, body) = call(
            &app,
            "POST",
            "/api/procs",
            &[AUTH, origin],
            Some(json!({ "runner": "reflector", "prompt": "BADREFLECT the review", "target": 0, "reflect": true })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", body);
        let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_u64()
            .unwrap();
        for _ in 0..400 {
            if app.procs.get(id).unwrap().view().status != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let sent = std::fs::read_to_string(&log).unwrap();
        assert!(
            sent.contains("BADREFLECT") && sent.matches("Before you finish").count() == 2,
            "it was asked"
        );
        crate::deck::knowledge::harvest(&app);
        assert_eq!(app.learnings.list().len(), 1, "only the one from before");
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

    #[cfg(unix)]
    const PLANNER_AND_WORKER: &str = r#"#!/usr/bin/env python3
import json, os, sys
log = open(os.environ["FAKE_DELEGATE_LOG"], "a")
def out(o):
    print(json.dumps(o), flush=True)
def recv():
    l = sys.stdin.readline()
    if not l:
        sys.exit(0)
    return json.loads(l)
m = recv()
out({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {}}})
while True:
    m = recv()
    if m["type"] != "user":
        continue
    c = m["message"]["content"]
    log.write(json.dumps({"model": sys.argv[sys.argv.index("--model") + 1] if "--model" in sys.argv else None, "prompt": c}) + "\n"); log.flush()
    if "You are the planner" in c:
        t = 'Plan:\n{"summary": "Two steps", "steps": [{"id": "one", "title": "First", "prompt": "do one"}, {"id": "two", "title": "Second", "prompt": "do two", "size": "light"}]}'
        out({"type": "result", "subtype": "success", "num_turns": 1, "duration_ms": 5, "result": t, "total_cost_usd": 0.04})
    else:
        out({"type": "result", "subtype": "success", "num_turns": 1, "duration_ms": 5, "result": "done", "total_cost_usd": 0.001})
"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn a_strong_model_plans_and_cheaper_ones_build() {
        use std::os::unix::fs::PermissionsExt;
        let (base, dir) = fixture("delegate", false);
        let script = dir.join("agent.py");
        let log = dir.join("delegate.log");
        std::fs::write(&script, PLANNER_AND_WORKER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("FAKE_DELEGATE_LOG", &log);
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
            resume_args: vec![],
            fork_args: vec![],
            model_args: vec![],
            program: Some(script.clone()),
        });
        for (name, model) in [
            ("planner", "opus"),
            ("builder", "haiku"),
            ("local", "qwen3-coder"),
        ] {
            settings.profiles.push(config::ProfileCfg {
                name: name.into(),
                runner: Some("fake".into()),
                model: Some(model.into()),
                ..Default::default()
            });
        }
        settings.teams.push(config::TeamCfg {
            name: "t".into(),
            planner: "planner".into(),
            worker: "builder".into(),
            light_worker: Some("local".into()),
            reviewer: Some("planner".into()),
            ..Default::default()
        });
        let app = App::new(settings, TOKEN.into(), true);
        let origin = ("origin", "http://127.0.0.1:7878");
        let (s, _, b) = call(
            &app,
            "POST",
            "/api/delegations",
            &[AUTH, origin],
            Some(json!({ "prompt": "add a flag", "team": "nope" })),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", b);
        let (s, _, b) = call(
            &app,
            "POST",
            "/api/delegations",
            &[AUTH, origin],
            Some(json!({ "prompt": "add a flag", "team": "t", "target": 0 })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let id = serde_json::from_str::<Value>(&b).unwrap()["id"]
            .as_u64()
            .unwrap();
        let get = || {
            let app = app.clone();
            async move {
                let (_, _, b) = call(
                    &app,
                    "GET",
                    &format!("/api/delegations/{}", id),
                    &[AUTH],
                    None,
                )
                .await;
                serde_json::from_str::<Value>(&b).unwrap()
            }
        };
        let mut v = json!(null);
        for _ in 0..400 {
            crate::deck::delegate::tick(&app);
            v = get().await;
            if v["delegation"]["status"] != "planning" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(v["delegation"]["status"], "review", "{}", v);
        assert_eq!(
            v["delegation"]["plan"]["steps"].as_array().unwrap().len(),
            2
        );
        // Edit before running: a person rewrites the first step.
        let mut plan = v["delegation"]["plan"].clone();
        plan["steps"][0]["prompt"] = json!("do one, carefully");
        let (s, _, b) = call(
            &app,
            "POST",
            &format!("/api/delegations/{}/approve", id),
            &[AUTH, origin],
            Some(json!({ "plan": plan })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        for _ in 0..800 {
            crate::deck::delegate::tick(&app);
            v = get().await;
            if v["delegation"]["status"] != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(v["delegation"]["status"], "done", "{}", v);
        let roles: Vec<(String, String)> = v["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["id"].as_str().unwrap().to_string(),
                    s["role"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            roles,
            vec![
                ("one".into(), "worker".into()),
                ("two".into(), "light_worker".into()),
                ("review".into(), "reviewer".into())
            ]
        );
        assert!(
            (v["total_cost_usd"].as_f64().unwrap() - 0.043).abs() < 1e-9,
            "{}",
            v
        );
        // Who ran what: the planner on opus, the steps on cheaper models.
        let calls: Vec<Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let model_for = |needle: &str| {
            calls
                .iter()
                .find(|c| c["prompt"].as_str().unwrap().contains(needle))
                .map(|c| c["model"].as_str().unwrap_or("").to_string())
                .unwrap()
        };
        assert_eq!(model_for("You are the planner"), "opus");
        assert_eq!(model_for("do one, carefully"), "haiku");
        assert_eq!(model_for("Your step (two)"), "qwen3-coder");
        assert_eq!(model_for("Review their work"), "opus");
        // Steps run in order by default.
        let order: Vec<usize> = ["Your step (one)", "Your step (two)", "Review their work"]
            .iter()
            .map(|n| {
                calls
                    .iter()
                    .position(|c| c["prompt"].as_str().unwrap().contains(n))
                    .unwrap()
            })
            .collect();
        assert!(order[0] < order[1] && order[1] < order[2]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn review_tools_from_the_console() {
        let (app, dir) = fixture("tools", false);
        let tmp = dir.join("state");
        std::env::set_var("SH_REVIEW_CACHE_DIR", tmp.join("cache"));
        std::env::set_var("SH_METRICS_DIR", tmp.join("metrics"));
        let run = dir.join("target/runs/pr-7-20260101");
        let w = |rel: &str, s: &str| {
            let p = run.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        w("verified.jsonl", "{\"id\":\"F1\",\"title\":\"SQL injection in login\",\"vuln_class\":\"sqli\",\"severity\":\"high\",\"status\":\"verified\",\"file\":\"app/login.py\",\"line\":12}\n{\"id\":\"F2\",\"title\":\"Not a bug\",\"vuln_class\":\"xss\",\"severity\":\"low\",\"status\":\"false-positive\",\"file\":\"app/view.py\",\"line\":3}\n");
        w("run.json", "{\"repo\":\"acme/shop\",\"pr\":7}");
        w(
            "review/review-payload.json",
            "{\"body\":\"one finding\",\"event\":\"COMMENT\",\"comments\":[]}",
        );
        w(
            "review/verdict.json",
            "{\"head_sha\":\"abc123\",\"state\":\"failure\",\"description\":\"1 finding\"}",
        );
        let key = "0:pr-7-20260101";

        // What the run supports.
        let (st, _, b) = call(
            &app,
            "GET",
            &format!("/api/run/actions?key={}", key),
            &[AUTH],
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let a: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(a["findings"], true);
        assert_eq!(a["review"], true);
        assert_eq!(a["repo"], "acme/shop");
        assert_eq!(a["pr"], "7");
        let (st, _, _) = call(&app, "GET", "/api/run/actions?key=0:nope", &[AUTH], None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);

        // Record: a preview writes nothing, then the real thing writes the vault.
        let vault = dir.join("target/runs/memory");
        let (st, _, b) = call(
            &app,
            "POST",
            "/api/run/record",
            &[AUTH],
            Some(json!({ "key": key, "dry_run": true })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let r: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(r["new"], 1);
        assert_eq!(r["false-positive"], 1);
        assert!(!r["commands"].as_array().unwrap().is_empty());
        assert!(!vault.exists());
        let (st, _, b) = call(
            &app,
            "POST",
            "/api/run/record",
            &[AUTH],
            Some(json!({ "key": key })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        assert!(vault.is_dir());

        // Publish: the plan, never the false positive.
        let (st, _, b) = call(
            &app,
            "POST",
            "/api/run/publish",
            &[AUTH],
            Some(json!({ "key": key, "to": "github", "min_severity": "low" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let p: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(p["records"].as_array().unwrap().len(), 1);
        assert!(!b.contains("Not a bug"));
        for bad in [
            json!({ "key": key, "to": "slack" }),
            json!({ "key": key, "to": "jira", "statuses": ["false-positive"] }),
        ] {
            let (st, _, _) = call(&app, "POST", "/api/run/publish", &[AUTH], Some(bad)).await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
        }

        // Post: a dry run sends nothing; a bad repository never reaches gh.
        let (st, _, b) = call(
            &app,
            "POST",
            "/api/run/post",
            &[AUTH],
            Some(json!({ "key": key, "dry_run": true })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let r: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(r["ok"], true);
        assert!(
            b.contains("acme/shop#7") && b.contains("[dry-run]"),
            "{}",
            b
        );
        for (repo, pr) in [("../..", "7"), ("a/b/c", "7"), ("acme/shop", "7; rm")] {
            let (st, _, _) = call(
                &app,
                "POST",
                "/api/run/post",
                &[AUTH],
                Some(json!({ "key": key, "repo": repo, "pr": pr, "dry_run": true })),
            )
            .await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "{} {}", repo, pr);
        }

        // The Tools page.
        let (st, _, b) = call(&app, "GET", "/api/tools", &[AUTH], None).await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let t: Value = serde_json::from_str(&b).unwrap();
        assert!(t["cache"]["root"]
            .as_str()
            .unwrap()
            .starts_with(tmp.join("cache").to_str().unwrap()));
        assert_eq!(
            t["metrics_path"]["dir"],
            tmp.join("metrics").display().to_string()
        );
        let (st, _, b) = call(
            &app,
            "POST",
            "/api/tools/cache",
            &[AUTH],
            Some(json!({ "action": "clear", "repo": ".." })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{}", b);
        let (st, _, _) = call(
            &app,
            "POST",
            "/api/tools/cache",
            &[AUTH],
            Some(json!({ "action": "prune", "max_age_days": 1 })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _, _) = call(
            &app,
            "POST",
            "/api/tools/cache",
            &[AUTH],
            Some(json!({ "action": "drop" })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _, b) = call(
            &app,
            "POST",
            "/api/tools/metrics/purge",
            &[AUTH],
            Some(json!({ "older_than_days": 30 })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);

        // Skills into a workspace (the repository is the clone the tests run in).
        if t["repo"].is_string() {
            let (st, _, b) = call(
                &app,
                "POST",
                "/api/tools/skills/install",
                &[AUTH],
                Some(json!({ "to": "agents", "scope": "workspace", "target": 0 })),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{}", b);
            let r: Value = serde_json::from_str(&b).unwrap();
            assert!(r["skills"].as_u64().unwrap() > 0);
            assert!(dir
                .join("target/.agents/skills/sh-security-review/SKILL.md")
                .is_file());
            let (st, _, _) = call(
                &app,
                "POST",
                "/api/tools/skills/install",
                &[AUTH],
                Some(json!({ "to": "elsewhere" })),
            )
            .await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
        }
        let _ = std::fs::remove_dir_all(&dir);

        // Read-only: previews work, writes are refused.
        let (app, dir) = fixture("tools-ro", true);
        let run = dir.join("target/runs/pr-7");
        std::fs::create_dir_all(run.join("review")).unwrap();
        std::fs::write(run.join("verified.jsonl"), "").unwrap();
        std::fs::write(run.join("review/review-payload.json"), "{}").unwrap();
        for (uri, body) in [
            ("/api/run/record", json!({ "key": "0:pr-7" })),
            (
                "/api/run/post",
                json!({ "key": "0:pr-7", "repo": "a/b", "pr": "1" }),
            ),
            ("/api/tools/cache", json!({ "action": "clear" })),
            ("/api/tools/metrics/purge", json!({})),
            ("/api/tools/skills/sync", json!({})),
            ("/api/tools/skills/install", json!({ "to": "agents" })),
            ("/api/tools/site", json!({})),
        ] {
            let (st, _, b) = call(&app, "POST", uri, &[AUTH], Some(body)).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{} {}", uri, b);
        }
        let (st, _, b) = call(
            &app,
            "POST",
            "/api/run/record",
            &[AUTH],
            Some(json!({ "key": "0:pr-7", "dry_run": true })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn triage_compare_and_pr_reviews() {
        let (app, dir) = fixture("triage", false);
        let w = |run: &str, body: &str| {
            let p = dir.join("target/runs").join(run);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("verified.jsonl"), body).unwrap();
        };
        let f = |id: &str, title: &str, file: &str, status: &str| {
            json!({ "id": id, "title": title, "class": "sqli", "severity": "high", "status": status, "file": file, "line": 3 }).to_string() + "\n"
        };
        // Base: A (open), B (open), C (false positive). Head: A, C, D new; B gone.
        w(
            "run-1",
            &(f("A", "SQLi in login", "a.py", "verified")
                + &f("B", "SQLi in search", "b.py", "verified")
                + &f("C", "Escaped", "c.py", "false-positive")),
        );
        w(
            "run-2",
            &(f("A", "SQLi in login", "a.py", "verified")
                + "not json\n"
                + &f("C", "Escaped", "c.py", "verified")
                + &f("D", "SQLi in export", "d.py", "needs-runtime")),
        );

        let (st, _, b) = call(&app, "GET", "/api/run/findings?key=0:run-2", &[AUTH], None).await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let list: Value = serde_json::from_str(&b).unwrap();
        let list = list["findings"].as_array().unwrap().clone();
        assert_eq!(list.len(), 3);
        assert_eq!(list[1]["index"], 2); // the bad line keeps its place

        let (st, _, b) = call(
            &app,
            "GET",
            "/api/run/compare?key=0:run-2&with=0:run-1",
            &[AUTH],
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let c: Value = serde_json::from_str(&b).unwrap();
        fn ids2(c: &Value, k: &str) -> Vec<String> {
            c[k].as_array()
                .unwrap()
                .iter()
                .map(|x| {
                    x.get("finding").unwrap_or(x)["id"]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect()
        }
        let ids = |k: &str| ids2(&c, k);
        let mut new = ids("new");
        new.sort();
        assert_eq!(new, vec!["C", "D"]); // C was a false positive in run-1
        assert_eq!(ids("fixed"), vec!["B"]);
        assert_eq!(ids("still_open"), vec!["A"]);
        assert_eq!(ids("changed"), vec!["C"]);

        // Triage: a false positive needs a reason; the old status is kept;
        // a stale fingerprint is refused; the unparsable line survives.
        let c_fp = list.iter().find(|x| x["id"] == "C").unwrap()["fingerprint"].clone();
        let body = |status: &str, reason: &str, fp: &Value| json!({ "key": "0:run-2", "index": 2, "fingerprint": fp, "status": status, "reason": reason });
        let (st, _, _) = call(
            &app,
            "POST",
            "/api/run/triage",
            &[AUTH],
            Some(body("false-positive", "", &c_fp)),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _, _) = call(
            &app,
            "POST",
            "/api/run/triage",
            &[AUTH],
            Some(body("fixed", "x", &c_fp)),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _, _) = call(
            &app,
            "POST",
            "/api/run/triage",
            &[AUTH],
            Some(body("verified", "", &json!("0123456789ab"))),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _, b) = call(
            &app,
            "POST",
            "/api/run/triage",
            &[AUTH],
            Some(body("false-positive", "escaped by the ORM", &c_fp)),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let text = std::fs::read_to_string(dir.join("target/runs/run-2/verified.jsonl")).unwrap();
        assert!(text.contains("not json\n"));
        let c_line: Value = serde_json::from_str(text.lines().nth(2).unwrap()).unwrap();
        assert_eq!(c_line["status"], "false-positive");
        assert_eq!(c_line["triage"]["from"], "verified");
        assert_eq!(c_line["triage"]["reason"], "escaped by the ORM");
        // Now C is closed in both runs, so it is neither new nor fixed.
        let (_, _, b) = call(
            &app,
            "GET",
            "/api/run/compare?key=0:run-2&with=0:run-1",
            &[AUTH],
            None,
        )
        .await;
        let c: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(ids2(&c, "new"), vec!["D"]);
        assert!(ids2(&c, "changed").is_empty());
        // The memory recorder writes the person's reason into the learning.
        let vault = dir.join("target/runs/memory");
        crate::mem::record::record(&dir.join("target/runs/run-2"), &vault, false).unwrap();
        let mut found = false;
        for e in std::fs::read_dir(vault.join("learnings"))
            .unwrap()
            .flatten()
        {
            if std::fs::read_to_string(e.path())
                .unwrap()
                .contains("triaged: verified -> false-positive: escaped by the ORM")
            {
                found = true;
            }
        }
        assert!(found);

        // A PR review is a launch of /sh-pr-review with checked arguments.
        for bad in [
            json!({ "pr": "https://evil.example/owner/repo/pull/1", "target": 0 }),
            json!({ "pr": "12; rm -rf /", "target": 0 }),
            json!({ "pr": "https://github.com/../x/pull/1", "target": 0 }),
            json!({ "pr": "7", "target": 0, "tier": 9 }),
            json!({ "pr": "7", "target": 0, "fail_on": "everything" }),
        ] {
            let (st, _, b) = call(&app, "POST", "/api/pr-review", &[AUTH], Some(bad.clone())).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "{} {}", bad, b);
        }
        let (st, _, b) = call(&app, "POST", "/api/pr-review", &[AUTH], Some(json!({ "pr": "https://github.com/acme/shop/pull/7", "target": 0, "runner": "echo", "tier": 2, "deep": true, "fail_on": "high" }))).await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let r: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(r["prompt"], "/sh-pr-review https://github.com/acme/shop/pull/7 --tier=2 --deep --fail-on=high --dry-run");
        assert!(r["id"].as_u64().is_some());
        let _ = std::fs::remove_dir_all(&dir);

        let (app, dir) = fixture("triage-ro", true);
        let p = dir.join("target/runs/run-1");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("verified.jsonl"), f("A", "x", "a.py", "verified")).unwrap();
        let (st, _, _) = call(
            &app,
            "POST",
            "/api/run/triage",
            &[AUTH],
            Some(json!({ "key": "0:run-1", "index": 0, "fingerprint": "x", "status": "verified" })),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        let (st, _, _) = call(
            &app,
            "POST",
            "/api/pr-review",
            &[AUTH],
            Some(json!({ "pr": "7", "target": 0 })),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_terminal_runs_claude_code_with_a_profile_on_ollama() {
        let (app, dir) = fixture("term-ollama", false);
        let mut s = (*app.settings()).clone();
        s.runners.push(Runner {
            name: "claude".into(),
            command: "claude".into(),
            args: vec!["-p".into(), "{prompt}".into()],
            format: "claude-stream-json".into(),
            resume_args: vec!["--resume".into(), "{session}".into()],
            fork_args: vec![],
            model_args: vec![],
            program: crate::deck::config::which("sleep"),
        });
        s.profiles.push(
            serde_json::from_value(json!({
                "name": "local", "runner": "claude", "model": "qwen3-coder", "provider": "ollama",
                "ollama_url": "http://gpu:11434", "permission_mode": "acceptEdits",
                "env": { "FOO": "secret:missing" }
            }))
            .unwrap(),
        );
        s.profiles.push(
            serde_json::from_value(json!({ "name": "plain", "runner": "claude", "model": "opus" }))
                .unwrap(),
        );
        app.settings.set(s);
        app.shell.store(true, std::sync::atomic::Ordering::SeqCst);
        let check = |q: &str| format!("/api/term/check?what=claude&target=0&{}", q);
        // A profile's secret that is not set is refused, not dropped.
        let (st, _, b) = call(&app, "GET", &check("profile=local"), &[AUTH], None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{}", b);
        assert!(b.contains("missing"), "{}", b);
        let mut s = (*app.settings()).clone();
        s.profiles[0].env.clear();
        app.settings.set(s);
        let (st, _, b) = call(&app, "GET", &check("profile=local"), &[AUTH], None).await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        let v: Value = serde_json::from_str(&b).unwrap();
        let args: Vec<String> = serde_json::from_value(v["args"].clone()).unwrap();
        assert!(
            args.windows(2).any(|w| w == ["--model", "qwen3-coder"]),
            "{:?}",
            args
        );
        assert!(args
            .windows(2)
            .any(|w| w == ["--permission-mode", "acceptEdits"]));
        let env = v["env"].to_string();
        for k in [
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_MODEL",
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ] {
            assert!(env.contains(k), "{} in {}", k, env);
        }
        assert!(v["label"]
            .as_str()
            .unwrap()
            .contains("qwen3-coder (Ollama)"));
        // The spec carries the values; the router fronts the server.
        let q: TermQ =
            serde_json::from_value(json!({ "what": "claude", "target": 0, "profile": "local" }))
                .unwrap();
        let Ok(spec) = term_spec(&app, &q) else {
            panic!("term_spec failed")
        };
        let base = spec
            .env
            .iter()
            .find(|(k, _)| k == "ANTHROPIC_BASE_URL")
            .unwrap()
            .1
            .clone();
        assert!(base.starts_with("http://127.0.0.1:"), "{}", base);
        // Without Ollama nothing is added; an unknown profile is refused.
        let (_, _, b) = call(&app, "GET", &check("profile=plain"), &[AUTH], None).await;
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["env"], json!([]));
        let (st, _, _) = call(&app, "GET", &check("profile=nobody"), &[AUTH], None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn users_roles_and_sessions() {
        let (app, dir) = fixture("users", false);
        let token_cookie = format!("{}={}", COOKIE, TOKEN);
        // No users: the token (cookie or Bearer) is the key, as before.
        let (_, _, b) = call(&app, "GET", "/api/session", &[], None).await;
        assert_eq!(serde_json::from_str::<Value>(&b).unwrap()["users"], false);
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/procs",
                &[("cookie", token_cookie.as_str())],
                None
            )
            .await
            .0,
            StatusCode::OK
        );
        // The token holder adds the first user, who is always an admin.
        let (st, _, b) = call(
            &app,
            "POST",
            "/api/users",
            &[AUTH],
            Some(json!({ "name": "alice", "role": "viewer", "password": "alice-pass-1234" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        assert_eq!(
            serde_json::from_str::<Value>(&b).unwrap()[0]["role"],
            "admin"
        );
        // Now the token signs nobody in: not as a cookie, not from a URL. As a
        // Bearer credential (tools) it is still an admin.
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/procs",
                &[("cookie", token_cookie.as_str())],
                None
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        let (st, h, _) = call(&app, "GET", &format!("/?token={}", TOKEN), &[], None).await;
        assert_eq!(st, StatusCode::OK);
        assert!(h.get(header::SET_COOKIE).is_none());
        assert_eq!(
            call(&app, "GET", "/api/users", &[AUTH], None).await.0,
            StatusCode::OK
        );

        async fn login(app: &Arc<App>, name: &str, pw: &str) -> (StatusCode, String) {
            let (st, h, b) = call(
                app,
                "POST",
                "/api/login",
                &[],
                Some(json!({ "name": name, "password": pw })),
            )
            .await;
            let c = h
                .get(header::SET_COOKIE)
                .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
                .unwrap_or(b);
            (st, c)
        }
        let (st, _) = login(&app, "alice", "wrong-password-x").await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, alice) = login(&app, "alice", "alice-pass-1234").await;
        assert_eq!(st, StatusCode::OK, "{}", alice);
        assert!(alice.starts_with("indra_session="));
        let a = [("cookie", alice.as_str())];
        for (n, r) in [("bob", "operator"), ("carol", "viewer")] {
            let (st, _, b) = call(
                &app,
                "POST",
                "/api/users",
                &a,
                Some(json!({ "name": n, "role": r, "password": format!("{}-pass-1234", n) })),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{}", b);
        }
        let (_, bob) = login(&app, "bob", "bob-pass-1234").await;
        let (_, carol) = login(&app, "carol", "carol-pass-1234").await;
        let (b_, c_) = ([("cookie", bob.as_str())], [("cookie", carol.as_str())]);

        // A viewer reads, and the console shows them read-only.
        assert_eq!(
            call(&app, "GET", "/api/procs", &c_, None).await.0,
            StatusCode::OK
        );
        let (_, _, ov) = call(&app, "GET", "/api/overview", &c_, None).await;
        let ov: Value = serde_json::from_str(&ov).unwrap();
        assert_eq!(
            (
                ov["user"].as_str(),
                ov["role"].as_str(),
                ov["read_only"].as_bool()
            ),
            (Some("carol"), Some("viewer"), Some(true))
        );
        for (m, u) in [
            ("POST", "/api/procs"),
            ("POST", "/api/workflows"),
            ("GET", "/api/config"),
            ("GET", "/api/secrets"),
            ("GET", "/api/users"),
            ("GET", "/api/term/check?what=shell"),
        ] {
            let (st, _, b) = call(&app, m, u, &c_, Some(json!({}))).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "viewer {} {}: {}", m, u, b);
        }
        // An operator runs things but cannot change settings, secrets, users or prompts.
        let (st, _, b) = call(
            &app,
            "POST",
            "/api/workflows",
            &b_,
            Some(json!({ "name": "nope" })),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::BAD_REQUEST,
            "an operator gets past the role check: {}",
            b
        );
        for (m, u) in [
            ("GET", "/api/config"),
            ("PUT", "/api/secrets/x"),
            ("POST", "/api/users"),
            ("PUT", "/api/def"),
            ("POST", "/api/market/install"),
        ] {
            let (st, _, _) = call(&app, m, u, &b_, Some(json!({}))).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "operator {} {}", m, u);
        }
        // Anyone signed in may change their own password (asked for the current one).
        let (st, _, _) = call(
            &app,
            "PUT",
            "/api/session/password",
            &c_,
            Some(json!({ "current": "nope-nope-nope", "new": "carol-new-pass-99" })),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        let (st, _, b) = call(
            &app,
            "PUT",
            "/api/session/password",
            &c_,
            Some(json!({ "current": "carol-pass-1234", "new": "carol-new-pass-99" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", b);
        assert_eq!(
            call(&app, "GET", "/api/procs", &c_, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        // Admins manage roles; the last admin cannot be demoted.
        let (st, _, _) = call(
            &app,
            "PATCH",
            "/api/users/alice",
            &a,
            Some(json!({ "role": "viewer" })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _, _) = call(
            &app,
            "PATCH",
            "/api/users/bob",
            &a,
            Some(json!({ "role": "viewer" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/workflows",
                &b_,
                Some(json!({ "name": "nope" }))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        // Signing out ends the session.
        let (st, h, _) = call(&app, "POST", "/api/logout", &a, Some(json!({}))).await;
        assert_eq!(st, StatusCode::OK);
        assert!(h[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0"));
        assert_eq!(
            call(&app, "GET", "/api/procs", &a, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        // Every change is in the audit log, with who made it.
        let log = std::fs::read_to_string(dir.join("data/logs/audit.jsonl")).unwrap();
        assert!(
            log.contains("\"user\":\"alice\"")
                && log.contains("\"user\":\"token\"")
                && log.contains("\"path\":\"/api/users/bob\""),
            "{}",
            log
        );
        assert!(!log.contains("pass-1234"), "no passwords in the log");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
