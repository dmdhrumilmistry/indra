//! The model router: Claude Code's main agent on Anthropic and its subagents
//! on a local model, in one session.
//!
//! Claude Code sends every model call to one base URL. When a profile hands
//! its subagents to Ollama (`subagent_provider = "ollama"`), indra points that
//! URL at this router, on loopback, in the process that starts the agent. The
//! router reads each request's `model`: the models the launch named for
//! Ollama go to the Ollama server (which speaks the Anthropic Messages API
//! since v0.14), everything else goes on to Anthropic, unchanged.
//!
//! - Each launch gets its own route behind a random path token, so another
//!   local process cannot borrow it by guessing the port.
//! - Credentials are never sent to Ollama: `authorization` and `x-api-key`
//!   are dropped on that leg. A whole agent on Ollama goes through the router
//!   for this alone, since Claude Code sends whatever login it has (a
//!   subscription login included) to its base URL. On the Anthropic leg they pass through as the
//!   agent sent them; the router adds none of its own.
//! - Responses are streamed through as they arrive (server-sent events).
//! - `HTTPS_PROXY`/`NO_PROXY` and `SSL_CERT_FILE`/`NODE_EXTRA_CA_CERTS` are
//!   honoured, as Claude Code does.

use axum::body::{Body, Bytes};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode, Uri};
use axum::routing::any;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Where requests for the models in `local` go, and where the rest go.
/// With `all_local`, every request goes to Ollama (a whole agent on a local
/// model): the router is then only there to keep credentials off Ollama.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Route {
    pub ollama: String,
    pub local: Vec<String>,
    pub anthropic: String,
    pub all_local: bool,
    /// Where a request goes, in order, when the Anthropic leg fails
    /// (unreachable, 429, 5xx, 529 overloaded).
    pub fallbacks: Vec<Upstream>,
}

/// A fallback upstream, resolved: its base URL, the model requests are
/// rewritten to, and the key it gets instead of the agent's (none for
/// Ollama). The agent's own credentials never go to a fallback.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Upstream {
    pub base: String,
    pub model: Option<String>,
    pub key: Option<String>,
    /// The key is an OAuth token (a Claude subscription, `claude
    /// setup-token`): sent as a bearer token with Anthropic's OAuth beta.
    pub oauth: bool,
}

/// Anthropic's beta flag for requests made with an OAuth token.
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// A fallback as a profile names it: `{ url = "ollama", model =
/// "qwen3-coder" }` (this machine's Ollama) or `{ url =
/// "https://gw.example.com", model = "...", api_key_env = "GW_KEY" }` (an
/// Anthropic-compatible endpoint, its key from the agent's environment).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fallback {
    /// "ollama", an Anthropic-compatible URL, or empty with an account
    /// (Anthropic's API).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// An `[[account]]` (a Claude one) whose credential this fallback uses:
    /// an API key, or a subscription OAuth token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// api_key (default) or oauth_token: how the key is sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl Fallback {
    /// Where it goes: Anthropic's API when only an account is named.
    pub fn base(&self) -> &str {
        if self.url.is_empty() {
            "https://api.anthropic.com"
        } else {
            &self.url
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.url.is_empty() && self.account.is_none() {
            return Err("fallback: say where it goes (url = \"ollama\" or an https URL), or name an account".into());
        }
        if let Some(a) = &self.account {
            crate::deck::nodes::validate_name(a).map_err(|e| format!("fallback account: {}", e))?;
            if self.url == "ollama" {
                return Err("fallback: Ollama takes no account".into());
            }
        }
        if let Some(k) = &self.kind {
            if !crate::deck::accounts::KINDS.contains(&k.as_str()) {
                return Err("fallback: kind is api_key or oauth_token".into());
            }
        }
        if !self.url.is_empty() && self.url != "ollama" {
            check_url(&self.url).map_err(|e| format!("fallback: {}", e))?;
            if self.url.starts_with("http://")
                && !(self.url.starts_with("http://127.0.0.1")
                    || self.url.starts_with("http://localhost")
                    || self.url.starts_with("http://[::1]"))
                && self.api_key_env.is_some()
            {
                return Err("fallback: a key is only sent over https (or to this machine)".into());
            }
        } else if self.url == "ollama" && self.model.is_none() {
            return Err(
                "fallback: name the Ollama model it runs on (model = \"qwen3-coder\")".into(),
            );
        }
        if let Some(m) = &self.model {
            if m.is_empty()
                || m.len() > 128
                || !m
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._:/-[]".contains(c))
            {
                return Err(format!("fallback: invalid model {:?}", m));
            }
        }
        if let Some(k) = &self.api_key_env {
            if k.is_empty()
                || k.len() > 128
                || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(format!(
                    "fallback: invalid environment variable name {:?}",
                    k
                ));
            }
        }
        Ok(())
    }
}

