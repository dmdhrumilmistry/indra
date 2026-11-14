//! `indra review`: the deterministic parts of sh-pr-review.
//!
//! - `cache`: a local cache so a re-push does not re-analyse unchanged code. A
//!   cache is exact bookkeeping, and a model that hand-manages one will
//!   eventually report a hit it cannot justify, so every key is derived here
//!   and every miss states its reason. It fails closed: a stale entry in a
//!   security tool does not make it slow, it makes it *wrong*, by suppressing a
//!   finding the current knowledge base would catch. An entry is returned only
//!   when the knowledge bases, the skill version, the model and the file
//!   contents all still match what produced it.
//! - `metrics`: local-only run metrics (wall time, tokens, model, outcome).
//!   Nothing is ever transmitted; `metrics path` prints where they live and
//!   `metrics purge` deletes them.
//! - `post`: post the review to the PR and set the commit status, with one
//!   guarantee: it never exits leaving the status at `pending`.

use clap::{Args, Subcommand};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const SCHEMA: u64 = 1;
const DEFAULT_TTL_DAYS: i64 = 7;

#[derive(Args)]
pub struct ReviewArgs {
    #[command(subcommand)]
    cmd: ReviewCmd,
}

#[derive(Subcommand)]
enum ReviewCmd {
    /// The PR review cache (`indra review cache --help`)
    Cache(CacheArgs),
    /// Local-only run metrics (`indra review metrics --help`)
    Metrics(MetricsArgs),
    /// Post the review to the PR and set the commit status (needs `gh`, logged in)
    Post(PostArgs),
}

pub fn run(a: ReviewArgs) -> Result<ExitCode, String> {
    match a.cmd {
        ReviewCmd::Cache(c) => cache(c),
        ReviewCmd::Metrics(m) => metrics(m),
        ReviewCmd::Post(p) => post(p),
    }
}

fn out(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn home() -> PathBuf {
    crate::paths::home().unwrap_or_else(|| PathBuf::from("."))
}

fn env_dir(k: &str) -> Option<PathBuf> {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

// ---- time ------------------------------------------------------------------------

/// Now, as `2026-11-14T08:00:00.123456+00:00` (what the cache always wrote).
fn now_iso() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let base = crate::mem::time::format_unix(d.as_secs());
    format!(
        "{}.{:06}+00:00",
        base.trim_end_matches('Z'),
        d.subsec_micros()
    )
}

/// Seconds since the epoch for an ISO 8601 time with an offset (`Z` or
/// `+HH:MM`); a time without one is refused, as before.
fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let mut d = date.split('-');
    let (y, mo, da): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let (clock, off) = if let Some(c) = rest.strip_suffix('Z') {
        (c, 0)
    } else {
        let i = rest.rfind(['+', '-'])?;
        let (c, o) = rest.split_at(i);
        let sign = if o.starts_with('-') { -1 } else { 1 };
        let mut hm = o[1..].split(':');
        let oh: i64 = hm.next()?.parse().ok()?;
        let om: i64 = hm.next().unwrap_or("0").parse().ok()?;
        (c, sign * (oh * 3600 + om * 60))
    };
    let clock = clock.split('.').next()?;
    let mut t = clock.split(':');
    let (h, mi, se): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
    );
    // days from civil (Howard Hinnant)
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se - off)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Whole days since `iso`, rounded down (negative for the future).
fn age_days(iso: &str) -> Option<i64> {
    parse_iso(iso).map(|t| (now_secs() - t).div_euclid(86_400))
}

// ---- cache -------------------------------------------------------------------------

#[derive(Args)]
struct CacheArgs {
    /// The folder holding the sh-kb-* knowledge bases (default: $INDRA_SKILLS_DIR,
    /// else found from this folder up, then in ~/.agents/skills and ~/.claude/skills)
    #[arg(long, global = true)]
    skills: Option<PathBuf>,
    #[command(subcommand)]
    cmd: CacheCmd,
}

#[derive(Subcommand)]
pub(crate) enum CacheCmd {
    /// Hash of every sh-kb-* knowledge base
    KbVersion,
    /// The full key context, as JSON
    Key {
        #[arg(long)]
        model: Option<String>,
    },
    /// The cached run, or a miss with its reason
    Get {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        pr: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = DEFAULT_TTL_DAYS)]
        max_age_days: i64,
    },
    /// Store a run (a JSON object)
    Put {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        pr: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        model: Option<String>,
    },
    /// Which paths changed since the cached run (input: {"path": "<sha>"})
    Changed {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        pr: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// What is cached and how big it is
    Stat {
        #[arg(long)]
        repo: Option<String>,
    },
    /// Drop expired entries
    Prune {
        #[arg(long, default_value_t = DEFAULT_TTL_DAYS)]
        max_age_days: i64,
    },
    /// Drop everything, or one repo
    Clear {
        #[arg(long)]
        repo: Option<String>,
    },
}

fn cache_root() -> PathBuf {
    let v = format!("v{}", SCHEMA);
    if let Some(d) = env_dir("SH_REVIEW_CACHE_DIR") {
        return d.join(v);
    }
    if cfg!(windows) {
        return env_dir("LOCALAPPDATA")
            .unwrap_or_else(home)
            .join("indra")
            .join("cache")
            .join(v);
    }
    if cfg!(target_os = "macos") {
        return home().join("Library").join("Caches").join("indra").join(v);
    }
    env_dir("XDG_CACHE_HOME")
        .unwrap_or_else(|| home().join(".cache"))
        .join("indra")
        .join(v)
}

