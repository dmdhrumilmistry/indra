//! Engagements: tests against live systems, inside a written scope.
//!
//! An engagement names what may be tested (hosts, `*.domain` suffixes,
//! IPs and CIDRs, minus exclusions), when (a time window), on which one
//! machine its agents run, at what pace (requests per second per host),
//! and who authorized it. Agents started in it run with `indra guard` as
//! Claude Code's PreToolUse and PostToolUse hook: every tool call that
//! names a host is checked against the scope and the window, paced per
//! host, and written to an append-only audit log whose entries are chained
//! by SHA-256, so an edit or a deletion shows.
//!
//! The guard reads the hosts a tool call names (URLs, IPs, CIDRs, host
//! names); it does not see what a program resolves or connects to on its
//! own. It is a policy layer for the agent, not a network sandbox: pair it
//! with a firewall on the test machine for hard enforcement.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What an agent in an engagement is given (through its environment).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Spec {
    pub id: String,
    pub name: String,
    pub scope: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Unix seconds.
    pub starts: u64,
    pub ends: u64,
    /// Tool calls per second, per host.
    pub rate: f64,
    /// Send its agents' web traffic through indra's recording proxy: the
    /// scope is then enforced on every connection, and each request and
    /// response is kept as evidence.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub record: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Engagement {
    #[serde(flatten)]
    pub spec: Spec,
    /// The machine its agents run on ("local" or a node's name).
    pub machine: String,
    /// Who authorized the test, and the reference (contract, ticket).
    pub authorization: String,
    pub created_by: String,
    pub created: u64,
    /// What all its agents together may spend on API calls (US dollars).
    /// Near it they are told to wrap up; at it they are stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Unix milliseconds.
pub fn now_ms_pub() -> u64 {
    now_ms()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// An engagement id: 8 lowercase letters and digits.
pub fn valid_id(id: &str) -> bool {
    id.len() == 8
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

// ---- scope ----------------------------------------------------------------------

/// One scope or exclusion entry.
#[derive(Debug, Clone, PartialEq)]
enum Rule {
    /// Exactly this host name.
    Host(String),
    /// `*.example.com`: any subdomain of it (not the name itself).
    Sub(String),
    /// An address or a network.
    Net(IpAddr, u8),
}

fn norm_host(h: &str) -> String {
    h.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

fn parse_net(s: &str) -> Option<(IpAddr, u8)> {
    let (a, p) = match s.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (s, None),
    };
    let ip: IpAddr = a.trim_matches(|c| c == '[' || c == ']').parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let p = match p {
        Some(p) => p.parse::<u8>().ok().filter(|p| *p <= max)?,
        None => max,
    };
    Some((ip, p))
}

impl Rule {
    fn parse(entry: &str) -> Result<Rule, String> {
        let e = entry.trim();
        // A URL: its host.
        let e = match e.split_once("://") {
            Some((_, rest)) => rest
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("")
                .rsplit('@')
                .next()
                .unwrap_or(""),
            None => e,
        };
        if let Some(n) = parse_net(e) {
            return Ok(Rule::Net(n.0, n.1));
        }
        // host:port -> host
        let e = match e.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok() && !h.contains(':') => h,
            _ => e,
        };
        if let Some(base) = e.strip_prefix("*.") {
            let b = norm_host(base);
            if valid_host(&b) && b.contains('.') {
                return Ok(Rule::Sub(b));
            }
        } else {
            let h = norm_host(e);
            if valid_host(&h) {
                return Ok(Rule::Host(h));
            }
        }
        Err(format!(
            "{:?} is not a host, *.domain, IP or CIDR (like app.example.com, *.example.com, 10.0.0.5 or 10.0.0.0/24)",
            entry
        ))
    }

    fn covers(&self, t: &Target) -> bool {
        match (self, t) {
            (Rule::Host(h), Target::Host(x)) => h == x,
            (Rule::Sub(b), Target::Host(x)) => {
                x.len() > b.len() + 1 && x.ends_with(&format!(".{}", b))
            }
            (Rule::Net(n, p), Target::Net(a, q)) => q >= p && in_net(*a, *n, *p),
            _ => false,
        }
    }

    /// For an exclusion: does it touch any part of the target?
    fn overlaps(&self, t: &Target) -> bool {
        match (self, t) {
            (Rule::Net(n, p), Target::Net(a, q)) => in_net(*a, *n, *p) || in_net(*n, *a, *q),
            _ => self.covers(t),
        }
    }
}

fn in_net(a: IpAddr, net: IpAddr, prefix: u8) -> bool {
    match (a, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) => {
            let m = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix as u32)
            };
            u32::from(a) & m == u32::from(n) & m
        }
        (IpAddr::V6(a), IpAddr::V6(n)) => {
            let m = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix as u32)
            };
            u128::from(a) & m == u128::from(n) & m
        }
        _ => false,
    }
}

/// A host a tool call names.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Host(String),
    /// An address (prefix 32 or 128) or a range.
    Net(IpAddr, u8),
}

impl Target {
    pub fn show(&self) -> String {
        match self {
            Target::Host(h) => h.clone(),
            Target::Net(a, p) if (a.is_ipv4() && *p == 32) || *p == 128 => a.to_string(),
            Target::Net(a, p) => format!("{}/{}", a, p),
        }
    }

    fn loopback(&self) -> bool {
        match self {
            Target::Host(h) => h == "localhost" || h.ends_with(".localhost"),
            Target::Net(a, _) => a.is_loopback(),
        }
    }
}

impl Spec {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err("bad engagement id".into());
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 80 {
            return Err("an engagement needs a name of up to 80 characters".into());
        }
        if self.scope.is_empty() {
            return Err(
                "an engagement needs a scope: the hosts, domains, IPs or CIDRs that may be tested"
                    .into(),
            );
        }
        if self.scope.len() + self.exclude.len() > 500 {
            return Err("at most 500 scope and exclusion entries".into());
        }
        for e in self.scope.iter().chain(&self.exclude) {
            Rule::parse(e)?;
        }
        if self.ends <= self.starts {
            return Err("the window must end after it starts".into());
        }
        if self.ends - self.starts > 90 * 86_400 {
            return Err("the window can be at most 90 days".into());
        }
        if !(0.1..=100.0).contains(&self.rate) {
            return Err("the rate is 0.1 to 100 requests per second per host".into());
        }
        Ok(())
    }

    pub fn active(&self, t: u64) -> bool {
        t >= self.starts && t < self.ends
    }

    /// Whether `t` may be tested: in the scope, not excluded. Loopback is
    /// this machine itself, always allowed.
    pub fn allows(&self, t: &Target) -> bool {
        if t.loopback() {
            return true;
        }
        let rules =
            |v: &[String]| -> Vec<Rule> { v.iter().filter_map(|e| Rule::parse(e).ok()).collect() };
        if rules(&self.exclude).iter().any(|r| r.overlaps(t)) {
            return false;
        }
        rules(&self.scope).iter().any(|r| r.covers(t))
    }
}