/// Whether a response (or a failure to get one) sends a request on to the
/// next fallback.
fn retryable(r: &Result<Response<Body>, String>) -> bool {
    match r {
        Err(_) => true,
        Ok(r) => matches!(r.status().as_u16(), 429 | 500 | 502 | 503 | 504 | 529),
    }
}

/// `body` with its `model` set to `m`, when it is a JSON object with one.
fn with_model(body: &Bytes, m: &str) -> Bytes {
    match serde_json::from_slice::<Value>(body) {
        Ok(mut v) if v.get("model").is_some() => {
            v["model"] = json!(m);
            Bytes::from(v.to_string())
        }
        _ => body.clone(),
    }
}

#[derive(Default)]
struct Counts {
    ollama: AtomicU64,
    anthropic: AtomicU64,
    failed: AtomicU64,
    fallback: AtomicU64,
}

struct Shared {
    port: u16,
    routes: Mutex<HashMap<String, (Route, Arc<Counts>)>>,
    by_route: Mutex<HashMap<Route, String>>,
}

static ROUTER: OnceLock<Arc<Shared>> = OnceLock::new();
const MAX_BODY: usize = 64 << 20;

/// Upstream base URLs must be plain http(s) URLs.
pub fn check_url(u: &str) -> Result<(), String> {
    let ok = (u.starts_with("http://") || u.starts_with("https://"))
        && u.len() <= 512
        && !u.chars().any(|c| c.is_control() || c.is_whitespace())
        && u.parse::<Uri>()
            .map(|x| x.host().is_some())
            .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(format!("not an http(s) URL: {:?}", u))
    }
}

/// The base URL an agent should use for `route`: this process's router,
/// started on first use on a thread of its own.
pub fn register(route: Route) -> Result<String, String> {
    if route.all_local || !route.local.is_empty() || !route.ollama.is_empty() {
        check_url(&route.ollama)?;
    }
    if !route.all_local {
        check_url(&route.anthropic)?;
    }
    if route.local.is_empty() && !route.all_local && route.fallbacks.is_empty() {
        return Err("name the model the subagents run on".into());
    }
    for f in &route.fallbacks {
        check_url(&f.base)?;
    }
    let shared = match ROUTER.get() {
        Some(s) => s.clone(),
        None => {
            let std = std::net::TcpListener::bind("127.0.0.1:0")
                .map_err(|e| format!("model router: {}", e))?;
            std.set_nonblocking(true).map_err(|e| e.to_string())?;
            let port = std.local_addr().map_err(|e| e.to_string())?.port();
            let shared = Arc::new(Shared {
                port,
                routes: Mutex::default(),
                by_route: Mutex::default(),
            });
            // Two launches at once: the first to set it wins; the loser's
            // listener is dropped unused.
            if ROUTER.set(shared.clone()).is_err() {
                return register(route);
            }
            let app = axum::Router::new()
                .route("/r/{token}/{*rest}", any(proxy))
                .with_state(shared.clone());
            // Its own thread and runtime, so it outlives whatever started it.
            std::thread::Builder::new()
                .name("indra-model-router".into())
                .spawn(move || {
                    let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                    else {
                        return;
                    };
                    rt.block_on(async move {
                        if let Ok(l) = tokio::net::TcpListener::from_std(std) {
                            let _ = axum::serve(l, app).await;
                        }
                    });
                })
                .map_err(|e| format!("model router: {}", e))?;
            shared
        }
    };
    let mut by = shared.by_route.lock().unwrap();
    let token = match by.get(&route) {
        Some(t) => t.clone(),
        None => {
            let t = crate::deck::link::random_token(16);
            shared
                .routes
                .lock()
                .unwrap()
                .insert(t.clone(), (route.clone(), Arc::default()));
            by.insert(route, t.clone());
            t
        }
    };
    Ok(format!("http://127.0.0.1:{}/r/{}", shared.port, token))
}