fn repo_slug(repo: &str) -> String {
    repo.replace(['/', '\\'], "__")
}

/// `owner/repo`, as GitHub spells them: two names of letters, digits, `-`,
/// `_` and `.`, neither of them `.` or `..`. Anything else is refused before it
/// becomes a cache folder or a `gh api` path.
pub(crate) fn valid_repo(repo: &str) -> bool {
    let parts: Vec<&str> = repo.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

fn entry_path(repo: &str, pr: &str) -> PathBuf {
    cache_root()
        .join(repo_slug(repo))
        .join(format!("pr-{}.json", pr))
}

fn has_kb(d: &Path) -> bool {
    std::fs::read_dir(d)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("sh-kb-") && e.path().is_dir())
        })
        .unwrap_or(false)
}

/// The folder holding the knowledge bases.
fn skills_dir(given: &Option<PathBuf>) -> Option<PathBuf> {
    if let Some(d) = given.clone().or_else(|| env_dir("INDRA_SKILLS_DIR")) {
        return Some(d);
    }
    let mut d = std::env::current_dir().ok();
    while let Some(x) = d {
        for c in [
            x.clone(),
            x.join("skills"),
            x.join("plugins").join("indra").join("skills"),
        ] {
            if has_kb(&c) {
                return Some(c);
            }
        }
        d = x.parent().map(Path::to_path_buf);
    }
    [
        home().join(".agents").join("skills"),
        home().join(".claude").join("skills"),
    ]
    .into_iter()
    .find(|c| has_kb(c))
}

/// The plugin (or extension) manifest above the knowledge bases.
fn manifest(skills: &Path) -> Option<PathBuf> {
    for name in [
        [".claude-plugin", "plugin.json"].as_slice(),
        ["gemini-extension.json"].as_slice(),
    ] {
        let mut d = Some(skills.to_path_buf());
        while let Some(x) = d {
            let p = name.iter().fold(x.clone(), |p, n| p.join(n));
            if p.is_file() {
                return Some(p);
            }
            d = x.parent().map(Path::to_path_buf);
        }
    }
    None
}

/// A hash over every knowledge base a hunter could load. Any change to a base
/// invalidates every cached finding, with no version number to forget to bump.
fn kb_version(skills: &Option<PathBuf>) -> String {
    let Some(root) = skills_dir(skills).filter(|d| d.is_dir()) else {
        // No bases means no basis for trusting a cached verdict: every lookup misses.
        return "no-skills-dir".into();
    };
    let mut h = Sha256::new();
    fn walk(root: &Path, dir: &Path, h: &mut Sha256) {
        let rel = dir
            .strip_prefix(root)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for e in rd.flatten() {
            let ft = match e.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let name = e.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                dirs.push(name);
            } else {
                files.push(name);
            }
        }
        files.sort();
        dirs.sort();
        if rel.is_empty() || rel.starts_with("sh-kb-") {
            for f in files {
                if !(f.ends_with(".md") || f.ends_with(".json")) {
                    continue;
                }
                let p = dir.join(&f);
                let relp = if rel.is_empty() {
                    f.clone()
                } else {
                    format!("{}/{}", rel, f)
                };
                h.update(relp.as_bytes());
                if let Ok(b) = std::fs::read(&p) {
                    h.update(&b);
                }
            }
        }
        for d in dirs {
            walk(root, &dir.join(d), h);
        }
    }
    walk(&root, &root, &mut h);
    h.finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()[..16]
        .to_string()
}

fn skill_version(skills: &Option<PathBuf>) -> String {
    skills_dir(skills)
        .and_then(|d| manifest(&d))
        .and_then(|m| std::fs::read_to_string(m).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("version").cloned())
        .map(|v| crate::findings::text(&v))
        .unwrap_or_else(|| "unknown".into())
}

fn key_context(skills: &Option<PathBuf>, model: &Option<String>) -> Value {
    let model = model
        .clone()
        .filter(|m| !m.is_empty())
        .or_else(|| {
            std::env::var("SH_REVIEW_MODEL")
                .ok()
                .filter(|m| !m.is_empty())
        })
        .unwrap_or_else(|| "unspecified".into());
    json!({ "schema": SCHEMA, "kb_version": kb_version(skills), "skill_version": skill_version(skills), "model": model })
}

fn miss(reason: &str, extra: Value) -> Result<Value, String> {
    let mut d = json!({ "hit": false, "reason": reason });
    if let (Some(o), Value::Object(x)) = (d.as_object_mut(), extra) {
        o.extend(x);
    }
    Ok(d)
}

fn read_json(p: &Path) -> Result<Value, String> {
    let t =
        std::fs::read_to_string(p).map_err(|e| format!("cannot read {}: {}", p.display(), e))?;
    serde_json::from_str(&t).map_err(|e| format!("cannot read {}: {}", p.display(), e))
}