/// Whether a connection to `host`, which resolved to `addrs`, may be made
/// now: inside the window, the name in scope (and none of its addresses
/// excluded), or else every address it resolved to in scope. Loopback is
/// this machine. The reason when not.
pub fn allows_connection(spec: &Spec, host: &str, addrs: &[IpAddr], t: u64) -> Result<(), String> {
    let target = match parse_net(host) {
        Some((a, p)) => Target::Net(a, p),
        None => Target::Host(norm_host(host)),
    };
    if target.loopback() || (!addrs.is_empty() && addrs.iter().all(|a| a.is_loopback())) {
        return Ok(());
    }
    if !spec.active(t) {
        return Err(format!(
            "the engagement '{}' is not active now (its window is {} to {} UTC)",
            spec.name,
            when(spec.starts),
            when(spec.ends)
        ));
    }
    let net = |a: &IpAddr| Target::Net(*a, if a.is_ipv4() { 32 } else { 128 });
    let excluded: Vec<Rule> = spec
        .exclude
        .iter()
        .filter_map(|e| Rule::parse(e).ok())
        .collect();
    if let Some(a) = addrs
        .iter()
        .find(|a| excluded.iter().any(|r| r.overlaps(&net(a))))
    {
        return Err(format!(
            "{} resolves to {}, which the engagement '{}' excludes",
            host, a, spec.name
        ));
    }
    if spec.allows(&target) {
        return Ok(());
    }
    if !addrs.is_empty() && addrs.iter().all(|a| spec.allows(&net(a))) {
        return Ok(());
    }
    Err(format!(
        "{} is outside the scope of the engagement '{}' (in scope: {})",
        host,
        spec.name,
        spec.scope.join(", ")
    ))
}

/// Whether `text` (an alert) mentions `target` (as the audit log shows it):
/// the host name, or an IPv4 address inside the address or range.
pub fn mentions(target: &str, text: &str) -> bool {
    let low = text.to_ascii_lowercase();
    if let Some((net, p)) = parse_net(target) {
        let mut found = Vec::new();
        targets_in_text(text, None, &mut found);
        return found.iter().any(|t| match t {
            Target::Net(a, q) => *q == if a.is_ipv4() { 32 } else { 128 } && in_net(*a, net, p),
            _ => false,
        });
    }
    let t = norm_host(target);
    !t.is_empty() && low.contains(&t)
}

// ---- what a tool call names -----------------------------------------------------

/// Top-level domains a bare host name is recognised by. File extensions
/// that are also country domains (.md, .rs, .py, .sh, .pl, .so, .cc, .ps)
/// are left out, so file names are not taken for hosts.
const TLDS: &[&str] = &[
    "com", "net", "org", "io", "dev", "app", "ai", "co", "gov", "edu", "mil", "int", "info", "biz",
    "cloud", "tech", "xyz", "me", "us", "uk", "de", "fr", "in", "jp", "cn", "ru", "br", "au", "ca",
    "nl", "es", "it", "ch", "se", "no", "fi", "dk", "be", "at", "cz", "nz", "za", "kr", "tw", "hk",
    "sg", "id", "my", "ph", "vn", "th", "tr", "il", "ae", "sa", "eg", "ng", "ke", "ar", "mx", "cl",
    "pe", "ie", "pt", "gr", "hu", "ro", "ua", "eu", "asia", "online", "site", "store", "shop",
    "page", "live", "blog", "news", "local", "internal", "lan", "corp", "home", "test", "example",
    "invalid", "onion", "arpa", "global", "company", "services", "systems", "network",
];

fn targets_in_text(s: &str, cwd: Option<&Path>, out: &mut Vec<Target>) {
    use std::sync::OnceLock;
    static URL: OnceLock<regex::Regex> = OnceLock::new();
    static IP4: OnceLock<regex::Regex> = OnceLock::new();
    static NAME: OnceLock<regex::Regex> = OnceLock::new();
    let url = URL.get_or_init(|| {
        regex::Regex::new(r#"(?i)\b[a-z][a-z0-9+.-]{1,15}://([^\s/?#'"<>\\`|;,)]+)"#).unwrap()
    });
    let ip4 = IP4.get_or_init(|| {
        regex::Regex::new(r"\b(\d{1,3}(?:\.\d{1,3}){3})(?:/(\d{1,2}))?\b").unwrap()
    });
    let name = NAME.get_or_init(|| {
        regex::Regex::new(r"(?i)\b((?:[a-z0-9](?:[a-z0-9_-]{0,61}[a-z0-9])?\.)+[a-z]{2,24})\b")
            .unwrap()
    });
    let mut push = |t: Target| {
        if !out.contains(&t) {
            out.push(t)
        }
    };
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for c in url.captures_iter(s) {
        let m = c.get(1).unwrap();
        spans.push((m.start(), m.end()));
        let auth = m.as_str().rsplit('@').next().unwrap_or("");
        let host = if let Some(rest) = auth.strip_prefix('[') {
            rest.split(']').next().unwrap_or("").to_string()
        } else {
            auth.split(':').next().unwrap_or("").to_string()
        };
        if let Some((a, p)) = parse_net(&host) {
            push(Target::Net(a, p));
        } else {
            let h = norm_host(&host);
            if valid_host(&h) {
                push(Target::Host(h));
            }
        }
    }
    let inside = |i: usize| spans.iter().any(|(a, b)| i >= *a && i < *b);
    for c in ip4.captures_iter(s) {
        let m = c.get(0).unwrap();
        if inside(m.start()) {
            continue;
        }
        // Part of a version string or a longer dotted number (1.2.3.4.5).
        let before = s[..m.start()].chars().next_back();
        let after = s[m.end()..].chars().next();
        if matches!(before, Some('.') | Some('v') | Some('V'))
            || matches!(after, Some('.'))
                && s[m.end() + 1..].starts_with(|c: char| c.is_ascii_digit())
        {
            continue;
        }
        if let Some((a, p)) = parse_net(m.as_str()) {
            push(Target::Net(a, p));
        }
    }
    for c in name.captures_iter(s) {
        let m = c.get(1).unwrap();
        if inside(m.start()) {
            continue;
        }
        let tok = m.as_str();
        let tld = tok.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if !TLDS.contains(&tld.as_str()) || tok.chars().all(|c| c.is_ascii_digit() || c == '.') {
            continue;
        }
        // A path component (dir/name.ext) or a file that exists here.
        if matches!(
            s[..m.start()].chars().next_back(),
            Some('/') | Some('\\') | Some('.')
        ) {
            continue;
        }
        if let Some(d) = cwd {
            if d.join(tok).exists() {
                continue;
            }
        }
        push(Target::Host(norm_host(tok)));
    }
}

/// Tools that never reach another machine by themselves.
const LOCAL_TOOLS: &[&str] = &[
    "Read",
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "NotebookRead",
    "Glob",
    "Grep",
    "LS",
    "TodoWrite",
    "Task",
    "Agent",
    "WebSearch",
    "ExitPlanMode",
    "Skill",
    "SlashCommand",
    "BashOutput",
    "KillShell",
    "KillBash",
    // Gemini CLI
    "read_file",
    "read_many_files",
    "write_file",
    "replace",
    "glob",
    "grep_search",
    "search_file_content",
    "list_directory",
    "write_todos",
    "save_memory",
    "ask_user",
    "activate_skill",
    "get_internal_docs",
    "enter_plan_mode",
    "exit_plan_mode",
    "google_web_search",
    "update_topic",
    "complete_task",
    // Codex
    "apply_patch",
    "update_plan",
    "view_image",
    "write_stdin",
];

/// A tool's name as the guard reads it: the shell and web tools of Gemini
/// CLI and Codex under Claude Code's names.
pub fn canonical_tool(tool: &str) -> &str {
    match tool {
        "run_shell_command" | "exec_command" | "shell" | "local_shell" => "Bash",
        "web_fetch" => "WebFetch",
        _ => tool,
    }
}

/// The hosts a tool call names.
pub fn targets(tool: &str, input: &Value, cwd: Option<&Path>) -> Vec<Target> {
    let mut out = Vec::new();
    if LOCAL_TOOLS.contains(&tool) {
        return out;
    }
    // A command can come as one string or as argv (Codex's older shell).
    let text = |v: &Value| -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        }
    };
    match canonical_tool(tool) {
        "Bash" => {
            let c = if input.get("command").is_some() {
                text(&input["command"])
            } else {
                text(&input["cmd"])
            };
            targets_in_text(&c, cwd, &mut out)
        }
        // Claude Code takes a url; Gemini CLI a prompt with the URLs in it.
        "WebFetch" => {
            let t = format!("{} {}", text(&input["url"]), text(&input["prompt"]));
            targets_in_text(&t, None, &mut out)
        }
        _ => {
            // Any other tool (MCP tools included): every string it is given.
            fn walk(v: &Value, cwd: Option<&Path>, out: &mut Vec<Target>) {
                match v {
                    Value::String(s) => targets_in_text(s, cwd, out),
                    Value::Array(a) => a.iter().for_each(|x| walk(x, cwd, out)),
                    Value::Object(o) => o.values().for_each(|x| walk(x, cwd, out)),
                    _ => {}
                }
            }
            walk(input, cwd, &mut out);
        }
    }
    out
}

