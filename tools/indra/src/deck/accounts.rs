//! Accounts: the credentials agents run with, their usage, and routing.
//!
//! By default nothing changes: an agent uses the login its CLI has on the
//! machine it runs on (`claude` logged in, `codex login`, ...), and indra
//! only counts what each of those logins spends.
//!
//! Add `[[account]]`s to give agents credentials from here instead, so a
//! node needs no login of its own (the key is a secret, resolved on the hub
//! and handed to the agent only):
//!
//! ```toml
//! [[account]]
//! name       = "work"
//! agent      = "claude"                  # claude (default) | codex | gemini | a runner name
//! credential = "secret:anthropic-work"   # an API key, or a Claude subscription token
//! kind       = "oauth_token"             # api_key (default) | oauth_token (`claude setup-token`)
//! daily_usd  = 20                        # optional limits
//!
//! [routing]
//! strategy = "most_remaining"            # most_remaining (default) | round_robin | in_order
//! ```
//!
//! Each launch picks an account for its agent and node: one a profile pins
//! (`account = "work"`, or `"login"` for the machine's own), else the one
//! with the most left today. An account that hits a usage limit (the agent
//! fails saying so) rests until the limit resets; when every account is
//! used up, the machine's own login takes over (`fallback_to_login`).

use crate::deck::config::Shared;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

/// One `[[account]]`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AccountCfg {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The agent it is for: "claude" (default), "codex", "gemini", or a
    /// runner's name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    /// The credential, as a secret reference (`secret:<name>`).
    pub credential: String,
    /// "api_key" (default) or "oauth_token" (a Claude subscription token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The environment variable it goes in (default by agent and kind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Spend at most this much a day / a month (US dollars, as agents report).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_usd: Option<f64>,
    /// At most this many runs a day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_runs: Option<u32>,
    /// Nodes it may be used on (default: all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<String>,
    /// For "in_order" routing: lower goes first (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

/// `[routing]`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingCfg {
    /// most_remaining (default): the account with the most left of its
    /// tightest limit (least spent today when it has none); round_robin;
    /// in_order (by priority, then as written).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// When every account is used up or resting, use the machine's own
    /// login (default true); false: the launch fails instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_to_login: Option<bool>,
    /// How long an account rests after a usage limit when the agent does not
    /// say until when (minutes, default 60).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_minutes: Option<u64>,
}

pub const STRATEGIES: [&str; 3] = ["most_remaining", "round_robin", "in_order"];
pub const KINDS: [&str; 2] = ["api_key", "oauth_token"];

/// The agent kind a runner is: claude, codex, gemini, or its own name.
pub fn agent_of(runner: &str, command: &str, format: &str) -> String {
    let c = std::path::Path::new(command)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if format == "claude-stream-json" || c == "claude" || runner == "claude" {
        return "claude".into();
    }
    for k in ["codex", "gemini"] {
        if c == k || runner == k {
            return k.into();
        }
    }
    runner.to_string()
}

impl AccountCfg {
    pub fn agent(&self) -> &str {
        if self.agent.is_empty() {
            "claude"
        } else {
            &self.agent
        }
    }

    /// The environment variable the credential goes in.
    pub fn env_var(&self) -> Option<String> {
        if let Some(e) = self.env.as_ref().filter(|e| !e.is_empty()) {
            return Some(e.clone());
        }
        Some(
            match (self.agent(), self.kind.as_deref().unwrap_or("api_key")) {
                ("claude", "oauth_token") => "CLAUDE_CODE_OAUTH_TOKEN",
                ("claude", _) => "ANTHROPIC_API_KEY",
                ("codex", _) => "OPENAI_API_KEY",
                ("gemini", _) => "GEMINI_API_KEY",
                _ => return None,
            }
            .into(),
        )
    }