/// Requests sent each way so far, per route (for the console).
pub fn stats() -> Vec<Value> {
    let Some(s) = ROUTER.get() else {
        return vec![];
    };
    let routes = s.routes.lock().unwrap();
    routes
        .values()
        .map(|(r, c)| {
            json!({
                "ollama": r.ollama, "local_models": r.local, "all_local": r.all_local,
                "anthropic": if r.all_local { Value::Null } else { json!(r.anthropic) },
                "to_ollama": c.ollama.load(Ordering::Relaxed),
                "to_anthropic": c.anthropic.load(Ordering::Relaxed),
                "failed": c.failed.load(Ordering::Relaxed),
                "fallbacks": r.fallbacks.iter().map(|f| f.base.clone()).collect::<Vec<_>>(),
                "to_fallback": c.fallback.load(Ordering::Relaxed),
            })
        })
        .collect()
}

fn api_error(status: StatusCode, msg: &str) -> Response<Body> {
    let body = json!({ "type": "error", "error": { "type": "api_error", "message": format!("indra model router: {}", msg) } });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// The `model` of a Messages API request, if this is one.
fn model_of(rest: &str, body: &[u8]) -> Option<String> {
    if !rest.starts_with("v1/messages") {
        return None;
    }
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("model")?.as_str().map(str::to_string)
}

async fn proxy(
    State(s): State<Arc<Shared>>,
    Path((token, rest)): Path<(String, String)>,
    req: Request,
) -> Response<Body> {
    let Some((route, counts)) = s.routes.lock().unwrap().get(&token).cloned() else {
        return api_error(StatusCode::NOT_FOUND, "unknown route");
    };
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return api_error(StatusCode::PAYLOAD_TOO_LARGE, "request too large"),
    };
    let local = route.all_local
        || model_of(&rest, &body)
            .map(|m| route.local.contains(&m))
            .unwrap_or(false);
    let (base, counter) = if local {
        (&route.ollama, &counts.ollama)
    } else {
        (&route.anthropic, &counts.anthropic)
    };
    counter.fetch_add(1, Ordering::Relaxed);
    let path = match parts.uri.query() {
        Some(q) => format!("{}?{}", rest, q),
        None => rest,
    };
    let mut r = forward(
        base,
        parts.method.as_str(),
        &path,
        &parts.headers,
        body.clone(),
        local,
    )
    .await;
    // The Anthropic leg failed: the fallbacks, in order, each with its own
    // key (never the agent's) and model.
    if !local && retryable(&r) {
        for f in &route.fallbacks {
            let mut headers = parts.headers.clone();
            headers.remove("authorization");
            headers.remove("x-api-key");
            match (f.key.as_deref(), f.oauth) {
                (Some(k), true) => {
                    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", k)) {
                        headers.insert("authorization", v);
                    }
                    // Anthropic's API takes an OAuth token with its beta flag.
                    let beta = match headers.get("anthropic-beta").and_then(|v| v.to_str().ok()) {
                        Some(b) if b.split(',').any(|x| x.trim() == OAUTH_BETA) => b.to_string(),
                        Some(b) if !b.trim().is_empty() => format!("{},{}", b, OAUTH_BETA),
                        _ => OAUTH_BETA.to_string(),
                    };
                    if let Ok(v) = HeaderValue::from_str(&beta) {
                        headers.insert("anthropic-beta", v);
                    }
                }
                (Some(k), false) => {
                    if let Ok(v) = HeaderValue::from_str(k) {
                        headers.insert("x-api-key", v);
                    }
                }
                (None, _) => {}
            }
            let b = match &f.model {
                Some(m) => with_model(&body, m),
                None => body.clone(),
            };
            counts.fallback.fetch_add(1, Ordering::Relaxed);
            // The agent's credentials were taken off above; this key stays.
            r = forward(&f.base, parts.method.as_str(), &path, &headers, b, false).await;
            if !retryable(&r) {
                break;
            }
        }
    }
    match r {
        Ok(r) => r,
        Err(e) => {
            counts.failed.fetch_add(1, Ordering::Relaxed);
            api_error(StatusCode::BAD_GATEWAY, &e)
        }
    }
}