// ---- audit log ------------------------------------------------------------------

pub fn dir_of(base: &Path, id: &str) -> PathBuf {
    base.join("engagements").join(id)
}

/// Where this machine keeps engagement audit logs and pacing state.
pub fn local_dir(id: &str) -> PathBuf {
    dir_of(&crate::paths::data_dir(), id)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "..."
    }
}

/// Run `f` holding the engagement directory's lock (across processes).
pub(crate) fn locked<T>(dir: &Path, f: impl FnOnce() -> T) -> std::io::Result<T> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".lock"))?;
    lock.lock()?;
    let r = f();
    let _ = lock.unlock();
    Ok(r)
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_data()
}

/// Append an entry to the audit log in `dir`, chained to the one before.
pub fn audit(dir: &Path, mut entry: Value) -> std::io::Result<()> {
    locked(dir, || -> std::io::Result<()> {
        let head = dir.join("audit.head");
        let prev = std::fs::read_to_string(&head).unwrap_or_default();
        let o = entry.as_object_mut().expect("an object");
        o.insert("ts".into(), json!(now_ms()));
        o.insert("prev".into(), json!(prev.trim()));
        o.remove("hash");
        let body = serde_json::to_string(&entry).unwrap_or_default();
        let hash = hex(&Sha256::digest(body.as_bytes()));
        entry
            .as_object_mut()
            .unwrap()
            .insert("hash".into(), json!(hash));
        append_line(
            &dir.join("audit.jsonl"),
            &serde_json::to_string(&entry).unwrap_or_default(),
        )?;
        std::fs::write(&head, &hash)
    })?
}

/// The audit log's entries (the newest `max`), and whether its chain is
/// intact: each entry's hash matches its content and names the one before.
pub fn read_audit(dir: &Path, max: usize) -> (Vec<Value>, bool) {
    if !dir.join("audit.jsonl").exists() {
        return (vec![], true);
    }
    // Under the writers' lock, so an entry half-written is never read.
    locked(dir, || read_audit_unlocked(dir, max)).unwrap_or((vec![], false))
}