    pub fn validate(&self) -> Result<(), String> {
        let at = |m: String| format!("account '{}': {}", self.name, m);
        crate::deck::nodes::validate_name(&self.name).map_err(at)?;
        if self.name == "login" {
            return Err(at(
                "'login' means the machine's own login; pick another name".into(),
            ));
        }
        match crate::deck::secrets::reference(&self.credential) {
            Some(n) => crate::deck::secrets::validate_name(n).map_err(at)?,
            None => {
                return Err(at(
                    "credential must be a secret reference, \"secret:<name>\" (add it in Settings > Secrets), never the key itself".into(),
                ))
            }
        }
        if let Some(k) = &self.kind {
            if !KINDS.contains(&k.as_str()) {
                return Err(at(format!("kind must be {}", KINDS.join(" or "))));
            }
            if k == "oauth_token" && self.agent() != "claude" {
                return Err(at(
                    "oauth_token is for Claude Code (claude setup-token)".into()
                ));
            }
        }
        if self.env_var().is_none() {
            return Err(at(format!(
                "say which environment variable the credential goes in (env = \"...\") for agent '{}'",
                self.agent()
            )));
        }
        if let Some(e) = &self.env {
            if e.is_empty()
                || !e.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                || e.starts_with(|c: char| c.is_ascii_digit())
            {
                return Err(at(format!("invalid environment variable name {:?}", e)));
            }
        }
        for v in [self.daily_usd, self.monthly_usd].into_iter().flatten() {
            if !(v > 0.0 && v.is_finite()) {
                return Err(at("limits must be positive".into()));
            }
        }
        Ok(())
    }
}

pub fn validate_routing(r: &RoutingCfg) -> Result<(), String> {
    if let Some(s) = &r.strategy {
        if !STRATEGIES.contains(&s.as_str()) {
            return Err(format!(
                "routing: strategy must be one of {}",
                STRATEGIES.join(", ")
            ));
        }
    }
    if r.cooldown_minutes == Some(0) {
        return Err("routing: cooldown_minutes must be at least 1".into());
    }
    Ok(())
}

/// What one account (or one machine's login) has used, per day.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Usage {
    /// "YYYY-MM-DD" -> (runs, usd).
    #[serde(default)]
    days: BTreeMap<String, (u32, f64)>,
    /// Resting after a usage limit until this time (unix seconds).
    #[serde(default)]
    cooldown_until: u64,
    #[serde(default)]
    last_limit: String,
    #[serde(default)]
    last_used: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    usage: BTreeMap<String, Usage>,
    /// Processes whose end was already counted.
    #[serde(default)]
    counted: Vec<u64>,
    #[serde(default)]
    turn: u64,
}

pub struct Accounts {
    settings: Shared,
    path: PathBuf,
    store: Mutex<Store>,
    /// Process id -> account key, for processes started by this console.
    procs: Mutex<BTreeMap<u64, String>>,
}

/// The account a launch got.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    /// The account's name, or `login:<agent>@<node>` for the machine's own.
    pub key: String,
    /// The variable and secret reference to add to the agent's environment.
    pub env: Option<(String, String)>,
    /// Why this one, for its page.
    pub why: String,
}

fn day(t: u64) -> String {
    crate::mem::time::format_unix(t)[..10].to_string()
}

fn login_key(agent: &str, node: &str) -> String {
    format!("login:{}@{}", agent, node)
}

impl Usage {
    fn today(&self, now: u64) -> (u32, f64) {
        self.days.get(&day(now)).copied().unwrap_or((0, 0.0))
    }
    fn month_usd(&self, now: u64) -> f64 {
        let m = &day(now)[..7];
        self.days
            .iter()
            .filter(|(d, _)| d.starts_with(m))
            .map(|(_, (_, u))| u)
            .sum::<f64>()
            // An empty sum is -0.0.
            + 0.0
    }
}

impl Accounts {
    pub fn new(settings: Shared, data_dir: PathBuf) -> Accounts {
        let path = data_dir.join("accounts.json");
        let store = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Accounts {
            settings,
            path,
            store: Mutex::new(store),
            procs: Mutex::new(BTreeMap::new()),
        }
    }

    fn save(&self, st: &Store) {
        if let Some(d) = self.path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(text) = serde_json::to_string_pretty(st) {
            let _ = crate::deck::confedit::write_atomic(&self.path, &text);
        }
    }