/// Headers that belong to one hop, not the request.
const HOP: [&str; 9] = [
    "host",
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// One request to `base` + `path`, and its response, streamed.
pub async fn forward(
    base: &str,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: Bytes,
    strip_auth: bool,
) -> Result<Response<Body>, String> {
    let uri: Uri = base.parse().map_err(|_| format!("bad upstream {}", base))?;
    let https = uri.scheme_str() == Some("https");
    let host = uri.host().ok_or("upstream has no host")?.to_string();
    let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
    let full = format!(
        "{}/{}",
        uri.path().trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let io = connect(&host, port, https).await?;
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io))
            .await
            .map_err(|e| format!("{}: {}", host, e))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let mut b = hyper::Request::builder()
        .method(method)
        .uri(if full.starts_with('/') {
            full
        } else {
            format!("/{}", full)
        });
    for (k, v) in headers {
        let name = k.as_str();
        if HOP.contains(&name) || name == "content-length" {
            continue;
        }
        if strip_auth && (name == "authorization" || name == "x-api-key") {
            continue;
        }
        b = b.header(k, v);
    }
    let authority = match uri.port_u16() {
        Some(p) => format!("{}:{}", host, p),
        None => host.clone(),
    };
    b = b.header("host", authority);
    let req = b
        .body(http_body_util::Full::new(body))
        .map_err(|e| e.to_string())?;
    let resp = tokio::time::timeout(Duration::from_secs(600), sender.send_request(req))
        .await
        .map_err(|_| format!("{}: no response", host))?
        .map_err(|e| format!("{}: {}", host, e))?;
    let (mut parts, incoming) = resp.into_parts();
    for h in HOP {
        parts.headers.remove(h);
    }
    Ok(Response::from_parts(parts, Body::new(incoming)))
}

/// Whether `host` skips the proxy, by `NO_PROXY` (exact, `.suffix`,
/// `*.suffix` or `*`). CIDR entries are not interpreted.
pub(crate) fn no_proxy(host: &str) -> bool {
    let list = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();
    list.split(',').map(str::trim).any(|e| {
        let e = e.trim_start_matches('*');
        !e.is_empty()
            && (e == "*"
                || host == e.trim_start_matches('.')
                || (e.starts_with('.') && host.ends_with(e)))
    }) || list.trim() == "*"
}

async fn connect(host: &str, port: u16, https: bool) -> Result<Box<dyn Io>, String> {
    let loopback = host == "localhost" || host == "127.0.0.1" || host == "::1";
    let proxy = if https && !loopback && !no_proxy(host) {
        std::env::var("HTTPS_PROXY")
            .or_else(|_| std::env::var("https_proxy"))
            .ok()
            .filter(|p| p.starts_with("http://"))
    } else {
        None
    };
    let tcp = match &proxy {
        Some(p) => tunnel(p, host, port).await?,
        None => tokio::time::timeout(Duration::from_secs(15), TcpStream::connect((host, port)))
            .await
            .map_err(|_| format!("{}:{}: connection timed out", host, port))?
            .map_err(|e| format!("{}:{}: {}", host, port, e))?,
    };
    let _ = tcp.set_nodelay(true);
    if !https {
        return Ok(Box::new(tcp));
    }
    let name = rustls_pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| format!("bad host name {}", host))?;
    let tls = tokio_rustls::TlsConnector::from(tls_config()?)
        .connect(name, tcp)
        .await
        .map_err(|e| format!("{}: TLS: {}", host, e))?;
    Ok(Box::new(tls))
}

/// An HTTP CONNECT tunnel through `proxy` (http://[user:pass@]host:port).
pub(crate) async fn tunnel(proxy: &str, host: &str, port: u16) -> Result<TcpStream, String> {
    let u: Uri = proxy.parse().map_err(|_| "bad HTTPS_PROXY".to_string())?;
    let auth = u.authority().ok_or("bad HTTPS_PROXY")?;
    let (creds, hostport) = match auth.as_str().rsplit_once('@') {
        Some((c, h)) => (Some(c.to_string()), h.to_string()),
        None => (None, auth.as_str().to_string()),
    };
    let mut s = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(&hostport))
        .await
        .map_err(|_| "proxy: connection timed out".to_string())?
        .map_err(|e| format!("proxy {}: {}", hostport, e))?;
    let mut req = format!(
        "CONNECT {h}:{p} HTTP/1.1\r\nHost: {h}:{p}\r\n",
        h = host,
        p = port
    );
    if let Some(c) = creds {
        req.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            crate::deck::trackers::base64(c.as_bytes())
        ));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 << 10 || s.read(&mut byte).await.map_err(|e| e.to_string())? == 0 {
            return Err("proxy closed the tunnel".into());
        }
        head.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&head);
    let status = line.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(format!(
            "proxy refused the tunnel: {}",
            line.lines().next().unwrap_or("")
        ));
    }
    Ok(s)
}