fn read_audit_unlocked(dir: &Path, max: usize) -> (Vec<Value>, bool) {
    let Ok(f) = std::fs::File::open(dir.join("audit.jsonl")) else {
        return (vec![], true);
    };
    let mut all = Vec::new();
    let mut prev = String::new();
    let mut intact = true;
    for line in std::io::BufReader::new(f).lines() {
        let Ok(line) = line else {
            intact = false;
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(mut v) = serde_json::from_str::<Value>(&line) else {
            intact = false;
            continue;
        };
        let hash = v["hash"].as_str().unwrap_or("").to_string();
        if v["prev"].as_str().unwrap_or("") != prev {
            intact = false;
        }
        v.as_object_mut().map(|o| o.remove("hash"));
        let body = serde_json::to_string(&v).unwrap_or_default();
        if hex(&Sha256::digest(body.as_bytes())) != hash {
            intact = false;
        }
        v.as_object_mut()
            .map(|o| o.insert("hash".into(), json!(hash.clone())));
        prev = hash;
        all.push(v);
    }
    let head = std::fs::read_to_string(dir.join("audit.head")).unwrap_or_default();
    if !all.is_empty() && head.trim() != prev {
        // The newest entries were cut off.
        intact = false;
    }
    let skip = all.len().saturating_sub(max);
    (all.split_off(skip), intact)
}

// ---- pacing ---------------------------------------------------------------------

/// How long to wait before the next call to each of `hosts`, reserving
/// the slots: calls to one host are spaced 1/rate seconds apart.
pub(crate) fn pace(dir: &Path, hosts: &[String], rate: f64) -> std::io::Result<u64> {
    locked(dir, || {
        let p = dir.join("pace.json");
        let mut state: std::collections::BTreeMap<String, u64> = std::fs::read_to_string(&p)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let t = now_ms();
        let step = (1000.0 / rate).ceil() as u64;
        let mut wait = 0;
        for h in hosts {
            let next = state.get(h).copied().unwrap_or(0).max(t);
            wait = wait.max(next - t);
            state.insert(h.clone(), next + step);
        }
        // Forget hosts not called for an hour.
        state.retain(|_, v| *v + 3_600_000 > t);
        let _ = std::fs::write(&p, serde_json::to_string(&state).unwrap_or_default());
        wait
    })
}

/// Waits longer than this are refused instead of slept through.
pub(crate) const MAX_WAIT_MS: u64 = 30_000;

// ---- the guard ------------------------------------------------------------------

/// What the guard decided for one PreToolUse call.
#[derive(Debug, PartialEq)]
pub enum Decision {
    Allow { waited_ms: u64 },
    Deny(String),
}

/// Decide a PreToolUse call: scope, window, pace. Writes the audit entry.
pub fn decide(spec: &Spec, dir: &Path, hook: &Value) -> Decision {
    let tool = hook["tool_name"].as_str().unwrap_or("");
    let input = &hook["tool_input"];
    let cwd = hook["cwd"].as_str().map(Path::new);
    let ts = targets(tool, input, cwd);
    let shown: Vec<String> = ts.iter().map(Target::show).collect();
    let d = if ts.is_empty() {
        Decision::Allow { waited_ms: 0 }
    } else if !spec.active(now()) {
        Decision::Deny(format!(
            "The engagement '{}' is not active now: it runs from {} to {} (UTC). Nothing that reaches another host may run outside its window.",
            spec.name,
            when(spec.starts),
            when(spec.ends)
        ))
    } else if let Some(out) = ts.iter().find(|t| !spec.allows(t)) {
        Decision::Deny(format!(
            "{} is outside the scope of the engagement '{}'. In scope: {}{}. Test only those hosts; ask the operator to widen the scope if this one is needed.",
            out.show(),
            spec.name,
            spec.scope.join(", "),
            if spec.exclude.is_empty() {
                String::new()
            } else {
                format!(" (excluded: {})", spec.exclude.join(", "))
            }
        ))
    } else {
        let hosts: Vec<String> = ts
            .iter()
            .filter(|t| !t.loopback())
            .map(Target::show)
            .collect();
        match pace(dir, &hosts, spec.rate) {
            Ok(w) if w > MAX_WAIT_MS => Decision::Deny(format!(
                "Rate limit: this engagement allows {} calls per second per host, and {} is booked for the next {}s. Slow down and try again later.",
                spec.rate,
                hosts.join(", "),
                w / 1000
            )),
            Ok(w) => {
                if w > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(w));
                }
                Decision::Allow { waited_ms: w }
            }
            Err(e) => Decision::Deny(format!("the engagement's pacing state cannot be read: {}", e)),
        }
    };
    let (decision, reason, waited) = match &d {
        Decision::Allow { waited_ms } => ("allow", String::new(), *waited_ms),
        Decision::Deny(r) => ("deny", r.clone(), 0),
    };
    let logged = audit(
        dir,
        json!({
            "event": "tool", "engagement": spec.id, "session": hook["session_id"],
            "tool": tool, "input": clip(&input.to_string(), 2000), "targets": shown,
            "decision": decision, "reason": reason, "waited_ms": waited,
        }),
    );
    match (d, logged) {
        // An unlogged action does not run.
        (Decision::Allow { .. }, Err(e)) => {
            Decision::Deny(format!("the audit log cannot be written: {}", e))
        }
        (d, _) => d,
    }
}

/// Record a PostToolUse call: what came of it. A host that answered 429
/// or 503 (rate limited, overloaded) is backed off: its next slot moves
/// out by its Retry-After, else by an interval that doubles each time.
pub fn record_result(spec: &Spec, dir: &Path, hook: &Value) {
    let tool = hook["tool_name"].as_str().unwrap_or("");
    let r = &hook["tool_response"];
    let failed = r["is_error"].as_bool().unwrap_or(false)
        || r["interrupted"].as_bool().unwrap_or(false)
        || r.get("error").map(|e| !e.is_null()).unwrap_or(false);
    let _ = audit(
        dir,
        json!({
            "event": "result", "engagement": spec.id, "session": hook["session_id"],
            "tool": tool, "ok": !failed, "output": clip(&r.to_string(), 600),
        }),
    );
    // The output as the program printed it (JSON escaping would hide
    // `"status": 429` inside a string).
    fn strings(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            Value::Number(n) => {
                out.push_str(&n.to_string());
                out.push('\n');
            }
            Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
            Value::Object(o) => o.iter().for_each(|(k, x)| {
                if let Value::Number(n) = x {
                    out.push_str(&format!("\"{}\": {}\n", k, n));
                } else {
                    strings(x, out)
                }
            }),
            _ => {}
        }
    }
    let mut text = String::new();
    strings(r, &mut text);
    if let Some(after) = throttled(&text) {
        let cwd = hook["cwd"].as_str().map(Path::new);
        let hosts: Vec<String> = targets(tool, &hook["tool_input"], cwd)
            .iter()
            .filter(|t| !t.loopback())
            .map(Target::show)
            .collect();
        if !hosts.is_empty() {
            if let Ok(wait) = back_off(dir, &hosts, after) {
                let _ = audit(
                    dir,
                    json!({
                        "event": "backoff", "engagement": spec.id, "session": hook["session_id"],
                        "tool": tool, "targets": hosts, "waited_ms": wait,
                        "reason": "the host answered 429 or 503: its next calls wait",
                    }),
                );
            }
        }
    }
}

/// Whether a tool's output shows the host throttling it (HTTP 429 or 503),
/// and the Retry-After it asked for, in seconds, when it gave one.
pub fn throttled(text: &str) -> Option<Option<u64>> {
    use std::sync::OnceLock;
    static STATUS: OnceLock<regex::Regex> = OnceLock::new();
    static AFTER: OnceLock<regex::Regex> = OnceLock::new();
    let status = STATUS.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(HTTP/[0-9.]+\s+(429|503)\b|\b429 Too Many Requests\b|\b503 Service Unavailable\b|"(status|status_code|statusCode)"\s*:\s*(429|503)\b|\bstatus(?: code)?[ =:]+(429|503)\b)"#,
        )
        .unwrap()
    });
    let after = AFTER.get_or_init(|| regex::Regex::new(r"(?i)retry-after:\s*(\d{1,6})\b").unwrap());
    status
        .is_match(text)
        .then(|| after.captures(text).and_then(|c| c[1].parse::<u64>().ok()))
}