fn entries() -> Vec<(String, PathBuf)> {
    let mut v = Vec::new();
    let Ok(rd) = std::fs::read_dir(cache_root()) else {
        return v;
    };
    let mut slugs: Vec<_> = rd.flatten().filter(|e| e.path().is_dir()).collect();
    slugs.sort_by_key(|e| e.file_name());
    for s in slugs {
        let Ok(rd) = std::fs::read_dir(s.path()) else {
            continue;
        };
        let mut fs: Vec<_> = rd
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .collect();
        fs.sort_by_key(|e| e.file_name());
        for f in fs {
            v.push((s.file_name().to_string_lossy().into_owned(), f.path()));
        }
    }
    v
}

/// One cache command; its JSON answer.
pub(crate) fn cache_value(sk: &Option<PathBuf>, cmd: CacheCmd) -> Result<Value, String> {
    let last: Value;
    match cmd {
        CacheCmd::KbVersion => last = json!(kb_version(sk)),
        CacheCmd::Key { model } => last = key_context(sk, &model),
        CacheCmd::Get {
            repo,
            pr,
            model,
            max_age_days,
        } => {
            let p = entry_path(&repo, &pr);
            if !p.is_file() {
                return miss("no cached run for this PR", json!({}));
            }
            let mut entry = match read_json(&p) {
                Ok(e) => e,
                Err(_) => return miss("cache entry unreadable (ValueError)", json!({})),
            };
            let want = key_context(sk, &model);
            if entry.get("schema") != Some(&json!(SCHEMA)) {
                return miss("cache schema changed", json!({}));
            }
            if entry.get("kb_version") != Some(&want["kb_version"]) {
                return miss(
                    "knowledge bases changed since this run was cached",
                    json!({}),
                );
            }
            if entry.get("skill_version") != Some(&want["skill_version"]) {
                return miss("skill version changed since this run was cached", json!({}));
            }
            // An unspecified model on either side is not a match: a haiku verdict
            // and an opus verdict are not interchangeable.
            if entry.get("model") != Some(&want["model"]) || want["model"] == "unspecified" {
                return miss(
                    "model differs or is unspecified",
                    json!({ "cached_model": entry.get("model").cloned().unwrap_or(Value::Null), "current_model": want["model"] }),
                );
            }
            match entry
                .get("created_at")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                None => return miss("cache entry has no timestamp", json!({})),
                Some(c) => match age_days(c) {
                    None => return miss("cache entry has an unparseable timestamp", json!({})),
                    Some(age) if age > max_age_days => {
                        return miss(
                            &format!("cached run is {} days old (max {})", age, max_age_days),
                            json!({}),
                        )
                    }
                    _ => {}
                },
            }
            entry["hit"] = json!(true);
            last = entry;
        }
        CacheCmd::Put {
            repo,
            pr,
            file,
            model,
        } => {
            let mut entry = read_json(&file)?;
            let Some(o) = entry.as_object_mut() else {
                return Err("entry must be a JSON object".into());
            };
            if let Value::Object(k) = key_context(sk, &model) {
                o.extend(k);
            }
            o.insert("repo".into(), json!(repo));
            o.insert("pr".into(), json!(pr));
            o.insert("created_at".into(), json!(now_iso()));
            o.remove("hit");
            let p = entry_path(&repo, &pr);
            std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
            let tmp = p.with_extension("json.tmp");
            std::fs::write(
                &tmp,
                serde_json::to_string_pretty(&entry).unwrap_or_default(),
            )
            .map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
            last = json!({ "stored": p.display().to_string(), "kb_version": entry["kb_version"],
                         "skill_version": entry["skill_version"], "model": entry["model"] });
        }
        CacheCmd::Changed { repo, pr, file } => {
            // A path the cache has never seen is always changed; never assume an
            // unseen file is clean.
            let current = read_json(&file)?;
            let cur: Map<String, Value> = current.as_object().cloned().unwrap_or_default();
            let p = entry_path(&repo, &pr);
            if !p.is_file() {
                let all: Vec<&String> = cur.keys().collect();
                last = json!({ "reusable": [], "changed": all, "reason": "no cached run; everything must be analysed" });
                return Ok(last);
            }
            let cached = read_json(&p)?
                .get("files")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let (mut changed, mut reusable) = (vec![], vec![]);
            for (path, sha) in &cur {
                if cached.get(path) == Some(sha) && !sha.is_null() {
                    reusable.push(path.clone());
                } else {
                    changed.push(path.clone());
                }
            }
            // Gone paths matter: their findings must be dropped, not carried forward.
            let vanished: Vec<&String> = cached.keys().filter(|k| !cur.contains_key(*k)).collect();
            last = json!({ "reusable": reusable, "changed": changed, "vanished": vanished });
        }
        CacheCmd::Stat { repo } => {
            let kbv = kb_version(sk);
            let (mut items, mut total) = (vec![], 0u64);
            for (slug, p) in entries() {
                if let Some(r) = &repo {
                    if slug != repo_slug(r) {
                        continue;
                    }
                }
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                total += size;
                match read_json(&p) {
                    Ok(e) => items.push(json!({
                        "repo": e.get("repo").cloned().unwrap_or(json!(slug)), "pr": e.get("pr").cloned().unwrap_or(Value::Null),
                        "head_sha": e.get("head_sha").and_then(Value::as_str).unwrap_or("").chars().take(8).collect::<String>(),
                        "findings": e.get("findings").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
                        "created_at": e.get("created_at").cloned().unwrap_or(Value::Null),
                        "stale_kb": e.get("kb_version").and_then(Value::as_str) != Some(kbv.as_str()),
                        "bytes": size,
                    })),
                    Err(_) => items.push(json!({ "repo": slug, "unreadable": p.file_name().map(|f| f.to_string_lossy().into_owned()), "bytes": size })),
                }
            }
            last = json!({ "root": cache_root().display().to_string(), "entries": items.len(), "bytes": total,
                         "current_kb_version": kbv, "items": items });
        }
        CacheCmd::Prune { max_age_days } => {
            let mut removed = vec![];
            for (_, p) in entries() {
                let drop = match read_json(&p) {
                    Ok(e) => {
                        let old = match e
                            .get("created_at")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            None => true,
                            Some(c) => age_days(c).map(|a| a > max_age_days).unwrap_or(true),
                        };
                        old || e.get("schema") != Some(&json!(SCHEMA))
                    }
                    Err(_) => true, // unreadable entries are never useful
                };
                if drop && std::fs::remove_file(&p).is_ok() {
                    removed.push(p.display().to_string());
                }
            }
            last = json!({ "removed": removed.len(), "paths": removed });
        }
        CacheCmd::Clear { repo } => {
            let root = cache_root();
            let target = match &repo {
                Some(r) if !valid_repo(r) => {
                    return Err(format!("not an owner/repo: {:?}", r));
                }
                Some(r) => root.join(repo_slug(r)),
                None => root,
            };
            if target.is_dir() {
                std::fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
                last = json!({ "cleared": target.display().to_string() });
            } else {
                last = json!({ "cleared": null, "reason": format!("nothing at {}", target.display()) });
            }
        }
    }
    Ok(last)
}