pub(crate) fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CFG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CFG.get_or_init(|| {
        use rustls_pki_types::pem::PemObject;
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for var in ["SSL_CERT_FILE", "NODE_EXTRA_CA_CERTS"] {
            if let Ok(path) = std::env::var(var) {
                if let Ok(iter) = rustls_pki_types::CertificateDer::pem_file_iter(&path) {
                    for c in iter.flatten() {
                        let _ = roots.add(c);
                    }
                }
            }
        }
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Arc::new(cfg))
    })
    .clone()
}

/// The models an Ollama server has: `GET /api/tags`.
pub async fn ollama_models(base: &str) -> Result<Vec<Value>, String> {
    check_url(base)?;
    let mut h = HeaderMap::new();
    h.insert(
        HeaderName::from_static("accept"),
        HeaderValue::from_static("application/json"),
    );
    let resp = tokio::time::timeout(
        Duration::from_secs(10),
        forward(base, "GET", "api/tags", &h, Bytes::new(), true),
    )
    .await
    .map_err(|_| format!("{}: no answer", base))??;
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 8 << 20)
        .await
        .map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("{} answered {}", base, status));
    }
    let v: Value =
        serde_json::from_slice(&body).map_err(|_| format!("{}: not an Ollama server", base))?;
    Ok(v.get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            json!({
                "name": m.get("name").or_else(|| m.get("model")).cloned().unwrap_or(Value::Null),
                "size": m.get("size").cloned().unwrap_or(Value::Null),
                "parameters": m.pointer("/details/parameter_size").cloned().unwrap_or(Value::Null),
                "family": m.pointer("/details/family").cloned().unwrap_or(Value::Null),
                "modified": m.get("modified_at").cloned().unwrap_or(Value::Null),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// A stand-in upstream that records what it got and answers with a
    /// two-part stream.
    async fn upstream(name: &'static str) -> (String, Arc<StdMutex<Vec<Value>>>) {
        let seen: Arc<StdMutex<Vec<Value>>> = Arc::default();
        let s2 = seen.clone();
        let app = axum::Router::new().fallback(move |req: Request| {
            let seen = s2.clone();
            async move {
                let (p, b) = req.into_parts();
                let body = axum::body::to_bytes(b, 1 << 20).await.unwrap();
                let hdr = |k: &str| p.headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                seen.lock().unwrap().push(json!({
                    "path": p.uri.to_string(), "auth": hdr("authorization"), "key": hdr("x-api-key"),
                    "beta": hdr("anthropic-beta"), "body": String::from_utf8_lossy(&body),
                }));
                let chunks = futures_util::stream::iter(vec![
                    Ok::<_, std::io::Error>(Bytes::from(format!("event: a\ndata: {}\n\n", name))),
                    Ok(Bytes::from("event: b\ndata: done\n\n")),
                ]);
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(chunks))
                    .unwrap()
            }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (url, seen)
    }

    async fn post(base: &str, path: &str, body: Value) -> (u16, String) {
        let mut h = HeaderMap::new();
        h.insert("x-api-key", HeaderValue::from_static("sk-secret"));
        h.insert(
            "authorization",
            HeaderValue::from_static("Bearer oauth-secret"),
        );
        h.insert("anthropic-beta", HeaderValue::from_static("x-1"));
        h.insert("content-type", HeaderValue::from_static("application/json"));
        let r = forward(base, "POST", path, &h, Bytes::from(body.to_string()), false)
            .await
            .unwrap();
        let st = r.status().as_u16();
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (st, String::from_utf8_lossy(&b).into_owned())
    }

    #[tokio::test]
    async fn routes_by_model_and_keeps_credentials_off_ollama() {
        let (ollama, o_seen) = upstream("ollama").await;
        let (anthropic, a_seen) = upstream("anthropic").await;
        let route = Route {
            ollama: ollama.clone(),
            local: vec!["qwen3-coder".into()],
            anthropic: format!("{}/gateway", anthropic),
            all_local: false,
            fallbacks: vec![],
        };
        let base = register(route.clone()).unwrap();
        assert!(base.starts_with("http://127.0.0.1:"));
        // The same route reuses its token; another route gets a new one.
        assert_eq!(register(route.clone()).unwrap(), base);
        let other = register(Route {
            local: vec!["llama3".into()],
            ..route.clone()
        })
        .unwrap();
        assert_ne!(other, base);

        let (st, body) = post(
            &base,
            "v1/messages?beta=true",
            json!({ "model": "qwen3-coder", "stream": true }),
        )
        .await;
        assert_eq!(st, 200);
        assert_eq!(body, "event: a\ndata: ollama\n\nevent: b\ndata: done\n\n");
        let (_, body) = post(&base, "v1/messages", json!({ "model": "claude-opus-4-1" })).await;
        assert!(body.contains("anthropic"));
        let (_, body) = post(
            &base,
            "v1/messages/count_tokens",
            json!({ "model": "qwen3-coder" }),
        )
        .await;
        assert!(body.contains("ollama"));

        let o = o_seen.lock().unwrap().clone();
        let a = a_seen.lock().unwrap().clone();
        assert_eq!(o.len(), 2);
        assert_eq!(o[0]["path"], "/v1/messages?beta=true");
        assert_eq!(o[0]["auth"], "");
        assert_eq!(o[0]["key"], "");
        assert_eq!(o[0]["beta"], "x-1");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0]["path"], "/gateway/v1/messages");
        assert_eq!(a[0]["key"], "sk-secret");
        assert_eq!(a[0]["auth"], "Bearer oauth-secret");
        assert!(a[0]["body"].as_str().unwrap().contains("claude-opus-4-1"));

        // An unknown token is refused, and a dead upstream is a 502 in the
        // API's own error shape.
        let bad = base.rsplit_once('/').unwrap().0.to_string() + "/nope";
        let (st, _) = post(&bad, "v1/messages", json!({})).await;
        assert_eq!(st, 404);
        let dead = register(Route {
            anthropic: "http://127.0.0.1:9".into(),
            ..route.clone()
        })
        .unwrap();
        let (st, body) = post(&dead, "v1/messages", json!({ "model": "claude-x" })).await;
        assert_eq!(st, 502);
        assert!(body.contains("\"type\":\"error\""));
        assert!(stats().iter().any(|s| s["to_ollama"] == 2));

        // A whole agent on Ollama: every model goes there, without credentials.
        let all = register(Route {
            ollama: ollama.clone(),
            all_local: true,
            ..Default::default()
        })
        .unwrap();
        let (_, body) = post(&all, "v1/messages", json!({ "model": "claude-opus-4-1" })).await;
        assert!(body.contains("ollama"));
        let o = o_seen.lock().unwrap().clone();
        assert_eq!(o.len(), 3);
        assert_eq!(o[2]["auth"], "");
        assert_eq!(o[2]["key"], "");
    }

    #[tokio::test]
    async fn falls_back_without_the_agents_credentials() {
        // A primary that is overloaded (529), then a gateway and Ollama.
        let app = axum::Router::new().fallback(|| async {
            Response::builder()
                .status(529)
                .body(Body::from("{\"type\":\"error\"}"))
                .unwrap()
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let busy = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let (gw, g_seen) = upstream("gateway").await;
        let base = register(Route {
            anthropic: busy.clone(),
            fallbacks: vec![
                Upstream {
                    base: "http://127.0.0.1:9".into(),
                    model: None,
                    key: None,
                    oauth: false,
                },
                Upstream {
                    base: gw.clone(),
                    model: Some("glm-4.6".into()),
                    key: Some("gw-key".into()),
                    oauth: false,
                },
            ],
            ..Default::default()
        })
        .unwrap();
        let (st, body) = post(
            &base,
            "v1/messages",
            json!({ "model": "claude-opus-4-1", "stream": true }),
        )
        .await;
        assert_eq!(st, 200);
        assert!(body.contains("gateway"), "{}", body);
        let g = g_seen.lock().unwrap().clone();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0]["key"], "gw-key", "the fallback's own key");
        assert_eq!(g[0]["auth"], "", "never the agent's login");
        assert!(g[0]["body"].as_str().unwrap().contains("\"glm-4.6\""));
        assert!(stats().iter().any(|s| s["to_fallback"] == 2));
        // A request that succeeds on the primary never reaches a fallback.
        let (ok, a_seen) = upstream("anthropic").await;
        let base = register(Route {
            anthropic: ok,
            fallbacks: vec![Upstream {
                base: gw.clone(),
                model: None,
                key: None,
                oauth: false,
            }],
            ..Default::default()
        })
        .unwrap();
        let (_, body) = post(&base, "v1/messages", json!({ "model": "claude-x" })).await;
        assert!(body.contains("anthropic"));
        assert_eq!(a_seen.lock().unwrap().len(), 1);
        assert_eq!(g_seen.lock().unwrap().len(), 1);
        // An account's OAuth token (a Claude subscription): a bearer token
        // with Anthropic's OAuth beta, beside the agent's own betas.
        let base = register(Route {
            anthropic: busy.clone(),
            fallbacks: vec![Upstream {
                base: gw.clone(),
                model: None,
                key: Some("sk-ant-oat-x".into()),
                oauth: true,
            }],
            ..Default::default()
        })
        .unwrap();
        let (st, _) = post(&base, "v1/messages", json!({ "model": "claude-x" })).await;
        assert_eq!(st, 200);
        let g = g_seen.lock().unwrap().clone();
        let last = g.last().unwrap();
        assert_eq!(last["auth"], "Bearer sk-ant-oat-x");
        assert_eq!(last["key"], "", "no API key header with an OAuth token");
        assert_eq!(last["beta"], "x-1,oauth-2025-04-20");
        let by_account = Fallback {
            account: Some("team-max".into()),
            ..Default::default()
        };
        assert!(by_account.validate().is_ok());
        assert_eq!(by_account.base(), "https://api.anthropic.com");
        assert!(Fallback::default().validate().is_err(), "nowhere to go");
        // Profiles' fallbacks are checked.
        assert!(Fallback {
            url: "ollama".into(),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Fallback {
            url: "http://gw.example".into(),
            api_key_env: Some("K".into()),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Fallback {
            url: "https://gw.example".into(),
            api_key_env: Some("GW_KEY".into()),
            ..Default::default()
        }
        .validate()
        .is_ok());
    }

    #[tokio::test]
    async fn lists_ollama_models() {
        let app = axum::Router::new().route(
            "/api/tags",
            axum::routing::get(|| async {
                axum::Json(json!({ "models": [{ "name": "qwen3-coder:latest", "size": 1, "details": { "parameter_size": "30B", "family": "qwen3" } }] }))
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let m = ollama_models(&url).await.unwrap();
        assert_eq!(m[0]["name"], "qwen3-coder:latest");
        assert_eq!(m[0]["parameters"], "30B");
        assert!(ollama_models("http://127.0.0.1:9").await.is_err());
        assert!(ollama_models("file:///etc").await.is_err());
    }

    /// Live: TLS to the real API (direct, or through HTTPS_PROXY when the
    /// host is not in NO_PROXY). `cargo test -- --ignored live_tls`.
    #[tokio::test]
    #[ignore]
    async fn live_tls() {
        let h = HeaderMap::new();
        for (base, path) in [
            ("https://api.anthropic.com", "v1/models"),
            ("https://api.github.com", "zen"),
        ] {
            let r = forward(base, "GET", path, &h, Bytes::new(), true)
                .await
                .unwrap();
            let st = r.status().as_u16();
            let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
            eprintln!(
                "{} {} {}",
                base,
                st,
                String::from_utf8_lossy(&b)
                    .chars()
                    .take(120)
                    .collect::<String>()
            );
            assert!(st == 200 || st == 401 || st == 403, "{}", st);
        }
    }

    #[test]
    fn urls() {
        assert!(check_url("http://localhost:11434").is_ok());
        assert!(check_url("https://api.anthropic.com").is_ok());
        assert!(check_url("ftp://x").is_err());
        assert!(check_url("http://").is_err());
        assert!(check_url("http://a b").is_err());
    }
}
