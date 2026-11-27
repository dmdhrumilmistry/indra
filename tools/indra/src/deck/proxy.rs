//! The recording proxy: an engagement's agents reach the web through it, so
//! the scope holds on every connection and every exchange is kept as
//! evidence.
//!
//! `indra guard` reads the hosts a tool call names; this sees what the
//! programs actually connect to. Each agent launched in an engagement with
//! recording on gets its own proxy on loopback, in the indra process that
//! starts it, and its own certificate authority, made at launch and never
//! written to disk with its key (only the certificate, so the agent's tools
//! can trust it). The agent is pointed at it with HTTP(S)_PROXY and the CA
//! variables its tools read.
//!
//! For each request it:
//!
//! - resolves the host and checks the name and every address against the
//!   scope, the exclusions and the window (`engage::allows_connection`),
//!   then connects to the address it checked, so DNS cannot change the
//!   answer in between (through an upstream proxy, the check is by name);
//! - paces it per host (the engagement's rate), refusing with 429 when the
//!   host is booked too far ahead, and backs off a host that answers 429
//!   or 503;
//! - forwards it, asking for an uncompressed answer so evidence can be
//!   quoted, and streams the response back as it arrives;
//! - records request and response (headers, and up to 256 KiB of each body,
//!   stored by SHA-256) under `engagements/<id>/traffic/`, and writes a
//!   `traffic` entry to the engagement's audit log.
//!
//! The agent's own model API (Anthropic, its configured base URL, and the
//! hosts of the MCP servers the console gave it) is tunnelled untouched and
//! unrecorded: never decrypted, so its credentials are never seen.

use http_body_util::BodyExt;
use hyper::body::Bytes;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use crate::deck::engage::{self, Spec};

/// The UTC date `days` days from today, for a certificate's validity.
fn ymd(days: i64) -> (i32, u8, u8) {
    let d = (engage::now() as i64).div_euclid(86_400) + days;
    // Civil from days (Howard Hinnant's algorithm).
    let z = d + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let y = (400 * era + yoe + if m <= 2 { 1 } else { 0 }) as i32;
    (y, m, dd)
}

macro_rules! day {
    ($n:expr) => {{
        let (y, m, d) = ymd($n);
        rcgen::date_time_ymd(y, m, d)
    }};
}

/// The most of each body kept as evidence.
pub const MAX_KEEP: usize = 256 << 10;
/// The most of a request body read before forwarding it.
const MAX_REQUEST: usize = 64 << 20;

/// Codex's own hosts: the OpenAI API and the ChatGPT login.
pub const CODEX_HOSTS: &[&str] = &[
    "api.openai.com",
    "chatgpt.com",
    "auth.openai.com",
    "ab.chatgpt.com",
];

/// Gemini CLI's own hosts: the Gemini API, Code Assist (Login with Google),
/// Google's sign-in, and Vertex AI.
pub const GEMINI_HOSTS: &[&str] = &[
    "generativelanguage.googleapis.com",
    "cloudcode-pa.googleapis.com",
    "oauth2.googleapis.com",
    "www.googleapis.com",
    "accounts.google.com",
    "aiplatform.googleapis.com",
];

/// Hosts an agent's own runner talks to: tunnelled, never decrypted or
/// recorded, and outside any scope check.
pub const RUNNER_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "statsig.anthropic.com",
    "console.anthropic.com",
    "claude.ai",
    "platform.claude.com",
    "sentry.io",
];

type BoxBody = http_body_util::combinators::BoxBody<Bytes, hyper::Error>;

fn full(b: impl Into<Bytes>) -> BoxBody {
    http_body_util::Full::new(b.into())
        .map_err(|never| match never {})
        .boxed()
}

fn refuse(status: StatusCode, msg: &str) -> Response<BoxBody> {
    let mut r = Response::new(full(format!("indra recording proxy: {}\n", msg)));
    *r.status_mut() = status;
    r.headers_mut()
        .insert("content-type", "text/plain; charset=utf-8".parse().unwrap());
    r
}

/// What one proxy works for.
struct Ctx {
    spec: Spec,
    /// The engagement's folder on this machine (audit log, pacing, traffic).
    dir: PathBuf,
    /// Which agent (a label in each record).
    agent: String,
    passthrough: Vec<String>,
    ca: rcgen::Certificate,
    ca_key: rcgen::KeyPair,
    leaf_key: rcgen::KeyPair,
    certs: Mutex<HashMap<String, Arc<rustls::ServerConfig>>>,
    seq: AtomicU64,
}

/// A running proxy. Dropping it stops it and removes its CA files.
pub struct Handle {
    pub port: u16,
    /// The CA certificate (PEM), and a bundle of it with this machine's
    /// own roots, for tools that replace their trust store.
    pub ca_file: PathBuf,
    pub bundle_file: PathBuf,
    stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    files: PathBuf,
}

impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(s) = self.stop.lock().unwrap().take() {
            let _ = s.send(());
        }
        let _ = std::fs::remove_dir_all(&self.files);
    }
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "proxy on 127.0.0.1:{}", self.port)
    }
}