/// Push the next slot of each of `hosts` out: by `retry_after` seconds when
/// the host gave one, else 2 s doubling with each throttle in a row (reset
/// after ten quiet minutes), at most 5 minutes. Returns the wait, in ms.
pub(crate) fn back_off(
    dir: &Path,
    hosts: &[String],
    retry_after: Option<u64>,
) -> std::io::Result<u64> {
    locked(dir, || {
        let p = dir.join("pace.json");
        let b = dir.join("backoff.json");
        let mut state: std::collections::BTreeMap<String, u64> = std::fs::read_to_string(&p)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let mut strikes: std::collections::BTreeMap<String, (u32, u64)> =
            std::fs::read_to_string(&b)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
        let t = now_ms();
        let mut wait = 0;
        for h in hosts {
            let (n, last) = strikes.get(h).copied().unwrap_or((0, 0));
            let n = if last + 600_000 < t { 1 } else { n + 1 };
            strikes.insert(h.clone(), (n, t));
            let w = match retry_after {
                Some(s) => s.clamp(1, 300) * 1000,
                None => (2_000u64 << (n - 1).min(8)).min(300_000),
            };
            let next = state.get(h).copied().unwrap_or(0).max(t + w);
            state.insert(h.clone(), next);
            wait = wait.max(next - t);
        }
        strikes.retain(|_, (_, last)| *last + 3_600_000 > t);
        let _ = std::fs::write(&p, serde_json::to_string(&state).unwrap_or_default());
        let _ = std::fs::write(&b, serde_json::to_string(&strikes).unwrap_or_default());
        wait
    })
}

// ---- coverage ------------------------------------------------------------------