    /// Why an account cannot take a run now, if it cannot.
    fn blocked(a: &AccountCfg, u: &Usage, now: u64) -> Option<String> {
        if a.disabled {
            return Some("turned off".into());
        }
        if u.cooldown_until > now {
            return Some(format!(
                "resting after a usage limit until {}",
                crate::mem::time::format_unix(u.cooldown_until)
            ));
        }
        let (runs, usd) = u.today(now);
        if a.daily_runs.is_some_and(|l| runs >= l) {
            return Some(format!("{} runs today, its daily limit", runs));
        }
        if a.daily_usd.is_some_and(|l| usd >= l) {
            return Some(format!("${:.2} spent today, its daily limit", usd));
        }
        if a.monthly_usd.is_some_and(|l| u.month_usd(now) >= l) {
            return Some("its monthly limit is spent".into());
        }
        None
    }

    /// The share of its tightest limit an account has left (1 when none).
    fn left(a: &AccountCfg, u: &Usage, now: u64) -> f64 {
        let (runs, usd) = u.today(now);
        let mut f: f64 = 1.0;
        if let Some(l) = a.daily_usd {
            f = f.min(1.0 - usd / l);
        }
        if let Some(l) = a.monthly_usd {
            f = f.min(1.0 - u.month_usd(now) / l);
        }
        if let Some(l) = a.daily_runs {
            f = f.min(1.0 - runs as f64 / l as f64);
        }
        f.max(0.0)
    }