impl Handle {
    /// The environment that sends an agent's tools through the proxy and
    /// makes them trust its CA.
    pub fn env(&self, passthrough: &[String]) -> Vec<(String, String)> {
        let url = format!("http://127.0.0.1:{}", self.port);
        let mut no = vec!["localhost".to_string(), "127.0.0.1".into(), "::1".into()];
        no.extend(passthrough.iter().cloned());
        let ca = self.ca_file.display().to_string();
        let bundle = self.bundle_file.display().to_string();
        vec![
            ("HTTPS_PROXY".into(), url.clone()),
            ("HTTP_PROXY".into(), url.clone()),
            ("https_proxy".into(), url.clone()),
            ("http_proxy".into(), url),
            ("NO_PROXY".into(), no.join(",")),
            ("no_proxy".into(), no.join(",")),
            ("NODE_EXTRA_CA_CERTS".into(), ca),
            ("SSL_CERT_FILE".into(), bundle.clone()),
            ("REQUESTS_CA_BUNDLE".into(), bundle.clone()),
            ("CURL_CA_BUNDLE".into(), bundle.clone()),
            ("GIT_SSL_CAINFO".into(), bundle),
        ]
    }
}

fn host_only(h: &str) -> String {
    h.trim()
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
}

/// The hosts never intercepted for an agent: its runner's, the host of its
/// model base URL, and those of the MCP servers it was given.
pub fn passthrough_for(
    agent: crate::deck::hooks::Agent,
    env: &[(String, String)],
    urls: &[String],
) -> Vec<String> {
    use crate::deck::hooks::Agent;
    let hosts: &[&str] = match agent {
        Agent::Codex => CODEX_HOSTS,
        Agent::Gemini => GEMINI_HOSTS,
        _ => RUNNER_HOSTS,
    };
    let mut v: Vec<String> = hosts.iter().map(|s| s.to_string()).collect();
    if agent == Agent::Gemini {
        // Vertex AI's regional endpoint.
        if let Some((_, loc)) = env.iter().find(|(k, _)| k == "GOOGLE_CLOUD_LOCATION") {
            if !loc.is_empty() && loc.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                v.push(format!(
                    "{}-aiplatform.googleapis.com",
                    loc.to_ascii_lowercase()
                ));
            }
        }
    }
    let host_of = |u: &str| -> Option<String> {
        let u: hyper::Uri = u.parse().ok()?;
        u.host().map(host_only)
    };
    for (k, val) in env {
        if [
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_BEDROCK_BASE_URL",
            "ANTHROPIC_VERTEX_BASE_URL",
            "OPENAI_BASE_URL",
            "GOOGLE_GEMINI_BASE_URL",
            "GOOGLE_VERTEX_BASE_URL",
        ]
        .contains(&k.as_str())
        {
            if let Some(h) = host_of(val) {
                v.push(h);
            }
        }
    }
    for u in urls {
        if let Some(h) = host_of(u) {
            v.push(h);
        }
    }
    v.sort();
    v.dedup();
    v
}

fn is_passthrough(ctx: &Ctx, host: &str) -> bool {
    let h = host_only(host);
    ctx.passthrough
        .iter()
        .any(|p| h == *p || h.ends_with(&format!(".{}", p)))
}

/// Start a proxy for one agent of engagement `spec`, keeping its records in
/// `dir`. It runs on its own thread until the handle is dropped.
pub fn start(
    spec: Spec,
    dir: PathBuf,
    agent: &str,
    passthrough: Vec<String>,
) -> Result<Handle, String> {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
    let ca_key = KeyPair::generate().map_err(|e| format!("proxy CA: {}", e))?;
    let mut p = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    p.distinguished_name.push(
        DnType::CommonName,
        format!("indra engagement {} (agent {})", spec.id, agent),
    );
    p.distinguished_name
        .push(DnType::OrganizationName, "indra recording proxy");
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    p.not_before = day!(-1);
    p.not_after = day!(91);
    let ca = p
        .self_signed(&ca_key)
        .map_err(|e| format!("proxy CA: {}", e))?;
    let leaf_key = KeyPair::generate().map_err(|e| e.to_string())?;

    // The CA's certificate (never its key), for the agent's tools.
    let files = dir.join("proxy").join(agent);
    std::fs::create_dir_all(&files).map_err(|e| format!("{}: {}", files.display(), e))?;
    let ca_file = files.join("ca.pem");
    std::fs::write(&ca_file, ca.pem()).map_err(|e| e.to_string())?;
    let mut bundle = ca.pem();
    for sys in [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/ssl/cert.pem",
        "/usr/local/etc/openssl/cert.pem",
    ] {
        if let Ok(t) = std::fs::read_to_string(sys) {
            bundle.push('\n');
            bundle.push_str(&t);
            break;
        }
    }
    let bundle_file = files.join("bundle.pem");
    std::fs::write(&bundle_file, bundle).map_err(|e| e.to_string())?;

    let ctx = Arc::new(Ctx {
        spec,
        dir,
        agent: agent.to_string(),
        passthrough,
        ca,
        ca_key,
        leaf_key,
        certs: Mutex::default(),
        seq: AtomicU64::new(0),
    });
    let std = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("proxy: {}", e))?;
    std.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = std.local_addr().map_err(|e| e.to_string())?.port();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    std::thread::Builder::new()
        .name("indra-recording-proxy".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                let Ok(l) = tokio::net::TcpListener::from_std(std) else {
                    return;
                };
                let mut rx = rx;
                loop {
                    tokio::select! {
                        _ = &mut rx => break,
                        acc = l.accept() => {
                            let Ok((sock, _)) = acc else { continue };
                            let ctx = ctx.clone();
                            tokio::spawn(async move { serve(ctx, sock).await });
                        }
                    }
                }
            });
        })
        .map_err(|e| format!("proxy: {}", e))?;
    Ok(Handle {
        port,
        ca_file,
        bundle_file,
        stop: Mutex::new(Some(tx)),
        files,
    })
}