fn cache(a: CacheArgs) -> Result<ExitCode, String> {
    match cache_value(&a.skills, a.cmd)? {
        Value::String(s) => println!("{}", s),
        Value::Null => {}
        v => out(&v),
    }
    Ok(ExitCode::SUCCESS)
}

// ---- metrics ---------------------------------------------------------------------------

#[derive(Args)]
struct MetricsArgs {
    #[command(subcommand)]
    cmd: MetricsCmd,
}

#[derive(Subcommand)]
pub(crate) enum MetricsCmd {
    /// Print the metrics directory
    Path,
    /// Open a run
    Start {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        pr: Option<String>,
        #[arg(long, default_value = "pr-review")]
        mode: String,
        #[arg(long)]
        tier: Option<i64>,
        /// The flags the user passed, verbatim
        #[arg(long, allow_hyphen_values = true)]
        arguments: Option<String>,
        /// "hit" or the miss reason
        #[arg(long)]
        cache: Option<String>,
        #[arg(long)]
        skill_version: Option<String>,
    },
    /// Record one phase or agent
    Event {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        phase: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        vuln_class: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        tokens_in: Option<i64>,
        #[arg(long)]
        tokens_out: Option<i64>,
        #[arg(long)]
        duration_ms: Option<i64>,
        #[arg(long, default_value = "ok")]
        status: String,
        #[arg(long)]
        reused: bool,
        #[arg(long, allow_hyphen_values = true)]
        note: Option<String>,
    },
    /// Close a run with its totals
    Finish {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        verdict: Option<String>,
        #[arg(long)]
        status_state: Option<String>,
        #[arg(long)]
        findings: Option<i64>,
        #[arg(long)]
        posted: Option<i64>,
        #[arg(long)]
        deduped: Option<i64>,
        #[arg(long)]
        reused_files: Option<i64>,
        #[arg(long)]
        reused_verdicts: Option<i64>,
        #[arg(long)]
        duration_ms: Option<i64>,
        #[arg(long)]
        tokens_in: Option<i64>,
        #[arg(long)]
        tokens_out: Option<i64>,
        #[arg(long)]
        cost_usd: Option<f64>,
        #[arg(long)]
        agents_launched: Option<i64>,
    },
    /// Aggregate what was recorded
    Report {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        last: Option<usize>,
    },
    /// Delete records (all, or older than N days)
    Purge {
        #[arg(long)]
        older_than_days: Option<i64>,
    },
}

fn metrics_dir() -> PathBuf {
    if let Some(d) = env_dir("SH_METRICS_DIR") {
        return d;
    }
    if cfg!(windows) {
        return env_dir("LOCALAPPDATA")
            .unwrap_or_else(home)
            .join("indra")
            .join("metrics");
    }
    if cfg!(target_os = "macos") {
        return home()
            .join("Library")
            .join("Application Support")
            .join("indra")
            .join("metrics");
    }
    env_dir("XDG_DATA_HOME")
        .unwrap_or_else(|| home().join(".local").join("share"))
        .join("indra")
        .join("metrics")
}

/// Anything that looks like a credential is replaced before it reaches disk:
/// local files get pasted into issues.
fn redact(s: &str) -> String {
    use std::sync::OnceLock;
    static PATS: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    let pats = PATS.get_or_init(|| {
        vec![
            (
                regex::Regex::new(r"\b(gh[pousr]_[A-Za-z0-9]{16,})\b").unwrap(),
                "<redacted-gh-token>",
            ),
            (
                regex::Regex::new(r"\b(sk-[A-Za-z0-9_-]{16,})\b").unwrap(),
                "<redacted-api-key>",
            ),
            (
                regex::Regex::new(r"\b(AIza[0-9A-Za-z_-]{20,})\b").unwrap(),
                "<redacted-api-key>",
            ),
            (
                regex::Regex::new(
                    r"(?i)\b(authorization|token|secret|password|api[_-]?key)\s*[:=]\s*\S+",
                )
                .unwrap(),
                "${1}=<redacted>",
            ),
        ]
    });
    let mut t = s.to_string();
    for (re, rep) in pats {
        t = re.replace_all(&t, *rep).into_owned();
    }
    t
}