/// What the engagement's agents actually touched, against its scope: for
/// each scope entry, the hosts under it that a tool call reached (allowed by
/// the guard) or that the recording proxy saw, and the entries nothing
/// reached yet. Refused hosts are listed apart.
pub fn coverage(spec: &Spec, entries: &[Value]) -> Value {
    use std::collections::BTreeMap;
    let rules: Vec<Option<Rule>> = spec.scope.iter().map(|e| Rule::parse(e).ok()).collect();
    // scope index -> host -> (calls, last ts)
    let mut hit: Vec<BTreeMap<String, (u64, u64)>> = vec![BTreeMap::new(); spec.scope.len()];
    let mut refused: BTreeMap<String, u64> = BTreeMap::new();
    let mut calls = 0u64;
    for x in entries {
        let kind = x["event"].as_str().unwrap_or("");
        if kind != "tool" && kind != "traffic" {
            continue;
        }
        let ts = x["ts"].as_u64().unwrap_or(0);
        let allowed = x["decision"] == "allow";
        let names: Vec<String> = x["targets"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if allowed && !names.is_empty() {
            calls += 1;
        }
        for n in names {
            let t = match parse_net(&n) {
                Some((a, p)) => Target::Net(a, p),
                None => Target::Host(norm_host(&n)),
            };
            if t.loopback() {
                continue;
            }
            if !allowed {
                *refused.entry(t.show()).or_default() += 1;
                continue;
            }
            for (i, r) in rules.iter().enumerate() {
                if r.as_ref().map(|r| r.covers(&t)).unwrap_or(false) {
                    let e = hit[i].entry(t.show()).or_default();
                    e.0 += 1;
                    e.1 = e.1.max(ts);
                }
            }
        }
    }
    let scope: Vec<Value> = spec
        .scope
        .iter()
        .zip(&hit)
        .map(|(entry, hosts)| {
            json!({
                "entry": entry,
                "tested": !hosts.is_empty(),
                "hosts": hosts.iter().map(|(h, (n, last))| json!({ "host": h, "calls": n, "last": last })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let tested = hit.iter().filter(|h| !h.is_empty()).count();
    let hosts: std::collections::BTreeSet<&String> = hit.iter().flat_map(|h| h.keys()).collect();
    json!({
        "summary": { "entries": spec.scope.len(), "tested": tested, "untested": spec.scope.len() - tested,
                     "hosts": hosts.len(), "calls": calls, "refused": refused.values().sum::<u64>() },
        "scope": scope,
        "refused": refused.iter().map(|(h, n)| json!({ "host": h, "calls": n })).collect::<Vec<_>>(),
    })
}

fn when(t: u64) -> String {
    let w = crate::deck::cron::from_utc(t as i64);
    format!("{:02}-{:02} {:02}:{:02}", w.month, w.day, w.hour, w.minute)
}

/// `indra guard pre|post|steer`: Claude Code's hook for agents in an
/// engagement (pre, post), and for steering any agent (steer; post too).
/// The engagement comes from INDRA_ENGAGEMENT; the call from stdin.
pub fn guard_main(phase: &str) -> std::process::ExitCode {
    use std::io::Read;
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let hook: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let event = hook["hook_event_name"]
        .as_str()
        .unwrap_or("PreToolUse")
        .to_string();
    let rules: crate::deck::approve::Rules = std::env::var("INDRA_APPROVALS")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let say = |v: Value| println!("{}", v);
    match phase {
        // Never refuses anything: it only hands over queued messages.
        "steer" => {
            crate::deck::steer::hook_main(&hook);
            return std::process::ExitCode::SUCCESS;
        }
        // Outside an engagement: the approval rules only.
        "rules" => {
            if let Some(v) = rule_answer(&rules, &hook, &event) {
                say(v);
            }
            return std::process::ExitCode::SUCCESS;
        }
        // Codex asks before running something its sandbox does not allow.
        "approve" => {
            let tool = hook["tool_name"].as_str().unwrap_or("");
            let decision = match rules.decide(canonical_tool(tool), &hook["tool_input"]) {
                crate::deck::approve::Verdict::Allow(_) => Some(json!({ "behavior": "allow" })),
                crate::deck::approve::Verdict::Deny(r) => Some(json!({ "behavior": "deny",
                    "message": format!("Refused by the approval rule `{}` set in the indra console.", r) })),
                crate::deck::approve::Verdict::Ask => None,
            };
            if let Some(d) = decision {
                say(
                    json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": d } }),
                );
            }
            return std::process::ExitCode::SUCCESS;
        }
        _ => {}
    }
    let spec: Option<Spec> = std::env::var("INDRA_ENGAGEMENT")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|s: &Spec| s.validate().is_ok());
    let Some(spec) = spec else {
        // Fail closed: without its engagement, nothing runs.
        eprintln!("indra guard: no valid engagement (INDRA_ENGAGEMENT); refusing");
        return std::process::ExitCode::from(2);
    };
    let dir = local_dir(&spec.id);
    if phase == "post" {
        record_result(&spec, &dir, &hook);
        crate::deck::steer::hook_main(&hook);
        return std::process::ExitCode::SUCCESS;
    }
    // A deny rule refuses before anything else (and is logged as the rule's).
    if let Some(v) = rule_answer(&rules, &hook, &event) {
        let _ = audit(
            &dir,
            json!({ "event": "tool", "engagement": spec.id, "session": hook["session_id"],
                    "tool": hook["tool_name"], "input": clip(&hook["tool_input"].to_string(), 2000),
                    "targets": [], "decision": "deny", "decided_by": "rule", "reason": v.to_string() }),
        );
        say(v);
        return std::process::ExitCode::SUCCESS;
    }
    match decide(&spec, &dir, &hook) {
        // Silence: the usual permission rules still apply.
        Decision::Allow { .. } => std::process::ExitCode::SUCCESS,
        Decision::Deny(reason) => {
            say(crate::deck::hooks::deny(&event, &reason));
            std::process::ExitCode::SUCCESS
        }
    }
}

/// What the approval rules say before a tool runs, as the hook's answer:
/// a deny rule refuses; for Gemini CLI, a request its policy would allow by
/// text that indra's stricter reading does not is put to a person.
fn rule_answer(rules: &crate::deck::approve::Rules, hook: &Value, event: &str) -> Option<Value> {
    use crate::deck::approve::Verdict;
    if rules.is_empty() {
        return None;
    }
    let tool = canonical_tool(hook["tool_name"].as_str().unwrap_or(""));
    let input = &hook["tool_input"];
    match rules.decide(tool, input) {
        Verdict::Deny(r) => Some(crate::deck::hooks::deny(
            event,
            &format!("Refused by the approval rule `{}` set in the indra console. Do not try to get around it; ask the operator if it is needed.", r),
        )),
        Verdict::Ask if tool == "Bash" => {
            let cmd = input["command"].as_str().unwrap_or("").trim_start();
            let by_text = rules.allow.iter().any(|r| cmd.starts_with(&r.join(" ")));
            if by_text {
                crate::deck::hooks::ask(event, "Not a single plain command that an approval rule allows: a person decides.")
            } else {
                None
            }
        }
        _ => None,
    }
}

// ---- the hub's store ----------------------------------------------------------------

pub struct Store {
    path: PathBuf,
    lock: Mutex<()>,
}

fn random_id() -> Result<String, String> {
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).map_err(|e| format!("no OS randomness: {}", e))?;
    Ok(b.iter()
        .map(|x| A[(*x as usize) % A.len()] as char)
        .collect())
}

/// The fields a new engagement is made from.
#[derive(Deserialize, Debug, Default)]
pub struct New {
    pub name: String,
    pub machine: String,
    pub scope: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    pub starts: u64,
    pub ends: u64,
    #[serde(default)]
    pub rate: Option<f64>,
    pub authorization: String,
    #[serde(default)]
    pub budget_usd: Option<f64>,
    #[serde(default)]
    pub record: bool,
}

impl Store {
    pub fn open(data_dir: &Path) -> Store {
        Store {
            path: data_dir.join("engagements.json"),
            lock: Mutex::new(()),
        }
    }

    fn load(&self) -> Vec<Engagement> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, all: &[Engagement]) -> Result<(), String> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let text = serde_json::to_string_pretty(all).map_err(|e| e.to_string())?;
        o.open(&tmp)
            .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|_| f.sync_all()))
            .and_then(|_| std::fs::rename(&tmp, &self.path))
            .map_err(|e| format!("{}: {}", self.path.display(), e))
    }

    pub fn list(&self) -> Vec<Engagement> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut v = self.load();
        v.reverse();
        v
    }

    pub fn get(&self, id: &str) -> Option<Engagement> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.load().into_iter().find(|e| e.spec.id == id)
    }

    pub fn create(&self, n: New, by: &str) -> Result<Engagement, String> {
        let auth = n.authorization.trim();
        if auth.chars().count() < 10 || auth.chars().count() > 2000 {
            return Err("say who authorized this test and the reference (a contract, ticket or email), 10 to 2000 characters".into());
        }
        crate::deck::nodes::validate_name(&n.machine).map_err(|e| format!("machine: {}", e))?;
        let clean = |v: Vec<String>| -> Vec<String> {
            v.into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && !s.starts_with('#'))
                .collect()
        };
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let e = Engagement {
            spec: Spec {
                id: random_id()?,
                name: n.name.trim().to_string(),
                scope: clean(n.scope),
                exclude: clean(n.exclude),
                starts: n.starts,
                ends: n.ends,
                rate: n.rate.unwrap_or(2.0),
                record: n.record,
            },
            machine: n.machine,
            authorization: auth.to_string(),
            created_by: by.to_string(),
            created: now(),
            budget_usd: n.budget_usd,
        };
        e.spec.validate()?;
        if let Some(b) = e.budget_usd {
            if !b.is_finite() || b <= 0.0 || b > 100_000.0 {
                return Err("the budget is a number of US dollars above 0 (at most 100000)".into());
            }
        }
        let mut all = self.load();
        all.push(e.clone());
        self.save(&all)?;
        Ok(e)
    }

    /// End it now: nothing in it may reach a host from here on.
    pub fn end(&self, id: &str) -> Result<Engagement, String> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        let e = all
            .iter_mut()
            .find(|e| e.spec.id == id)
            .ok_or("no such engagement")?;
        let t = now();
        if e.spec.ends > t {
            e.spec.ends = t.max(e.spec.starts + 1);
        }
        let out = e.clone();
        self.save(&all)?;
        Ok(out)
    }
}