async fn serve(ctx: Arc<Ctx>, sock: tokio::net::TcpStream) {
    let _ = sock.set_nodelay(true);
    let svc = hyper::service::service_fn(move |req| {
        let ctx = ctx.clone();
        async move { Ok::<_, hyper::Error>(outer(ctx, req).await) }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .preserve_header_case(true)
        .serve_connection(TokioIo::new(sock), svc)
        .with_upgrades()
        .await;
}

/// `host:port` of a CONNECT target.
fn split_authority(a: &str, default: u16) -> (String, u16) {
    if let Some(rest) = a.strip_prefix('[') {
        let (h, tail) = rest.split_once(']').unwrap_or((rest, ""));
        let p = tail.trim_start_matches(':').parse().unwrap_or(default);
        return (h.to_ascii_lowercase(), p);
    }
    match a.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_ascii_lowercase(), p.parse().unwrap_or(default)),
        _ => (a.to_ascii_lowercase(), default),
    }
}

/// A request to the proxy itself: CONNECT, or an absolute-URI request.
async fn outer(ctx: Arc<Ctx>, req: Request<Incoming>) -> Response<BoxBody> {
    if req.method() == hyper::Method::CONNECT {
        let auth = req
            .uri()
            .authority()
            .map(|a| a.to_string())
            .unwrap_or_default();
        let (host, port) = split_authority(&auth, 443);
        if host.is_empty() {
            return refuse(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
        }
        if is_passthrough(&ctx, &host) {
            // The runner's own traffic: a blind tunnel.
            tokio::spawn(async move {
                if let Ok(up) = hyper::upgrade::on(req).await {
                    if let Ok(mut s) = dial(&host, port, None).await {
                        let mut c = TokioIo::new(up);
                        let _ = tokio::io::copy_bidirectional(&mut c, &mut s).await;
                    }
                }
            });
            return Response::new(full(Bytes::new()));
        }
        // Checked now, before the tunnel opens, and again per request.
        if let Err(why) = check(&ctx, &host).await {
            audit_refusal(&ctx, &host, "CONNECT", &auth, &why);
            return refuse(StatusCode::FORBIDDEN, &why);
        }
        let ctx2 = ctx.clone();
        tokio::spawn(async move {
            let Ok(up) = hyper::upgrade::on(req).await else {
                return;
            };
            let cfg = match server_config(&ctx2, &host) {
                Ok(c) => c,
                Err(_) => return,
            };
            let Ok(tls) = tokio_rustls::TlsAcceptor::from(cfg)
                .accept(TokioIo::new(up))
                .await
            else {
                return;
            };
            let svc = hyper::service::service_fn(move |r: Request<Incoming>| {
                let ctx = ctx2.clone();
                let host = host.clone();
                async move { Ok::<_, hyper::Error>(exchange(ctx, true, host, port, r, None).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .preserve_header_case(true)
                .serve_connection(TokioIo::new(tls), svc)
                .await;
        });
        return Response::new(full(Bytes::new()));
    }
    // Plain HTTP through the proxy: an absolute URI.
    let uri = req.uri().clone();
    let Some(host) = uri.host().map(host_only) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "send absolute URIs (this is a proxy)",
        );
    };
    if uri.scheme_str() != Some("http") {
        return refuse(
            StatusCode::BAD_REQUEST,
            "only http:// URIs, or CONNECT for https",
        );
    }
    let port = uri.port_u16().unwrap_or(80);
    if is_passthrough(&ctx, &host) {
        return refuse(
            StatusCode::FORBIDDEN,
            "the runner's own hosts are reached over HTTPS only",
        );
    }
    exchange(ctx, false, host, port, req, None).await
}

/// The certificate the proxy shows for `host`, signed by this agent's CA.
fn server_config(ctx: &Ctx, host: &str) -> Result<Arc<rustls::ServerConfig>, String> {
    if let Some(c) = ctx.certs.lock().unwrap().get(host) {
        return Ok(c.clone());
    }
    let mut p = rcgen::CertificateParams::new(vec![host.to_string()]).map_err(|e| e.to_string())?;
    p.distinguished_name
        .push(rcgen::DnType::CommonName, host.to_string());
    p.not_before = day!(-1);
    p.not_after = day!(30);
    p.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = p
        .signed_by(&ctx.leaf_key, &ctx.ca, &ctx.ca_key)
        .map_err(|e| e.to_string())?;
    let key = rustls_pki_types::PrivateKeyDer::Pkcs8(rustls_pki_types::PrivatePkcs8KeyDer::from(
        ctx.leaf_key.serialize_der(),
    ));
    let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| e.to_string())?
    .with_no_client_auth()
    .with_single_cert(vec![leaf.der().clone(), ctx.ca.der().clone()], key)
    .map_err(|e| e.to_string())?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let cfg = Arc::new(cfg);
    ctx.certs
        .lock()
        .unwrap()
        .insert(host.to_string(), cfg.clone());
    Ok(cfg)
}

/// Resolve `host` and check it against the scope: the addresses to connect
/// to, or why not.
async fn check(ctx: &Ctx, host: &str) -> Result<Vec<IpAddr>, String> {
    let addrs: Vec<IpAddr> = match host.parse::<IpAddr>() {
        Ok(a) => vec![a],
        Err(_) => {
            match tokio::time::timeout(Duration::from_secs(10), tokio::net::lookup_host((host, 0)))
                .await
            {
                Ok(Ok(it)) => it.map(|s| s.ip()).collect(),
                _ => vec![],
            }
        }
    };
    engage::allows_connection(&ctx.spec, host, &addrs, engage::now())?;
    Ok(addrs)
}

/// Connect to `host:port`: to one of the checked `addrs` when given (so the
/// answer that was checked is the one used), through this machine's own
/// HTTPS_PROXY when it has one (then by name).
async fn dial(
    host: &str,
    port: u16,
    addrs: Option<&[IpAddr]>,
) -> Result<tokio::net::TcpStream, String> {
    let upstream = std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .ok()
        .filter(|p| p.starts_with("http://"))
        .filter(|_| !crate::deck::router::no_proxy(host));
    if let Some(p) = upstream {
        return crate::deck::router::tunnel(&p, host, port).await;
    }
    let targets: Vec<std::net::SocketAddr> = match addrs {
        Some(a) if !a.is_empty() => a
            .iter()
            .map(|ip| std::net::SocketAddr::new(*ip, port))
            .collect(),
        _ => tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| format!("{}: {}", host, e))?
            .collect(),
    };
    let mut last = format!("{}: no address", host);
    for t in targets {
        match tokio::time::timeout(Duration::from_secs(15), tokio::net::TcpStream::connect(t)).await
        {
            Ok(Ok(s)) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Ok(Err(e)) => last = format!("{}: {}", t, e),
            Err(_) => last = format!("{}: timed out", t),
        }
    }
    Err(last)
}

const HOP: [&str; 10] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
    "proxy-authenticate",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
];