    /// Choose the account for an agent on a node. `pinned`: a profile's
    /// `account` ("login" for the machine's own).
    pub fn pick(
        &self,
        agent: &str,
        node: &str,
        pinned: Option<&str>,
        now: u64,
    ) -> Result<Pick, String> {
        let s = self.settings.get();
        let login = |why: String| Pick {
            key: login_key(agent, node),
            env: None,
            why,
        };
        if pinned == Some("login") {
            return Ok(login("the profile uses the machine's own login".into()));
        }
        let mut st = self.store.lock().unwrap();
        let candidates: Vec<&AccountCfg> = s
            .accounts
            .iter()
            .filter(|a| a.agent() == agent)
            .filter(|a| a.nodes.is_empty() || a.nodes.iter().any(|n| n == node))
            .filter(|a| pinned.is_none_or(|p| a.name == p))
            .collect();
        if let Some(p) = pinned {
            let a = candidates.first().ok_or_else(|| {
                format!(
                    "the profile's account '{}' is not a {} account usable on {}",
                    p, agent, node
                )
            })?;
            let u = st.usage.get(&a.name).cloned().unwrap_or_default();
            if let Some(why) = Self::blocked(a, &u, now) {
                return Err(format!("account '{}' cannot run now: {}", a.name, why));
            }
            return Ok(self.take(
                &mut st,
                a,
                now,
                format!("the profile pins account '{}'", a.name),
            ));
        }
        if candidates.is_empty() {
            return Ok(login(format!(
                "no {} account is configured: the machine's own login",
                agent
            )));
        }
        let free: Vec<&AccountCfg> = candidates
            .iter()
            .copied()
            .filter(|a| {
                Self::blocked(a, &st.usage.get(&a.name).cloned().unwrap_or_default(), now).is_none()
            })
            .collect();
        if free.is_empty() {
            let why = candidates
                .iter()
                .map(|a| {
                    format!(
                        "{}: {}",
                        a.name,
                        Self::blocked(a, &st.usage.get(&a.name).cloned().unwrap_or_default(), now)
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            if s.routing.fallback_to_login.unwrap_or(true) {
                return Ok(login(format!(
                    "every {} account is used up ({}): the machine's own login",
                    agent, why
                )));
            }
            return Err(format!("every {} account is used up: {}", agent, why));
        }
        let strategy = s.routing.strategy.as_deref().unwrap_or("most_remaining");
        let chosen = match strategy {
            "round_robin" => {
                st.turn = st.turn.wrapping_add(1);
                free[(st.turn as usize) % free.len()]
            }
            "in_order" => *free.iter().min_by_key(|a| a.priority.unwrap_or(0)).unwrap(),
            _ => {
                let usage = |a: &AccountCfg| st.usage.get(&a.name).cloned().unwrap_or_default();
                *free
                    .iter()
                    .max_by(|x, y| {
                        let (ux, uy) = (usage(x), usage(y));
                        Self::left(x, &ux, now)
                            .partial_cmp(&Self::left(y, &uy, now))
                            .unwrap_or(std::cmp::Ordering::Equal)
                            // Then the one used least today, then the older use.
                            .then(
                                uy.today(now)
                                    .1
                                    .partial_cmp(&ux.today(now).1)
                                    .unwrap_or(std::cmp::Ordering::Equal),
                            )
                            .then(uy.last_used.cmp(&ux.last_used))
                    })
                    .unwrap()
            }
        };
        let why = format!(
            "{} routing chose '{}'",
            strategy.replace('_', " "),
            chosen.name
        );
        Ok(self.take(&mut st, chosen, now, why))
    }

    fn take(&self, st: &mut Store, a: &AccountCfg, now: u64, why: String) -> Pick {
        let u = st.usage.entry(a.name.clone()).or_default();
        u.last_used = now;
        self.save(st);
        Pick {
            key: a.name.clone(),
            env: a.env_var().map(|v| (v, a.credential.clone())),
            why,
        }
    }

    /// The account a profile pins, if it does.
    pub fn pin_of(&self, profile: &str) -> Option<String> {
        self.settings
            .get()
            .profile(profile)
            .and_then(|p| p.account.clone())
    }

    /// A process started with this pick.
    pub fn started(&self, proc_id: u64, key: &str, now: u64) {
        self.procs.lock().unwrap().insert(proc_id, key.to_string());
        let mut st = self.store.lock().unwrap();
        let u = st.usage.entry(key.to_string()).or_default();
        u.days.entry(day(now)).or_insert((0, 0.0)).0 += 1;
        self.save(&st);
    }

    /// Count what finished processes spent, and rest accounts that hit a
    /// usage limit. Safe to call often.
    pub fn sync(&self, procs: &crate::deck::procs::Procs, now: u64) {
        let started: Vec<(u64, String)> = self
            .procs
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        if started.is_empty() {
            return;
        }
        let counted: HashSet<u64> = self.store.lock().unwrap().counted.iter().copied().collect();
        let mut done = Vec::new();
        for (id, key) in started {
            if counted.contains(&id) {
                continue;
            }
            let Some(p) = procs.get(id) else { continue };
            if p.is_running() {
                continue;
            }
            let v = p.view();
            let failed = v.status == "failed";
            let text = if failed {
                format!("{}\n{}", p.outcome(), p.text_output(4000))
            } else {
                String::new()
            };
            self.finished(&key, v.started, v.cost_usd, failed, &text, now);
            done.push(id);
        }
        if !done.is_empty() {
            let mut st = self.store.lock().unwrap();
            st.counted.extend(done);
            let n = st.counted.len();
            if n > 5000 {
                st.counted.drain(..n - 5000);
            }
            self.save(&st);
            let mut p = self.procs.lock().unwrap();
            for id in st.counted.iter() {
                p.remove(id);
            }
        }
    }

    /// Count a finished run's cost, and rest the account when its output
    /// says it hit a usage limit (only for a run that failed).
    pub fn finished(
        &self,
        key: &str,
        started: u64,
        cost: Option<f64>,
        failed: bool,
        text: &str,
        now: u64,
    ) {
        let cooldown = self.settings.get().routing.cooldown_minutes.unwrap_or(60) * 60;
        let mut st = self.store.lock().unwrap();
        let u = st.usage.entry(key.to_string()).or_default();
        if let Some(c) = cost {
            u.days.entry(day(started)).or_insert((0, 0.0)).1 += c;
        }
        if failed {
            if let Some(until) = limit_hit(text, now, cooldown) {
                u.cooldown_until = until;
                u.last_limit = text
                    .lines()
                    .find(|l| is_limit_line(l))
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect();
            }
        }
        self.save(&st);
    }

    /// Let a resting account take runs again.
    pub fn wake(&self, key: &str) -> bool {
        let mut st = self.store.lock().unwrap();
        match st.usage.get_mut(key) {
            Some(u) => {
                u.cooldown_until = 0;
                self.save(&st);
                true
            }
            None => false,
        }
    }

    /// Every account and every machine login used so far, with today's and
    /// this month's use against the limits.
    pub fn view(&self, now: u64) -> Value {
        let s = self.settings.get();
        let st = self.store.lock().unwrap();
        let row = |key: &str, a: Option<&AccountCfg>| {
            let u = st.usage.get(key).cloned().unwrap_or_default();
            let (runs, usd) = u.today(now);
            let blocked = a.and_then(|a| Self::blocked(a, &u, now)).or_else(|| {
                (u.cooldown_until > now).then(|| {
                    format!(
                        "hit a usage limit; resting until {}",
                        crate::mem::time::format_unix(u.cooldown_until)
                    )
                })
            });
            json!({
                "key": key,
                "name": a.map(|a| a.name.clone()).unwrap_or_else(|| key.to_string()),
                "login": a.is_none(),
                "agent": a.map(|a| a.agent().to_string()).unwrap_or_else(|| key.trim_start_matches("login:").split('@').next().unwrap_or("").to_string()),
                "description": a.map(|a| a.description.clone()).unwrap_or_default(),
                "credential": a.and_then(|a| crate::deck::secrets::reference(&a.credential).map(String::from)),
                "env": a.and_then(|a| a.env_var()),
                "nodes": a.map(|a| a.nodes.clone()).unwrap_or_default(),
                "today": { "runs": runs, "usd": usd },
                "month_usd": u.month_usd(now),
                "limits": { "daily_usd": a.and_then(|a| a.daily_usd), "monthly_usd": a.and_then(|a| a.monthly_usd), "daily_runs": a.and_then(|a| a.daily_runs) },
                "left": a.map(|a| Self::left(a, &u, now)),
                "status": if a.is_some_and(|a| a.disabled) { "off" } else if blocked.is_some() { "used up" } else { "ready" },
                "why": blocked,
                "cooldown_until": (u.cooldown_until > now).then_some(u.cooldown_until),
                "last_limit": (!u.last_limit.is_empty()).then(|| u.last_limit.clone()),
                "last_used": (u.last_used > 0).then_some(u.last_used),
            })
        };
        let mut rows: Vec<Value> = s.accounts.iter().map(|a| row(&a.name, Some(a))).collect();
        for key in st.usage.keys().filter(|k| k.starts_with("login:")) {
            rows.push(row(key, None));
        }
        json!({
            "accounts": rows,
            "routing": {
                "strategy": s.routing.strategy.clone().unwrap_or_else(|| "most_remaining".into()),
                "fallback_to_login": s.routing.fallback_to_login.unwrap_or(true),
                "cooldown_minutes": s.routing.cooldown_minutes.unwrap_or(60),
            },
        })
    }
}

fn is_limit_line(l: &str) -> bool {
    let l = l.to_lowercase();
    [
        "usage limit",
        "rate limit",
        "rate_limit",
        "quota",
        "credit balance is too low",
        "insufficient_quota",
        "too many requests",
        "limit reached",
        "429",
    ]
    .iter()
    .any(|p| l.contains(p))
}

/// When a failed agent's output says it hit a usage limit: until when the
/// account should rest (the reset time it gives, else `cooldown` from now).
pub fn limit_hit(text: &str, now: u64, cooldown: u64) -> Option<u64> {
    if !text.lines().any(is_limit_line) {
        return None;
    }
    // Claude Code: "Claude AI usage limit reached|1731659400".
    if let Some(i) = text.find("limit reached|") {
        let digits: String = text[i + 14..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(t) = digits.parse::<u64>() {
            if t > now && t < now + 31 * 86400 {
                return Some(t);
            }
        }
    }
    // "try again in 25 minutes" / "in 2 hours".
    let low = text.to_lowercase();
    if let Some(i) = low.find("try again in ") {
        let rest = &low[i + 13..];
        let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = n.parse::<u64>() {
            let unit = rest[n.to_string().len()..].trim_start();
            let secs = if unit.starts_with("hour") || unit.starts_with('h') {
                n * 3600
            } else if unit.starts_with("sec") || unit.starts_with('s') {
                n
            } else {
                n * 60
            };
            return Some(now + secs.max(60));
        }
    }
    Some(now + cooldown)
}

/// Routing and accounts as loaded, checked.
pub fn check(accounts: &[AccountCfg], routing: &RoutingCfg) -> Result<(), String> {
    let mut seen = HashSet::new();
    for a in accounts {
        a.validate()?;
        if !seen.insert(a.name.clone()) {
            return Err(format!("account name '{}' is used twice", a.name));
        }
    }
    validate_routing(routing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(name: &str, f: impl Fn(&mut AccountCfg)) -> AccountCfg {
        let mut a = AccountCfg {
            name: name.into(),
            credential: format!("secret:{}", name),
            ..Default::default()
        };
        f(&mut a);
        a
    }

    #[test]
    fn checks_env_and_agents() {
        assert_eq!(agent_of("claude", "claude", "claude-stream-json"), "claude");
        assert_eq!(agent_of("fast", "/usr/bin/codex", "text"), "codex");
        assert_eq!(agent_of("aider", "aider", "text"), "aider");
        assert_eq!(
            acct("a", |_| {}).env_var().as_deref(),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(
            acct("a", |a| a.kind = Some("oauth_token".into()))
                .env_var()
                .as_deref(),
            Some("CLAUDE_CODE_OAUTH_TOKEN")
        );
        assert_eq!(
            acct("a", |a| a.agent = "codex".into()).env_var().as_deref(),
            Some("OPENAI_API_KEY")
        );
        assert!(
            acct("a", |a| a.agent = "aider".into()).validate().is_err(),
            "needs env"
        );
        assert!(acct("a", |a| {
            a.agent = "aider".into();
            a.env = Some("AIDER_KEY".into());
        })
        .validate()
        .is_ok());
        assert!(acct("a", |a| a.credential = "sk-ant-raw-key".into())
            .validate()
            .is_err());
        assert!(acct("login", |_| {}).validate().is_err());
        assert!(acct("a", |a| a.daily_usd = Some(-1.0)).validate().is_err());
        assert!(acct("a", |a| {
            a.agent = "codex".into();
            a.kind = Some("oauth_token".into());
        })
        .validate()
        .is_err());
        assert!(validate_routing(&RoutingCfg {
            strategy: Some("random".into()),
            ..Default::default()
        })
        .is_err());
    }

    fn accounts_with(toml_text: &str) -> (Accounts, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "indra-acct-{}-{}",
            std::process::id(),
            toml_text.len()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = crate::deck::config::LoadOptions {
            config: None,
            targets: vec![],
            bind: None,
            read_only: false,
            cwd: std::env::temp_dir(),
        };
        let f: crate::deck::config::ConfigFile = toml::from_str(toml_text).unwrap();
        let s = crate::deck::config::build_settings(&opts, None, &f).unwrap();
        (Accounts::new(Shared::new(s), dir.clone()), dir)
    }

    #[test]
    fn routing_limits_cooldowns_and_fallback() {
        let now = 1_800_000_000;
        let (acc, dir) = accounts_with(
            "[[account]]\nname = \"a\"\ncredential = \"secret:a\"\ndaily_usd = 10\n\
             [[account]]\nname = \"b\"\ncredential = \"secret:b\"\nkind = \"oauth_token\"\ndaily_runs = 2\n\
             [[account]]\nname = \"build-only\"\ncredential = \"secret:c\"\nnodes = [\"build-box\"]\n\
             [[account]]\nname = \"cx\"\nagent = \"codex\"\ncredential = \"secret:cx\"\n\
             [[profile]]\nname = \"pinned\"\naccount = \"b\"\n",
        );
        // Codex gets its account and its variable; an agent with none, the login.
        let p = acc.pick("codex", "local", None, now).unwrap();
        assert_eq!(
            (p.key.as_str(), p.env.clone()),
            ("cx", Some(("OPENAI_API_KEY".into(), "secret:cx".into())))
        );
        assert_eq!(
            acc.pick("gemini", "local", None, now).unwrap().key,
            "login:gemini@local"
        );
        // Only a or b here (build-only is for build-box).
        let first = acc.pick("claude", "local", None, now).unwrap();
        assert!(first.key == "a" || first.key == "b", "{:?}", first);
        assert_eq!(
            acc.pick("claude", "build-box", Some("build-only"), now)
                .unwrap()
                .key,
            "build-only"
        );
        // b's two runs are its limit; then a takes the runs.
        acc.started(1, "b", now);
        acc.started(2, "b", now);
        let p = acc.pick("claude", "local", None, now).unwrap();
        assert_eq!(p.key, "a");
        assert!(acc
            .pick("claude", "local", Some("b"), now)
            .unwrap_err()
            .contains("2 runs today"));
        assert_eq!(acc.pin_of("pinned").as_deref(), Some("b"));
        // a's money runs out: every claude account is used up, so the login.
        acc.finished("a", now, Some(10.5), false, "", now);
        let p = acc.pick("claude", "local", None, now).unwrap();
        assert_eq!(p.key, "login:claude@local");
        assert!(p.why.contains("used up"), "{}", p.why);
        // The next day both are back.
        assert!(["a", "b"].contains(
            &acc.pick("claude", "local", None, now + 86400)
                .unwrap()
                .key
                .as_str()
        ));
        // A usage limit rests an account until the reset it names; wake ends that.
        acc.finished(
            "cx",
            now,
            None,
            true,
            &format!("Error: usage limit reached|{}", now + 3600),
            now,
        );
        assert_eq!(
            acc.pick("codex", "local", None, now).unwrap().key,
            "login:codex@local"
        );
        assert_eq!(
            acc.pick("codex", "local", None, now + 3601).unwrap().key,
            "cx"
        );
        assert!(acc.wake("cx"));
        assert_eq!(acc.pick("codex", "local", None, now).unwrap().key, "cx");
        // A success that only mentions limits rests nothing.
        acc.finished("cx", now, None, false, "I added rate limit handling", now);
        assert_eq!(acc.pick("codex", "local", None, now).unwrap().key, "cx");
        // The view: every account and every login used, with what is left.
        let v = acc.view(now);
        let rows = v["accounts"].as_array().unwrap();
        let row = |k: &str| rows.iter().find(|r| r["key"] == k).cloned().unwrap();
        assert_eq!(row("a")["status"], "used up");
        assert_eq!(row("a")["left"], 0.0);
        assert_eq!(row("b")["today"]["runs"], 2);
        assert_eq!(row("b")["env"], "CLAUDE_CODE_OAUTH_TOKEN");
        assert!(
            !rows.iter().any(|r| r["login"] == true),
            "a login shows once it ran"
        );
        acc.started(3, "login:claude@local", now);
        let v = acc.view(now);
        let login = v["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["login"] == true)
            .cloned()
            .unwrap();
        assert_eq!(
            (login["agent"].as_str(), login["today"]["runs"].as_u64()),
            (Some("claude"), Some(1))
        );
        // It all survives a restart.
        let again = Accounts::new(acc.settings.clone(), dir.clone());
        assert_eq!(
            again.pick("claude", "local", None, now).unwrap().key,
            "login:claude@local"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strict_routing_fails_instead_of_falling_back() {
        let now = 1_800_000_000;
        let (acc, dir) = accounts_with(
            "[routing]\nfallback_to_login = false\nstrategy = \"in_order\"\n\
             [[account]]\nname = \"second\"\ncredential = \"secret:s\"\npriority = 2\n\
             [[account]]\nname = \"first\"\ncredential = \"secret:f\"\npriority = 1\ndaily_runs = 1\n",
        );
        assert_eq!(acc.pick("claude", "local", None, now).unwrap().key, "first");
        acc.started(1, "first", now);
        assert_eq!(
            acc.pick("claude", "local", None, now).unwrap().key,
            "second"
        );
        acc.finished("second", now, None, true, "429 Too Many Requests", now);
        let e = acc.pick("claude", "local", None, now).unwrap_err();
        assert!(e.contains("every claude account is used up"), "{}", e);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_limits_are_noticed() {
        let now = 1_800_000_000;
        assert_eq!(limit_hit("all good", now, 3600), None);
        assert_eq!(
            limit_hit(
                &format!("Claude AI usage limit reached|{}", now + 7200),
                now,
                3600
            ),
            Some(now + 7200)
        );
        assert_eq!(
            limit_hit(
                "Rate limit exceeded. Please try again in 15 minutes.",
                now,
                3600
            ),
            Some(now + 900)
        );
        assert_eq!(
            limit_hit(
                "You've hit your usage limit. Try again in 2 hours",
                now,
                3600
            ),
            Some(now + 7200)
        );
        assert_eq!(
            limit_hit("error: 429 Too Many Requests", now, 3600),
            Some(now + 3600)
        );
    }
}