/// An engagement as the console shows it.
pub fn view(e: &Engagement, t: u64) -> Value {
    let state = if t < e.spec.starts {
        "scheduled"
    } else if e.spec.active(t) {
        "active"
    } else {
        "ended"
    };
    json!({
        "id": e.spec.id, "name": e.spec.name, "scope": e.spec.scope, "exclude": e.spec.exclude,
        "starts": e.spec.starts, "ends": e.spec.ends, "rate": e.spec.rate, "machine": e.machine,
        "authorization": e.authorization, "created_by": e.created_by, "created": e.created, "state": state,
        "budget_usd": e.budget_usd, "record": e.spec.record,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            id: "abcd1234".into(),
            name: "lab".into(),
            scope: vec![
                "app.example.com".into(),
                "*.lab.example.com".into(),
                "10.0.0.0/24".into(),
                "https://api.example.org/v1".into(),
            ],
            exclude: vec!["db.lab.example.com".into(), "10.0.0.5".into()],
            starts: now() - 60,
            ends: now() + 3600,
            rate: 50.0,
            record: false,
        }
    }

    fn hosts(tool: &str, input: Value) -> Vec<String> {
        targets(tool, &input, None)
            .iter()
            .map(Target::show)
            .collect()
    }

    #[test]
    fn scope_rules() {
        let s = spec();
        s.validate().unwrap();
        let t = |h: &str| Target::Host(h.into());
        let ip = |a: &str| {
            let (a, p) = parse_net(a).unwrap();
            Target::Net(a, p)
        };
        assert!(s.allows(&t("app.example.com")));
        assert!(!s.allows(&t("www.example.com")));
        assert!(s.allows(&t("x.lab.example.com")));
        assert!(!s.allows(&t("lab.example.com")), "*.x is subdomains only");
        assert!(!s.allows(&t("db.lab.example.com")), "excluded");
        assert!(s.allows(&t("api.example.org")), "a URL entry is its host");
        assert!(s.allows(&ip("10.0.0.9")));
        assert!(!s.allows(&ip("10.0.0.5")), "excluded address");
        assert!(
            !s.allows(&ip("10.0.0.0/24")),
            "a range that holds an exclusion"
        );
        assert!(s.allows(&ip("10.0.0.128/25")));
        assert!(!s.allows(&ip("10.0.0.0/16")), "wider than the scope");
        assert!(!s.allows(&ip("10.0.1.1")));
        assert!(
            s.allows(&t("localhost")) && s.allows(&ip("127.0.0.1")),
            "this machine"
        );
        let mut bad = spec();
        bad.scope = vec!["not a host!".into()];
        assert!(bad.validate().is_err());
        bad.scope = vec![];
        assert!(bad.validate().is_err());
        let mut late = spec();
        late.ends = late.starts;
        assert!(late.validate().is_err());
    }

    #[test]
    fn targets_from_tool_calls() {
        assert_eq!(
            hosts(
                "Bash",
                json!({ "command": "curl -s https://user:pw@App.Example.com:8443/x?y=1 && nmap -p 80 10.0.0.0/28" })
            ),
            ["app.example.com", "10.0.0.0/28"]
        );
        assert_eq!(
            hosts(
                "Bash",
                json!({ "command": "dig www.example.com; ping 10.0.0.7" })
            ),
            ["10.0.0.7", "www.example.com"]
        );
        assert_eq!(
            hosts("WebFetch", json!({ "url": "http://[::1]:8080/" })),
            ["::1"]
        );
        // File names, paths, versions and local tools are not hosts.
        assert!(hosts("Bash", json!({ "command": "cat src/main.rs notes.md README.md && python3 x.py && pip install foo==1.2.3" })).is_empty());
        assert!(hosts("Bash", json!({ "command": "ls docs/report.com" })).is_empty());
        assert!(hosts(
            "Read",
            json!({ "file_path": "/etc/hosts", "note": "app.example.com" })
        )
        .is_empty());
        // MCP tools: every string.
        assert_eq!(
            hosts(
                "mcp__scan__probe",
                json!({ "args": { "target": "db.lab.example.com" } })
            ),
            ["db.lab.example.com"]
        );
    }

    #[test]
    fn the_guard_decides_logs_and_chains() {
        let dir = std::env::temp_dir().join(format!("indra-engage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = spec();
        let call = |cmd: &str| json!({ "session_id": "s1", "tool_name": "Bash", "tool_input": { "command": cmd }, "cwd": "/" });
        assert!(matches!(
            decide(&s, &dir, &call("curl https://app.example.com/")),
            Decision::Allow { .. }
        ));
        let d = decide(&s, &dir, &call("curl https://evil.example.net/"));
        assert!(
            matches!(&d, Decision::Deny(r) if r.contains("evil.example.net is outside the scope")),
            "{:?}",
            d
        );
        assert!(matches!(
            decide(&s, &dir, &call("ls -la")),
            Decision::Allow { waited_ms: 0 }
        ));
        // Outside the window, nothing that names a host runs.
        let mut over = s.clone();
        over.ends = now() - 1;
        over.starts = over.ends - 10;
        assert!(
            matches!(decide(&over, &dir, &call("curl https://app.example.com/")), Decision::Deny(r) if r.contains("not active"))
        );
        record_result(
            &s,
            &dir,
            &json!({ "tool_name": "Bash", "tool_response": { "stdout": "ok" } }),
        );
        let (log, intact) = read_audit(&dir, 100);
        assert!(intact);
        assert_eq!(log.len(), 5);
        assert_eq!(log[1]["decision"], "deny");
        assert_eq!(log[1]["targets"], json!(["evil.example.net"]));
        assert_eq!(log[4]["event"], "result");
        // Editing an entry breaks the chain.
        let p = dir.join("audit.jsonl");
        let text = std::fs::read_to_string(&p)
            .unwrap()
            .replace("\"deny\"", "\"allow\"");
        std::fs::write(&p, text).unwrap();
        assert!(!read_audit(&dir, 100).1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn alerts_that_mention_a_step() {
        assert!(mentions(
            "app.example.com",
            "GET https://APP.example.com/login from 10.9.9.9"
        ));
        assert!(!mentions("app.example.com", "www.example.com"));
        assert!(mentions("10.20.0.0/24", "scan from 10.20.0.77 to 8.8.8.8"));
        assert!(!mentions("10.20.0.0/24", "10.21.0.77"));
        assert!(mentions("10.20.0.7", "deny tcp 10.20.0.7:443"));
        assert!(!mentions("10.20.0.7", "10.20.0.70"));
    }

    #[test]
    fn calls_to_one_host_are_paced() {
        let dir = std::env::temp_dir().join(format!("indra-pace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let h = vec!["app.example.com".to_string()];
        assert_eq!(pace(&dir, &h, 1.0).unwrap(), 0);
        let w = pace(&dir, &h, 1.0).unwrap();
        assert!((900..=1000).contains(&w), "{}", w);
        assert!(pace(&dir, &h, 1.0).unwrap() >= 1900);
        assert_eq!(
            pace(&dir, &["other.example.com".to_string()], 1.0).unwrap(),
            0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn throttling_is_seen_and_backed_off() {
        assert_eq!(
            throttled("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 30"),
            Some(Some(30))
        );
        assert_eq!(throttled("< HTTP/2 503 \n< server: x"), Some(None));
        assert_eq!(throttled("\"status\": 429"), Some(None));
        assert_eq!(throttled("HTTP/1.1 200 OK, 4290 bytes"), None);
        assert_eq!(throttled("found 503 files"), None);
        let dir = std::env::temp_dir().join(format!("indra-backoff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = spec();
        let hook = |out: &str| {
            json!({ "session_id": "s1", "tool_name": "Bash",
            "tool_input": { "command": "curl -i https://app.example.com/api" },
            "tool_response": { "stdout": out, "stderr": "" } })
        };
        record_result(&s, &dir, &hook("HTTP/1.1 200 OK"));
        assert_eq!(pace(&dir, &["app.example.com".into()], 50.0).unwrap(), 0);
        record_result(
            &s,
            &dir,
            &hook("HTTP/1.1 429 Too Many Requests\nRetry-After: 60"),
        );
        // Booked a minute out: the guard refuses rather than sleeping.
        let call = json!({ "session_id": "s1", "tool_name": "Bash", "tool_input": { "command": "curl https://app.example.com/" }, "cwd": "/" });
        assert!(matches!(decide(&s, &dir, &call), Decision::Deny(r) if r.contains("Rate limit")));
        let (log, intact) = read_audit(&dir, 100);
        assert!(intact);
        let b = log.iter().find(|x| x["event"] == "backoff").unwrap();
        assert_eq!(b["targets"], json!(["app.example.com"]));
        assert!(b["waited_ms"].as_u64().unwrap() >= 59_000);
        // Without a Retry-After: 2 s, then 4 s.
        let other = |out: &str| json!({ "tool_name": "WebFetch", "tool_input": { "url": "https://x.lab.example.com/" }, "tool_response": out });
        record_result(&s, &dir, &other("Request failed with status code 503"));
        record_result(&s, &dir, &other("Request failed with status code 503"));
        let w = pace(&dir, &["x.lab.example.com".into()], 50.0).unwrap();
        assert!((3_000..=4_100).contains(&w), "{}", w);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gemini_and_codex_calls_are_read_and_answered_in_their_format() {
        // Gemini CLI's shell and web tools; Codex's shell (one string or argv).
        assert_eq!(
            hosts(
                "run_shell_command",
                json!({ "command": "curl https://app.example.com/x" })
            ),
            ["app.example.com"]
        );
        assert_eq!(
            hosts(
                "web_fetch",
                json!({ "prompt": "Summarise https://evil.example.net/a and https://app.example.com/b" })
            ),
            ["evil.example.net", "app.example.com"]
        );
        assert_eq!(
            hosts(
                "Bash",
                json!({ "command": ["bash", "-lc", "nmap 10.0.0.7"] })
            ),
            ["10.0.0.7"]
        );
        assert_eq!(
            hosts("exec_command", json!({ "cmd": "dig db.lab.example.com" })),
            ["db.lab.example.com"]
        );
        assert!(hosts(
            "read_file",
            json!({ "file_path": "notes/app.example.com.txt" })
        )
        .is_empty());
        assert!(hosts(
            "apply_patch",
            json!({ "command": "*** Update File: a.example.com" })
        )
        .is_empty());
        // The guard's answer in each CLI's own shape.
        let dir = std::env::temp_dir().join(format!("indra-gemguard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = spec();
        let call = json!({ "hook_event_name": "BeforeTool", "tool_name": "run_shell_command",
                           "tool_input": { "command": "curl https://evil.example.net/" }, "cwd": "/" });
        let Decision::Deny(r) = decide(&s, &dir, &call) else {
            panic!("refused")
        };
        assert_eq!(
            crate::deck::hooks::deny("BeforeTool", &r)["decision"],
            "deny"
        );
        // Rules: a deny rule refuses; Gemini asks a person about what its
        // policy would allow by text but indra would not.
        let rules = crate::deck::approve::Rules {
            allow: vec![vec!["git".into(), "status".into()]],
            deny: vec![vec!["git".into(), "push".into()]],
        };
        let ask = |event: &str, cmd: &str| {
            rule_answer(
                &rules,
                &json!({ "tool_name": "run_shell_command", "tool_input": { "command": cmd } }),
                event,
            )
        };
        assert_eq!(
            ask("BeforeTool", "git push origin").unwrap()["decision"],
            "deny"
        );
        assert_eq!(
            ask("BeforeTool", "git status; rm -rf x").unwrap()["decision"],
            "ask"
        );
        assert!(
            ask("BeforeTool", "git status").is_none(),
            "Gemini's own policy allows it"
        );
        assert!(
            ask("PreToolUse", "git status; rm -rf x").is_none(),
            "Claude and Codex ask by themselves"
        );
        let codex = rule_answer(
            &rules,
            &json!({ "tool_name": "Bash", "tool_input": { "command": "sudo git push" } }),
            "PreToolUse",
        )
        .unwrap();
        assert_eq!(codex["hookSpecificOutput"]["permissionDecision"], "deny");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coverage_follows_the_scope() {
        let s = spec();
        let e = |ev: &str, dec: &str, t: &[&str]| json!({ "event": ev, "decision": dec, "targets": t, "ts": 5 });
        let log = vec![
            e("tool", "allow", &["app.example.com"]),
            e("tool", "allow", &["app.example.com", "10.0.0.9"]),
            e("tool", "allow", &["a.lab.example.com"]),
            e("tool", "deny", &["evil.example.net"]),
            e("traffic", "allow", &["b.lab.example.com"]),
            e("tool", "allow", &["localhost"]),
            json!({ "event": "result", "ok": true }),
        ];
        let c = coverage(&s, &log);
        assert_eq!(c["summary"]["entries"], 4);
        assert_eq!(c["summary"]["tested"], 3);
        assert_eq!(c["summary"]["untested"], 1);
        assert_eq!(c["summary"]["refused"], 1);
        let sc = c["scope"].as_array().unwrap();
        assert_eq!(sc[0]["hosts"][0]["calls"], 2);
        assert_eq!(
            sc[1]["hosts"].as_array().unwrap().len(),
            2,
            "*.lab: a and b"
        );
        assert_eq!(sc[3]["entry"], "https://api.example.org/v1");
        assert_eq!(sc[3]["tested"], false);
        assert_eq!(c["refused"][0]["host"], "evil.example.net");
    }

    #[test]
    fn the_store_keeps_engagements() {
        let dir = std::env::temp_dir().join(format!("indra-engstore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = Store::open(&dir);
        let n = || New {
            name: "Q4 web test".into(),
            machine: "attack-box".into(),
            scope: vec!["app.example.com".into(), " ".into(), "# a comment".into()],
            starts: now(),
            ends: now() + 3600,
            authorization: "Signed SOW #42 by J. Doe".into(),
            ..Default::default()
        };
        let e = st.create(n(), "admin").unwrap();
        assert_eq!(e.spec.scope, ["app.example.com"]);
        assert_eq!(e.spec.rate, 2.0);
        assert!(st
            .create(
                New {
                    authorization: "ok".into(),
                    ..n()
                },
                "admin"
            )
            .is_err());
        assert!(st
            .create(
                New {
                    scope: vec![],
                    ..n()
                },
                "admin"
            )
            .is_err());
        assert_eq!(view(&st.get(&e.spec.id).unwrap(), now())["state"], "active");
        let ended = st.end(&e.spec.id).unwrap();
        assert!(!ended.spec.active(now() + 1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