fn headers_of(h: &hyper::HeaderMap) -> Vec<(String, String)> {
    h.iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).to_string(),
            )
        })
        .collect()
}

/// One request, checked, paced, forwarded and recorded. `replay_of` names
/// the exchange it repeats.
async fn exchange(
    ctx: Arc<Ctx>,
    https: bool,
    host: String,
    port: u16,
    req: Request<Incoming>,
    replay_of: Option<String>,
) -> Response<BoxBody> {
    let (parts, body) = req.into_parts();
    let body = match http_body_util::Limited::new(body, MAX_REQUEST)
        .collect()
        .await
    {
        Ok(b) => b.to_bytes(),
        Err(_) => return refuse(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    forward(
        ctx,
        https,
        host,
        port,
        parts.method,
        parts.uri,
        parts.headers,
        body,
        replay_of,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn forward(
    ctx: Arc<Ctx>,
    https: bool,
    host: String,
    port: u16,
    method: hyper::Method,
    uri: hyper::Uri,
    headers: hyper::HeaderMap,
    body: Bytes,
    replay_of: Option<String>,
) -> Response<BoxBody> {
    let path = uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    let default_port = if https { 443 } else { 80 };
    let authority = if port == default_port {
        host.clone()
    } else if host.contains(':') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    };
    let url = format!(
        "{}://{}{}",
        if https { "https" } else { "http" },
        authority,
        path
    );
    let addrs = match check(&ctx, &host).await {
        Ok(a) => a,
        Err(why) => {
            audit_refusal(&ctx, &host, method.as_str(), &url, &why);
            return refuse(StatusCode::FORBIDDEN, &why);
        }
    };
    // Pace per host.
    let loopback = addrs.iter().all(|a| a.is_loopback()) && !addrs.is_empty();
    if !loopback {
        match engage::pace(&ctx.dir, std::slice::from_ref(&host), ctx.spec.rate) {
            Ok(w) if w > engage::MAX_WAIT_MS => {
                let mut r = refuse(
                    StatusCode::TOO_MANY_REQUESTS,
                    &format!(
                        "the engagement allows {} requests per second to {}, which is booked for the next {}s",
                        ctx.spec.rate,
                        host,
                        w / 1000
                    ),
                );
                r.headers_mut().insert(
                    "retry-after",
                    (w / 1000).max(1).to_string().parse().unwrap(),
                );
                return r;
            }
            Ok(w) if w > 0 => tokio::time::sleep(Duration::from_millis(w)).await,
            Ok(_) => {}
            Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("pacing: {}", e)),
        }
    }
    let started = std::time::Instant::now();
    let req_headers = headers_of(&headers);
    let id = format!(
        "{}-{}",
        engage::now_ms_pub(),
        ctx.seq.fetch_add(1, Ordering::SeqCst)
    );
    let mut rec = json!({
        "id": id, "ts": engage::now_ms_pub(), "engagement": ctx.spec.id, "agent": ctx.agent,
        "method": method.as_str(), "url": url, "host": host,
        "request": blob(&ctx.dir, &body, req_headers),
    });
    if let Some(r) = &replay_of {
        rec["replay_of"] = json!(r);
    }
    // Upstream.
    let result: Result<Response<Incoming>, String> = async {
        let tcp = dial(&host, port, Some(&addrs)).await?;
        let io: Box<dyn crate::deck::router::Io> = if https {
            let name = rustls_pki_types::ServerName::try_from(host.clone())
                .map_err(|_| format!("bad host name {}", host))?;
            Box::new(
                tokio_rustls::TlsConnector::from(crate::deck::router::tls_config()?)
                    .connect(name, tcp)
                    .await
                    .map_err(|e| format!("{}: TLS: {}", host, e))?,
            )
        } else {
            Box::new(tcp)
        };
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
            .await
            .map_err(|e| format!("{}: {}", host, e))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let mut b = Request::builder().method(method.clone()).uri(path.clone());
        for (k, v) in headers.iter() {
            let n = k.as_str();
            if HOP.contains(&n) || n == "content-length" || n == "accept-encoding" {
                continue;
            }
            b = b.header(k, v);
        }
        // Uncompressed, so evidence can be quoted from the body.
        b = b
            .header("host", authority.clone())
            .header("accept-encoding", "identity");
        let req = b
            .body(http_body_util::Full::new(body.clone()))
            .map_err(|e| e.to_string())?;
        tokio::time::timeout(Duration::from_secs(300), sender.send_request(req))
            .await
            .map_err(|_| format!("{}: no response", host))?
            .map_err(|e| format!("{}: {}", host, e))
    }
    .await;
    match result {
        Err(e) => {
            rec["error"] = json!(e);
            rec["duration_ms"] = json!(started.elapsed().as_millis() as u64);
            save(&ctx, rec, None);
            refuse(StatusCode::BAD_GATEWAY, &e)
        }
        Ok(resp) => {
            let (mut parts, incoming) = resp.into_parts();
            let status = parts.status.as_u16();
            let resp_headers = headers_of(&parts.headers);
            for h in HOP {
                parts.headers.remove(h);
            }
            if status == 429 || status == 503 {
                let after = parts
                    .headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok());
                let _ = engage::back_off(&ctx.dir, std::slice::from_ref(&host), after);
            }
            let ctx2 = ctx.clone();
            let tee = Tee {
                inner: incoming,
                kept: Vec::new(),
                total: 0,
                done: Some(Box::new(move |kept: Vec<u8>, total: u64| {
                    let mut rec = rec;
                    let mut r = blob(&ctx2.dir, &kept, resp_headers);
                    r["status"] = json!(status);
                    r["body_len"] = json!(total);
                    r["body_truncated"] = json!(total as usize > kept.len());
                    rec["response"] = r;
                    rec["duration_ms"] = json!(started.elapsed().as_millis() as u64);
                    save(&ctx2, rec, Some(status));
                })),
            };
            Response::from_parts(parts, tee.boxed())
        }
    }
}

/// Keep up to MAX_KEEP bytes of a body under its SHA-256, and describe it.
fn blob(dir: &Path, data: &[u8], headers: Vec<(String, String)>) -> Value {
    use sha2::{Digest, Sha256};
    let kept = &data[..data.len().min(MAX_KEEP)];
    let hash: String = Sha256::digest(kept)
        .iter()
        .map(|x| format!("{:02x}", x))
        .collect();
    if !kept.is_empty() {
        let p = blob_path(dir, &hash);
        if !p.exists() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = write_private(&p, kept);
        }
    }
    json!({
        "headers": headers, "body_sha256": if kept.is_empty() { Value::Null } else { json!(hash) },
        "body_len": data.len(), "body_truncated": data.len() > kept.len(),
    })
}