fn append(name: &str, mut rec: Map<String, Value>) -> Result<PathBuf, String> {
    let d = metrics_dir();
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    for v in rec.values_mut() {
        if let Value::String(s) = v {
            *s = redact(s);
        }
    }
    rec.insert("schema".into(), json!(SCHEMA));
    rec.entry("at").or_insert_with(|| json!(now_iso()));
    let p = d.join(name);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{}", Value::Object(rec)).map_err(|e| e.to_string())?;
    Ok(p)
}

fn read_rows(name: &str) -> Vec<Value> {
    // A torn final line from a killed run is not an error.
    std::fs::read_to_string(metrics_dir().join(name))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect()
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// One metrics command; its JSON answer.
pub(crate) fn metrics_value(cmd: MetricsCmd) -> Result<Value, String> {
    let res = match cmd {
        MetricsCmd::Path => {
            let d = metrics_dir();
            json!({ "dir": d.display().to_string(), "runs": d.join("runs.jsonl").display().to_string(),
                         "events": d.join("events.jsonl").display().to_string(), "exists": d.is_dir(), "local_only": true })
        }
        MetricsCmd::Start {
            run_id,
            repo,
            pr,
            mode,
            tier,
            arguments,
            cache,
            skill_version,
        } => {
            let p = append(
                "runs.jsonl",
                obj(
                    json!({ "event": "start", "run_id": run_id, "repo": repo, "pr": pr, "mode": mode,
                "tier": tier, "arguments": arguments, "cache": cache, "skill_version": skill_version }),
                ),
            )?;
            json!({ "recorded": "start", "run_id": run_id, "file": p.display().to_string() })
        }
        MetricsCmd::Event {
            run_id,
            phase,
            agent,
            vuln_class,
            model,
            tokens_in,
            tokens_out,
            duration_ms,
            status,
            reused,
            note,
        } => {
            let p = append(
                "events.jsonl",
                obj(
                    json!({ "run_id": run_id, "phase": phase, "agent": agent, "vuln_class": vuln_class,
                "model": model, "tokens_in": tokens_in, "tokens_out": tokens_out, "duration_ms": duration_ms, "status": status,
                "reused": reused, "note": note }),
                ),
            )?;
            json!({ "recorded": "event", "run_id": run_id, "phase": phase, "file": p.display().to_string() })
        }
        MetricsCmd::Finish {
            run_id,
            verdict,
            status_state,
            findings,
            posted,
            deduped,
            reused_files,
            reused_verdicts,
            duration_ms,
            tokens_in,
            tokens_out,
            cost_usd,
            agents_launched,
        } => {
            let p = append(
                "runs.jsonl",
                obj(
                    json!({ "event": "finish", "run_id": run_id, "verdict": verdict,
                "status_state": status_state, "findings": findings, "posted": posted, "deduped": deduped,
                "reused_files": reused_files, "reused_verdicts": reused_verdicts, "duration_ms": duration_ms,
                "tokens_in": tokens_in, "tokens_out": tokens_out, "cost_usd": cost_usd, "agents_launched": agents_launched }),
                ),
            )?;
            json!({ "recorded": "finish", "run_id": run_id, "file": p.display().to_string() })
        }
        MetricsCmd::Report { repo, last } => {
            let (runs, events) = (read_rows("runs.jsonl"), read_rows("events.jsonl"));
            let id = |r: &Value| crate::findings::field(r, "run_id");
            let mut starts: std::collections::HashMap<String, Value> = Default::default();
            let mut fins: std::collections::HashMap<String, Value> = Default::default();
            for r in runs {
                match r.get("event").and_then(Value::as_str) {
                    Some("start") => {
                        starts.insert(id(&r), r);
                    }
                    Some("finish") => {
                        fins.insert(id(&r), r);
                    }
                    _ => {}
                }
            }
            let mut ids: Vec<String> = starts
                .iter()
                .filter(|(_, s)| {
                    repo.as_ref()
                        .map(|r| s.get("repo").and_then(Value::as_str) == Some(r))
                        .unwrap_or(true)
                })
                .map(|(k, _)| k.clone())
                .collect();
            ids.sort_by_key(|i| {
                starts[i]
                    .get("at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            });
            if let Some(n) = last.filter(|n| *n > 0) {
                let skip = ids.len().saturating_sub(n);
                ids.drain(..skip);
            }
            let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            let mut by_model: Map<String, Value> = Map::new();
            let mut rows = vec![];
            for i in &ids {
                let s = &starts[i];
                let f = fins.get(i).cloned().unwrap_or(json!({}));
                for e in events.iter().filter(|e| id(e) == *i) {
                    let m = e
                        .get("model")
                        .and_then(Value::as_str)
                        .filter(|m| !m.is_empty())
                        .unwrap_or("unknown")
                        .to_string();
                    let b = by_model.entry(m).or_insert(
                        json!({ "events": 0, "tokens_in": 0, "tokens_out": 0, "ms": 0 }),
                    );
                    b["events"] = json!(b["events"].as_i64().unwrap_or(0) + 1);
                    for (k, src) in [
                        ("tokens_in", "tokens_in"),
                        ("tokens_out", "tokens_out"),
                        ("ms", "duration_ms"),
                    ] {
                        b[k] = json!(b[k].as_i64().unwrap_or(0) + num(e, src) as i64);
                    }
                }
                let g = |v: &Value, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
                rows.push(json!({
                    "run_id": i, "at": g(s, "at"), "repo": g(s, "repo"), "pr": g(s, "pr"), "tier": g(s, "tier"), "cache": g(s, "cache"),
                    "verdict": g(&f, "verdict"), "status": g(&f, "status_state"), "agents": g(&f, "agents_launched"),
                    "findings": g(&f, "findings"), "reused_files": g(&f, "reused_files"), "reused_verdicts": g(&f, "reused_verdicts"),
                    "duration_ms": g(&f, "duration_ms"), "cost_usd": g(&f, "cost_usd"), "complete": fins.contains_key(i),
                }));
            }
            let cost: f64 = rows.iter().map(|r| num(r, "cost_usd")).sum();
            let totals = json!({
                "runs": rows.len(),
                "incomplete": rows.iter().filter(|r| r["complete"] == false).count(),
                "cost_usd": (cost * 10_000.0).round() / 10_000.0,
                "duration_ms": rows.iter().map(|r| num(r, "duration_ms") as i64).sum::<i64>(),
                "agents": rows.iter().map(|r| num(r, "agents") as i64).sum::<i64>(),
            });
            json!({ "dir": metrics_dir().display().to_string(), "totals": totals, "by_model": by_model, "runs": rows })
        }
        MetricsCmd::Purge { older_than_days } => {
            let d = metrics_dir();
            let Some(days) = older_than_days else {
                let mut removed = vec![];
                for n in ["runs.jsonl", "events.jsonl"] {
                    let p = d.join(n);
                    if p.is_file() && std::fs::remove_file(&p).is_ok() {
                        removed.push(p.display().to_string());
                    }
                }
                return Ok(json!({ "purged": removed }));
            };
            let cutoff = now_secs() - days * 86_400;
            let (mut kept_total, mut dropped_total) = (0, 0);
            for n in ["runs.jsonl", "events.jsonl"] {
                let rows = read_rows(n);
                // Unparseable timestamps are kept, not silently dropped.
                let kept: Vec<&Value> = rows
                    .iter()
                    .filter(|r| {
                        r.get("at")
                            .and_then(Value::as_str)
                            .and_then(parse_iso)
                            .map(|t| t >= cutoff)
                            .unwrap_or(true)
                    })
                    .collect();
                dropped_total += rows.len() - kept.len();
                kept_total += kept.len();
                let p = d.join(n);
                if p.is_file() {
                    let text: String = kept.iter().map(|r| format!("{}\n", r)).collect();
                    std::fs::write(&p, text).map_err(|e| e.to_string())?;
                }
            }
            json!({ "kept": kept_total, "dropped": dropped_total })
        }
    };
    Ok(res)
}

fn metrics(a: MetricsArgs) -> Result<ExitCode, String> {
    let v = metrics_value(a.cmd)?;
    if !v.is_null() {
        out(&v);
    }
    Ok(ExitCode::SUCCESS)
}

// ---- post ------------------------------------------------------------------------------

#[derive(Args)]
pub(crate) struct PostArgs {
    /// owner/repo
    #[arg(long)]
    pub(crate) repo: String,
    #[arg(long)]
    pub(crate) pr: String,
    /// review-payload.json
    #[arg(long)]
    pub(crate) payload: PathBuf,
    /// verdict.json (state + description)
    #[arg(long)]
    pub(crate) verdict: Option<PathBuf>,
    #[arg(long, default_value = "security/pr-review")]
    pub(crate) status_context: String,
    #[arg(long)]
    pub(crate) target_url: Option<String>,
    /// Append receipts (JSONL) here
    #[arg(long)]
    pub(crate) receipts: Option<PathBuf>,
    #[arg(long)]
    pub(crate) no_status: bool,
    #[arg(long)]
    pub(crate) dry_run: bool,
}

const MAX_DESCRIPTION: usize = 140;

/// Run `gh`: (ok, parsed output or text, stderr).
fn gh(args: &[String], input: Option<&Path>) -> (bool, Value, String) {
    let mut c = std::process::Command::new("gh");
    c.args(args);
    if let Some(p) = input {
        c.arg("--input").arg(p);
    }
    let o = match c.output() {
        Ok(o) => o,
        Err(_) => return (false, Value::Null, "gh not found on PATH".into()),
    };
    let body = String::from_utf8_lossy(&o.stdout).trim().to_string();
    if !o.status.success() {
        return (
            false,
            Value::String(body),
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        );
    }
    let v = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_str(&body).unwrap_or(Value::String(body))
    };
    (true, v, String::new())
}

/// What posting says, for a terminal (printed as it goes) or a page (kept).
pub(crate) struct Sink {
    pub echo: bool,
    pub lines: Vec<String>,
}

impl Sink {
    fn out(&mut self, s: String) {
        if self.echo {
            println!("{}", s);
        }
        self.lines.push(s);
    }
    fn err(&mut self, s: String) {
        if self.echo {
            eprintln!("{}", s);
        }
        self.lines.push(s);
    }
}

#[allow(clippy::too_many_arguments)]
fn set_status(
    log: &mut Sink,
    repo: &str,
    sha: &str,
    state: &str,
    description: &str,
    context: &str,
    target_url: Option<&str>,
    dry: bool,
) -> bool {
    if !matches!(state, "success" | "failure" | "error" | "pending") {
        log.err(format!("error: invalid status state {:?}", state));
        return false;
    }
    // GitHub truncates hard at 140; truncating here keeps the visible text ours.
    let description = if description.chars().count() > MAX_DESCRIPTION {
        format!(
            "{}...",
            description
                .chars()
                .take(MAX_DESCRIPTION - 3)
                .collect::<String>()
        )
    } else {
        description.to_string()
    };
    if dry {
        log.out(format!("  [dry-run] status -> {}: {}", state, description));
        return true;
    }
    let mut args: Vec<String> = vec![
        "api".into(),
        format!("repos/{}/statuses/{}", repo, sha),
        "--method".into(),
        "POST".into(),
        "-f".into(),
        format!("state={}", state),
        "-f".into(),
        format!("context={}", context),
        "-f".into(),
        format!("description={}", description),
    ];
    if let Some(u) = target_url.filter(|u| !u.is_empty()) {
        args.extend(["-f".into(), format!("target_url={}", u)]);
    }
    let (ok, _, err) = gh(&args, None);
    if ok {
        log.out(format!("  status -> {}: {}", state, description));
    } else {
        log.err(format!("  FAILED to set status: {}", err));
    }
    ok
}

fn is_line_error(err: &str, out: &Value) -> bool {
    let blob = format!(
        "{} {}",
        err,
        match out {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        }
    )
    .to_lowercase();
    blob.contains("not part of the diff") || blob.contains("pull_request_review_thread.line")
}

/// Move every inline comment into the body, once, after a 422. Never shift a
/// line number until the API accepts it: that posts a real finding against a
/// line that has nothing to do with it.
fn demote_comments(payload: &Value) -> (Value, usize) {
    let comments = payload
        .get("comments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if comments.is_empty() {
        return (payload.clone(), 0);
    }
    let mut lines: Vec<String> = vec![
        String::new(), "### Findings without a diff anchor".into(), String::new(),
        "_These could not be anchored to a line in this diff, so they are listed here rather than posted inline._".into(),
        String::new(),
    ];
    for c in &comments {
        let mut loc = format!("`{}`", c.get("path").and_then(Value::as_str).unwrap_or("?"));
        if crate::findings::truthy(c, "line") {
            loc.push_str(&format!(" line {}", crate::findings::field(c, "line")));
        }
        lines.push(format!("- {}", loc));
        for ln in c.get("body").and_then(Value::as_str).unwrap_or("").lines() {
            lines.push(format!("  {}", ln));
        }
        lines.push(String::new());
    }
    let mut p = payload.clone();
    p["body"] = json!(format!(
        "{}{}",
        payload.get("body").and_then(Value::as_str).unwrap_or(""),
        lines.join("\n")
    ));
    p["comments"] = json!([]);
    (p, comments.len())
}

fn write_receipts(path: &Option<PathBuf>, rows: &[Value]) {
    let (Some(p), false) = (path, rows.is_empty()) else {
        return;
    };
    if let Some(d) = std::path::absolute(p)
        .ok()
        .and_then(|a| a.parent().map(Path::to_path_buf))
    {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        for r in rows {
            let _ = writeln!(f, "{}", r);
        }
    }
}

fn post(a: PostArgs) -> Result<ExitCode, String> {
    let mut log = Sink {
        echo: true,
        lines: vec![],
    };
    Ok(ExitCode::from(post_with(&a, &mut log)))
}

/// Post the review and set the status; the exit code (0 posted, 1 failed with
/// the status still set, 2 bad input).
pub(crate) fn post_with(a: &PostArgs, log: &mut Sink) -> u8 {
    let mut load = |p: &Path, what: &str| -> Option<Value> {
        match read_json(p) {
            Ok(v) => Some(v),
            Err(e) => {
                log.err(format!(
                    "error: cannot read {} ({}): {}",
                    what,
                    p.display(),
                    e
                ));
                None
            }
        }
    };
    let Some(mut payload) = load(&a.payload, "payload") else {
        return 2;
    };
    let verdict = match &a.verdict {
        Some(v) => match load(v, "verdict") {
            Some(v) => v,
            None => return 2,
        },
        None => json!({}),
    };
    let sha = [payload.get("commit_id"), verdict.get("head_sha")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty())
        .map(str::to_string);
    let Some(sha) = sha else {
        log.err("error: no commit_id in payload and no head_sha in verdict.\nThe review must be pinned to the SHA that was analysed.".to_string());
        return 2;
    };
    // Never let a bot block or approve a merge: the commit status is the gate.
    let event = payload
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or("COMMENT")
        .to_string();
    if event != "COMMENT" {
        log.err(format!(
            "error: review event is '{}'; only COMMENT is allowed.",
            event
        ));
        return 2;
    }
    let state = verdict
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("error")
        .to_string();
    let description = verdict
        .get("description")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("Security review completed.")
        .to_string();
    let at = crate::mem::time::now_iso();
    let mut receipts: Vec<Value> = vec![];
    let n_inline = payload
        .get("comments")
        .and_then(Value::as_array)
        .map(|c| c.len())
        .unwrap_or(0);
    log.out(format!(
        "Posting review to {}#{} ({} inline + 1 summary), pinned to {}",
        a.repo,
        a.pr,
        n_inline,
        &sha[..sha.len().min(8)]
    ));
    if a.dry_run {
        log.out("  [dry-run] nothing sent".to_string());
        set_status(
            log,
            &a.repo,
            &sha,
            &state,
            &description,
            &a.status_context,
            a.target_url.as_deref(),
            true,
        );
        return 0;
    }
    let endpoint = format!("repos/{}/pulls/{}/reviews", a.repo, a.pr);
    let send = |pl: &Value| -> (bool, Value, String) {
        let mut rnd = [0u8; 8];
        let _ = getrandom::fill(&mut rnd);
        let tmp = std::env::temp_dir().join(format!(
            "indra-review-{}.json",
            rnd.iter().map(|b| format!("{:02x}", b)).collect::<String>()
        ));
        if std::fs::write(&tmp, pl.to_string()).is_err() {
            return (false, Value::Null, "cannot write a temporary file".into());
        }
        let r = gh(
            &[
                "api".into(),
                endpoint.clone(),
                "--method".into(),
                "POST".into(),
            ],
            Some(&tmp),
        );
        let _ = std::fs::remove_file(&tmp);
        r
    };
    let (mut ok, mut res, mut err) = send(&payload);
    // One recovery attempt, and only for the anchoring case.
    if !ok && is_line_error(&err, &res) && n_inline > 0 {
        log.out("  422: a comment is outside the diff. Moving inline comments to the body and retrying once (never adjusting a line number).".to_string());
        let (p, moved) = demote_comments(&payload);
        payload = p;
        receipts.push(json!({ "action": "body_only", "reason": "not_anchorable_422", "count": moved, "at": at }));
        (ok, res, err) = send(&payload);
    }
    if !ok {
        let low = err.to_lowercase();
        let hint = if low.contains("403") || low.contains("forbidden") {
            "the token cannot write to this PR (needs pull-requests: write)"
        } else if low.contains("404") {
            "repo or PR not found, or no access"
        } else {
            "see the error above"
        };
        log.err(format!("  FAILED to post review: {}\n  ({})", err, hint));
        receipts.push(json!({ "action": "failed", "error": err.chars().take(500).collect::<String>(), "at": at }));
        write_receipts(&a.receipts, &receipts);
        // The mandatory tail: a computed verdict that never reaches the status is
        // worse than no check at all.
        set_status(
            log,
            &a.repo,
            &sha,
            "error",
            "Review completed but could not be posted. See the run log.",
            &a.status_context,
            a.target_url.as_deref(),
            false,
        );
        return 1;
    }
    let url = res
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_string);
    log.out(format!(
        "  posted: {}",
        url.as_deref().unwrap_or("(no url returned)")
    ));
    receipts.push(json!({ "action": "posted_review", "review_url": url,
        "inline": payload.get("comments").and_then(Value::as_array).map(|c| c.len()).unwrap_or(0), "at": at }));
    if a.no_status {
        log.out("  --no-status: commit status not set".to_string());
    } else {
        // target_url points at the review, so a red check leads straight to why.
        let target = a.target_url.clone().or(url);
        set_status(
            log,
            &a.repo,
            &sha,
            &state,
            &description,
            &a.status_context,
            target.as_deref(),
            false,
        );
    }
    write_receipts(&a.receipts, &receipts);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_round_trip_and_age() {
        assert_eq!(parse_iso("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(parse_iso("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(
            parse_iso("2026-11-13T12:00:00.123456+00:00"),
            Some(1_794_571_200)
        );
        assert_eq!(parse_iso("2026-11-13T14:00:00+02:00"), Some(1_794_571_200));
        assert_eq!(
            parse_iso("2026-11-13T12:00:00"),
            None,
            "a time without an offset is refused"
        );
        assert_eq!(age_days(&now_iso()), Some(0));
    }

    #[test]
    fn secrets_never_reach_the_metrics() {
        assert_eq!(
            redact("used ghp_abcdefghijklmnop1234 ok"),
            "used <redacted-gh-token> ok"
        );
        assert_eq!(
            redact("key sk-ABCDEFGHIJKLMNOPQR"),
            "key <redacted-api-key>"
        );
        assert_eq!(
            redact("Authorization: Bearer xyz"),
            "Authorization=<redacted> xyz"
        );
        assert_eq!(redact("nothing here"), "nothing here");
    }

    #[test]
    fn a_comment_off_the_diff_moves_to_the_body() {
        let p = json!({ "body": "Summary", "comments": [{ "path": "a.py", "line": 3, "body": "SQLi\nhere" }] });
        let (q, n) = demote_comments(&p);
        assert_eq!(n, 1);
        assert_eq!(q["comments"], json!([]));
        assert!(q["body"]
            .as_str()
            .unwrap()
            .contains("- `a.py` line 3\n  SQLi\n  here"));
        assert!(is_line_error(
            "Validation Failed: pull_request_review_thread.line must be part of the diff",
            &Value::Null
        ));
    }
}