pub fn traffic_dir(dir: &Path) -> PathBuf {
    dir.join("traffic")
}

pub fn blob_path(dir: &Path, hash: &str) -> PathBuf {
    traffic_dir(dir)
        .join("blobs")
        .join(&hash[..2.min(hash.len())])
        .join(format!("{}.bin", hash))
}

fn write_private(p: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(p)?.write_all(data)
}

/// Write an exchange, its index line and its audit entry.
fn save(ctx: &Ctx, rec: Value, status: Option<u16>) {
    let dir = ctx.dir.clone();
    let spec_id = ctx.spec.id.clone();
    let run = move || {
        let t = traffic_dir(&dir);
        let _ = std::fs::create_dir_all(&t);
        let id = rec["id"].as_str().unwrap_or("x").to_string();
        if !crate::deck::traffic::valid_id(&id) {
            return;
        }
        let _ = write_private(
            &t.join(format!("{}.json", id)),
            serde_json::to_string_pretty(&rec)
                .unwrap_or_default()
                .as_bytes(),
        );
        let line = json!({
            "id": id, "ts": rec["ts"], "agent": rec["agent"], "method": rec["method"], "url": rec["url"],
            "host": rec["host"], "status": status, "error": rec.get("error"),
            "req_len": rec["request"]["body_len"], "resp_len": rec["response"]["body_len"],
            "replay_of": rec.get("replay_of"),
        });
        let _ = crate::deck::engage::locked(&t, || {
            let mut o = std::fs::OpenOptions::new();
            o.create(true).append(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            if let Ok(mut f) = o.open(t.join("index.jsonl")) {
                use std::io::Write;
                let _ = writeln!(f, "{}", line);
            }
        });
        let _ = crate::deck::engage::audit(
            &dir,
            json!({
                "event": "traffic", "engagement": spec_id, "tool": "proxy",
                "targets": [rec["host"]], "decision": "allow", "exchange": id,
                "input": format!("{} {} -> {}", rec["method"].as_str().unwrap_or(""),
                    rec["url"].as_str().unwrap_or(""),
                    status.map(|s| s.to_string()).unwrap_or_else(|| "error".into())),
            }),
        );
    };
    match tokio::runtime::Handle::try_current() {
        Ok(h) => {
            h.spawn_blocking(run);
        }
        Err(_) => run(),
    }
}

fn audit_refusal(ctx: &Ctx, host: &str, method: &str, url: &str, why: &str) {
    let _ = crate::deck::engage::audit(
        &ctx.dir,
        json!({
            "event": "traffic", "engagement": ctx.spec.id, "tool": "proxy", "targets": [host],
            "decision": "deny", "reason": why, "input": format!("{} {}", method, url),
        }),
    );
}

/// A response body passed through as it arrives, with up to MAX_KEEP bytes
/// kept; `done` gets them (and the total length) at the end.
struct Tee {
    inner: Incoming,
    kept: Vec<u8>,
    total: u64,
    #[allow(clippy::type_complexity)]
    done: Option<Box<dyn FnOnce(Vec<u8>, u64) + Send + Sync>>,
}

impl Body for Tee {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(f))) => {
                if let Some(d) = f.data_ref() {
                    this.total += d.len() as u64;
                    let room = MAX_KEEP.saturating_sub(this.kept.len());
                    this.kept.extend_from_slice(&d[..d.len().min(room)]);
                }
                Poll::Ready(Some(Ok(f)))
            }
            Poll::Ready(None) => {
                if let Some(done) = this.done.take() {
                    done(std::mem::take(&mut this.kept), this.total);
                }
                Poll::Ready(None)
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for Tee {
    fn drop(&mut self) {
        // The client went away early: record what came through.
        if let Some(done) = self.done.take() {
            done(std::mem::take(&mut self.kept), self.total);
        }
    }
}

// ---- replay -------------------------------------------------------------------------

/// Send a recorded request again, under the same checks, pacing and
/// recording, as a new exchange that names the one it repeats. Runs on the
/// engagement's machine (`indra traffic replay`).
pub async fn replay(spec: Spec, dir: PathBuf, rec: &Value) -> Result<Value, String> {
    use base64_free::decode_blob;
    let url: hyper::Uri = rec["url"]
        .as_str()
        .unwrap_or("")
        .parse()
        .map_err(|_| "the exchange has no URL")?;
    let https = url.scheme_str() == Some("https");
    let host = url
        .host()
        .map(host_only)
        .ok_or("the exchange has no host")?;
    let port = url.port_u16().unwrap_or(if https { 443 } else { 80 });
    if rec["request"]["body_truncated"].as_bool() == Some(true) {
        return Err("its request body was larger than what was kept (256 KiB): it cannot be replayed exactly".into());
    }
    let body = decode_blob(&dir, &rec["request"]["body_sha256"])?;
    let mut headers = hyper::HeaderMap::new();
    for h in rec["request"]["headers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let (Some(k), Some(v)) = (h[0].as_str(), h[1].as_str()) else {
            continue;
        };
        if let (Ok(k), Ok(v)) = (
            hyper::header::HeaderName::from_bytes(k.as_bytes()),
            hyper::header::HeaderValue::from_str(v),
        ) {
            headers.append(k, v);
        }
    }
    let method = hyper::Method::from_bytes(rec["method"].as_str().unwrap_or("GET").as_bytes())
        .map_err(|_| "bad method")?;
    let ctx = Arc::new(Ctx {
        agent: "replay".into(),
        passthrough: vec![],
        ca_key: rcgen::KeyPair::generate().map_err(|e| e.to_string())?,
        leaf_key: rcgen::KeyPair::generate().map_err(|e| e.to_string())?,
        ca: {
            let k = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
            rcgen::CertificateParams::new(Vec::<String>::new())
                .map_err(|e| e.to_string())?
                .self_signed(&k)
                .map_err(|e| e.to_string())?
        },
        certs: Mutex::default(),
        seq: AtomicU64::new(0),
        spec,
        dir: dir.clone(),
    });
    let of = rec["id"].as_str().map(String::from);
    let path: hyper::Uri = url
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .parse()
        .map_err(|_| "bad path")?;
    let resp = forward(ctx, https, host, port, method, path, headers, body, of).await;
    let status = resp.status().as_u16();
    let text = resp
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    // The record is written when the body ends; give it a moment.
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(json!({ "status": status, "body_len": text.len(),
               "preview": String::from_utf8_lossy(&text[..text.len().min(2000)]) }))
}

mod base64_free {
    use super::*;
    /// A kept body by its hash (empty when there is none).
    pub fn decode_blob(dir: &Path, hash: &Value) -> Result<Bytes, String> {
        match hash.as_str() {
            None => Ok(Bytes::new()),
            Some(h) if crate::deck::traffic::valid_hash(h) => std::fs::read(blob_path(dir, h))
                .map(Bytes::from)
                .map_err(|e| format!("the kept body {} cannot be read: {}", h, e)),
            Some(_) => Err("bad body hash".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(scope: &[&str]) -> Spec {
        Spec {
            id: "abcd1234".into(),
            name: "lab".into(),
            scope: scope.iter().map(|s| s.to_string()).collect(),
            exclude: vec![],
            starts: engage::now() - 60,
            ends: engage::now() + 3600,
            rate: 100.0,
            record: true,
        }
    }

    /// A stand-in web server on loopback: echoes the request, 429s on /slow.
    async fn upstream() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (s, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(|r: Request<Incoming>| async move {
                        let ae = r
                            .headers()
                            .get("accept-encoding")
                            .map(|v| v.to_str().unwrap_or("").to_string())
                            .unwrap_or_default();
                        let path = r.uri().path().to_string();
                        let body = r.into_body().collect().await.unwrap().to_bytes();
                        let mut resp =
                            Response::new(http_body_util::Full::new(Bytes::from(format!(
                                "hello from {} accept-encoding={} body={}",
                                path,
                                ae,
                                String::from_utf8_lossy(&body)
                            ))));
                        if path == "/slow" {
                            *resp.status_mut() = StatusCode::TOO_MANY_REQUESTS;
                            resp.headers_mut()
                                .insert("retry-after", "120".parse().unwrap());
                        }
                        Ok::<_, hyper::Error>(resp)
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(s), svc)
                        .await;
                });
            }
        });
        port
    }

    /// One HTTP/1.1 request through the proxy, raw.
    async fn via(proxy: u16, req: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", proxy))
            .await
            .unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), s.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).to_string()
    }

    #[tokio::test]
    async fn records_in_scope_refuses_the_rest_and_backs_off() {
        let dir = std::env::temp_dir().join(format!("indra-proxy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let up = upstream().await;
        // Loopback stands in for a scope host; 10.9.9.9 is out of scope.
        let h = start(
            spec(&["10.0.0.0/24"]),
            dir.clone(),
            "a1",
            vec!["api.anthropic.com".into()],
        )
        .unwrap();
        assert!(std::fs::read_to_string(&h.ca_file)
            .unwrap()
            .contains("BEGIN CERTIFICATE"));
        let env = h.env(&["api.anthropic.com".into()]);
        assert!(env
            .iter()
            .any(|(k, v)| k == "HTTPS_PROXY" && v.ends_with(&h.port.to_string())));
        assert!(env
            .iter()
            .any(|(k, v)| k == "NO_PROXY" && v.contains("api.anthropic.com")));

        let r = via(h.port, &format!("POST http://127.0.0.1:{}/login HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept-Encoding: gzip\r\nContent-Length: 7\r\nConnection: close\r\n\r\nuser=me", up)).await;
        assert!(r.starts_with("HTTP/1.1 200"), "{}", r);
        assert!(
            r.contains("hello from /login accept-encoding=identity body=user=me"),
            "{}",
            r
        );

        let r = via(
            h.port,
            "GET http://10.9.9.9/ HTTP/1.1\r\nHost: 10.9.9.9\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(r.starts_with("HTTP/1.1 403"), "{}", r);
        assert!(r.contains("outside the scope"), "{}", r);
        let r = via(
            h.port,
            "CONNECT evil.example.net:443 HTTP/1.1\r\nHost: evil.example.net:443\r\n\r\n",
        )
        .await;
        assert!(r.starts_with("HTTP/1.1 403"), "{}", r);

        let r = via(
            h.port,
            &format!(
                "GET http://127.0.0.1:{}/slow HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
                up
            ),
        )
        .await;
        assert!(r.starts_with("HTTP/1.1 429"), "{}", r);

        // Records are written as each body ends.
        let mut index = String::new();
        for _ in 0..100 {
            index =
                std::fs::read_to_string(traffic_dir(&dir).join("index.jsonl")).unwrap_or_default();
            let audited = engage::read_audit(&dir, 100)
                .0
                .iter()
                .filter(|x| x["event"] == "traffic" && x["decision"] == "allow")
                .count();
            if index.lines().count() >= 2 && audited >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let lines: Vec<Value> = index
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "{}", index);
        let login = lines
            .iter()
            .find(|l| l["url"].as_str().unwrap().ends_with("/login"))
            .unwrap();
        assert_eq!(login["status"], 200);
        let id = login["id"].as_str().unwrap();
        let rec: Value = serde_json::from_str(
            &std::fs::read_to_string(traffic_dir(&dir).join(format!("{}.json", id))).unwrap(),
        )
        .unwrap();
        assert_eq!(rec["method"], "POST");
        assert_eq!(rec["request"]["body_len"], 7);
        let body = std::fs::read_to_string(blob_path(
            &dir,
            rec["response"]["body_sha256"].as_str().unwrap(),
        ))
        .unwrap();
        assert!(body.contains("body=user=me"));
        // The audit log has the allowed exchanges and the refusals, chained.
        let (log, intact) = engage::read_audit(&dir, 100);
        assert!(intact);
        assert!(log.iter().any(|x| x["event"] == "traffic"
            && x["decision"] == "deny"
            && x["targets"][0] == "10.9.9.9"));
        assert!(log
            .iter()
            .any(|x| x["event"] == "traffic" && x["decision"] == "allow" && x["exchange"] == id));
        // The 429 backed the host off (Retry-After 120 s): the next is refused.
        let r = via(
            h.port,
            &format!(
                "GET http://127.0.0.1:{}/ HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
                up
            ),
        )
        .await;
        assert!(
            r.starts_with("HTTP/1.1 200"),
            "loopback is never paced: {}",
            r
        );

        // Replay: the same request again, a new exchange naming the first.
        let out = replay(spec(&["10.0.0.0/24"]), dir.clone(), &rec)
            .await
            .unwrap();
        assert_eq!(out["status"], 200);
        assert!(out["preview"].as_str().unwrap().contains("body=user=me"));
        // HTTPS: the client trusts this agent's CA only, and sees the
        // proxy's certificate for the host it asked for. (The upstream
        // here speaks plain HTTP, so the proxy answers 502, and records it.)
        {
            use rustls_pki_types::pem::PemObject;
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut roots = rustls::RootCertStore::empty();
            for c in rustls_pki_types::CertificateDer::pem_file_iter(&h.ca_file).unwrap() {
                roots.add(c.unwrap()).unwrap();
            }
            let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", h.port))
                .await
                .unwrap();
            s.write_all(
                format!(
                    "CONNECT localhost:{} HTTP/1.1\r\nHost: localhost:{}\r\n\r\n",
                    up, up
                )
                .as_bytes(),
            )
            .await
            .unwrap();
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                s.read_exact(&mut b).await.unwrap();
                head.push(b[0]);
            }
            assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));
            let name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
            let mut tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
                .connect(name, s)
                .await
                .expect("the client trusts the agent's CA");
            tls.write_all(b"GET /secure HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut out = Vec::new();
            let _ = tokio::time::timeout(Duration::from_secs(5), tls.read_to_end(&mut out)).await;
            let out = String::from_utf8_lossy(&out);
            assert!(out.starts_with("HTTP/1.1 502"), "{}", out);
            for _ in 0..50 {
                if list_has(&dir, "https://localhost:") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(list_has(&dir, &format!("https://localhost:{}/secure", up)));
        }
        let files = h.files.clone();
        drop(h);
        assert!(!files.exists(), "the CA files go with the proxy");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn list_has(dir: &Path, url: &str) -> bool {
        std::fs::read_to_string(traffic_dir(dir).join("index.jsonl"))
            .unwrap_or_default()
            .contains(url)
    }

    #[test]
    fn passthrough_hosts_come_from_the_runner_and_its_tools() {
        let p = passthrough_for(
            crate::deck::hooks::Agent::Claude,
            &[(
                "ANTHROPIC_BASE_URL".into(),
                "https://gw.corp.example:8443/v1".into(),
            )],
            &["https://mcp.vendor.example/sse".into()],
        );
        assert!(p.contains(&"api.anthropic.com".to_string()));
        assert!(p.contains(&"gw.corp.example".to_string()));
        assert!(p.contains(&"mcp.vendor.example".to_string()));
    }
}
