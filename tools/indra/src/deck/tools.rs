//! The review tooling, from the console: what `indra mem record-run`,
//! `indra publish plan`, `indra review` and `indra skills` do on the command
//! line, as API calls for the run pages and the Tools page. Both call the same
//! functions, so the CLI and the console always agree.

use crate::deck::web::{bad, forbidden, internal, not_found, ApiResult, App};
use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn writable(app: &App) -> ApiResult<()> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    Ok(())
}

/// The indra repository this console can see (for the skills mirror and the
/// site), from the harnesses' roots or the folder it was started in.
fn repo(app: &App) -> Option<PathBuf> {
    let s = app.settings();
    s.harnesses
        .iter()
        .map(|h| h.root.clone())
        .chain(std::env::current_dir().ok())
        .find_map(|p| crate::skills::find_repo(&p))
}

/// The folder holding the sh-kb-* knowledge bases (keys of the review cache).
fn skills_dir(app: &App) -> Option<PathBuf> {
    if let Some(r) = repo(app) {
        return Some(r.join("plugins").join("indra").join("skills"));
    }
    app.settings()
        .harnesses
        .iter()
        .map(|h| h.root.join("skills"))
        .find(|d| d.join("sh-kb-sqli").is_dir() || d.join("sh-security-review").is_dir())
}

// ---- review runs --------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RunQ {
    h: Option<String>,
    key: String,
}

fn run_dir(app: &App, h: Option<&str>, key: &str) -> ApiResult<PathBuf> {
    let h = app.harness(h)?;
    crate::deck::web::find_run(app, &h, key)
}

/// A PR review's repository and number, from its run.json.
fn pr_of(dir: &Path) -> (Option<String>, Option<String>) {
    let v: Value = std::fs::read_to_string(dir.join("run.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let s = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| v.get(*k).filter(|x| !x.is_null()))
            .map(crate::findings::text)
            .filter(|x| !x.is_empty())
    };
    (
        s(&["repo", "repository"]),
        s(&["pr", "number", "pr_number"]),
    )
}

/// Which actions a run supports: recording and publishing need its
/// verified.jsonl; posting needs a PR review's payload and verdict.
pub async fn actions(State(app): State<Arc<App>>, Query(q): Query<RunQ>) -> ApiResult<Json<Value>> {
    let dir = run_dir(&app, q.h.as_deref(), &q.key)?;
    let (repo, pr) = pr_of(&dir);
    Ok(Json(json!({
        "findings": dir.join("verified.jsonl").is_file(),
        "review": dir.join("review").join("review-payload.json").is_file(),
        "verdict": dir.join("review").join("verdict.json").is_file(),
        "repo": repo, "pr": pr,
        "vault": dir.parent().map(|p| p.join("memory").display().to_string()),
    })))
}

#[derive(Deserialize)]
pub struct RecordReq {
    h: Option<String>,
    key: String,
    #[serde(default)]
    dry_run: bool,
}

/// `indra mem record-run`: the run's findings into its memory vault.
pub async fn record(
    State(app): State<Arc<App>>,
    Json(r): Json<RecordReq>,
) -> ApiResult<Json<Value>> {
    if !r.dry_run {
        writable(&app)?;
    }
    let dir = run_dir(&app, r.h.as_deref(), &r.key)?;
    let vault = dir
        .parent()
        .map(|p| p.join("memory"))
        .ok_or_else(|| bad("the run has no parent folder"))?;
    let out = tokio::task::spawn_blocking(move || {
        if r.dry_run {
            // The commands it would run, as text, instead of printing them.
            let v = crate::mem::record::plan(&dir, &vault)?;
            Ok::<Value, String>(v)
        } else {
            crate::mem::record::record(&dir, &vault, false)
        }
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct PublishReq {
    h: Option<String>,
    key: String,
    to: String,
    #[serde(default = "medium")]
    min_severity: String,
    #[serde(default)]
    statuses: Vec<String>,
    #[serde(default = "twenty_five")]
    max: usize,
}

fn medium() -> String {
    "medium".into()
}

fn twenty_five() -> usize {
    25
}

/// `indra publish plan`: the records to create in a tracker.
pub async fn publish(
    State(app): State<Arc<App>>,
    Json(r): Json<PublishReq>,
) -> ApiResult<Json<Value>> {
    use crate::publish::Dest;
    let to = match r.to.as_str() {
        "jira" => Dest::Jira,
        "github" => Dest::Github,
        "confluence" => Dest::Confluence,
        "notion" => Dest::Notion,
        _ => return Err(bad("to must be jira, github, confluence or notion")),
    };
    let mut statuses = if r.statuses.is_empty() {
        vec!["verified".to_string()]
    } else {
        r.statuses
    };
    statuses.dedup();
    if statuses
        .iter()
        .any(|s| !matches!(s.as_str(), "verified" | "needs-runtime"))
    {
        return Err(bad(
            "statuses: verified and needs-runtime (false positives are never published)",
        ));
    }
    let dir = run_dir(&app, r.h.as_deref(), &r.key)?;
    let plan =
        crate::publish::plan(&dir, to, &r.min_severity, &statuses, r.max.max(1)).map_err(bad)?;
    Ok(Json(plan))
}

#[derive(Deserialize)]
pub struct PostReq {
    h: Option<String>,
    key: String,
    /// Override what run.json says.
    repo: Option<String>,
    pr: Option<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_status: bool,
}

/// `indra review post`: the review to the PR, and the commit status.
pub async fn post(State(app): State<Arc<App>>, Json(r): Json<PostReq>) -> ApiResult<Json<Value>> {
    if !r.dry_run {
        writable(&app)?;
    }
    let dir = run_dir(&app, r.h.as_deref(), &r.key)?;
    let (repo0, pr0) = pr_of(&dir);
    let repo = r
        .repo
        .filter(|x| !x.is_empty())
        .or(repo0)
        .ok_or_else(|| bad("which repository? run.json names none"))?;
    let pr =
        r.pr.filter(|x| !x.is_empty())
            .or(pr0)
            .ok_or_else(|| bad("which pull request? run.json names none"))?;
    if !crate::review::valid_repo(&repo) || pr.is_empty() || !pr.chars().all(|c| c.is_ascii_digit())
    {
        return Err(bad("repo must be owner/repo and pr a number"));
    }
    let review = dir.join("review");
    let verdict = review.join("verdict.json");
    let args = crate::review::PostArgs {
        repo,
        pr,
        payload: review.join("review-payload.json"),
        verdict: verdict.is_file().then_some(verdict),
        status_context: "security/pr-review".into(),
        target_url: None,
        receipts: Some(review.join("receipts.jsonl")),
        no_status: r.no_status,
        dry_run: r.dry_run,
    };
    let (code, lines) = tokio::task::spawn_blocking(move || {
        let mut log = crate::review::Sink {
            echo: false,
            lines: vec![],
        };
        let code = crate::review::post_with(&args, &mut log);
        (code, log.lines)
    })
    .await
    .map_err(|e| internal(e.to_string()))?;
    Ok(Json(
        json!({ "code": code, "ok": code == 0, "output": lines }),
    ))
}

// ---- the Tools page -----------------------------------------------------------------

/// The review cache, metrics, the skills mirror: what the Tools page shows.
pub async fn overview(State(app): State<Arc<App>>) -> ApiResult<Json<Value>> {
    use crate::review::{cache_value, metrics_value, CacheCmd, MetricsCmd};
    let skills = skills_dir(&app);
    let repo = repo(&app);
    let sk = skills.clone();
    let (cache, metrics, path, mirror) = tokio::task::spawn_blocking(move || {
        let cache = cache_value(&sk, CacheCmd::Stat { repo: None }).unwrap_or(Value::Null);
        let metrics = metrics_value(MetricsCmd::Report {
            repo: None,
            last: Some(100),
        })
        .unwrap_or(Value::Null);
        let path = metrics_value(MetricsCmd::Path).unwrap_or(Value::Null);
        let mirror = repo.as_deref().map(crate::skills::check);
        (cache, metrics, path, mirror)
    })
    .await
    .map_err(|e| internal(e.to_string()))?;
    Ok(Json(json!({
        "repo": self::repo(&app).map(|p| p.display().to_string()),
        "skills": skills.map(|p| p.display().to_string()),
        "cache": cache, "metrics": metrics, "metrics_path": path,
        "mirror": mirror.map(|(n, problems)| json!({ "files": n, "problems": problems })),
    })))
}

#[derive(Deserialize)]
pub struct CacheReq {
    /// prune | clear
    action: String,
    #[serde(default = "seven")]
    max_age_days: i64,
    repo: Option<String>,
}

fn seven() -> i64 {
    7
}

pub async fn cache(State(app): State<Arc<App>>, Json(r): Json<CacheReq>) -> ApiResult<Json<Value>> {
    use crate::review::{cache_value, CacheCmd};
    writable(&app)?;
    let cmd = match r.action.as_str() {
        "prune" => CacheCmd::Prune {
            max_age_days: r.max_age_days,
        },
        "clear" => CacheCmd::Clear {
            repo: r.repo.filter(|x| !x.is_empty()),
        },
        _ => return Err(bad("action must be prune or clear")),
    };
    let sk = skills_dir(&app);
    Ok(Json(cache_value(&sk, cmd).map_err(bad)?))
}

#[derive(Deserialize)]
pub struct PurgeReq {
    older_than_days: Option<i64>,
}

pub async fn metrics_purge(
    State(app): State<Arc<App>>,
    Json(r): Json<PurgeReq>,
) -> ApiResult<Json<Value>> {
    use crate::review::{metrics_value, MetricsCmd};
    writable(&app)?;
    Ok(Json(
        metrics_value(MetricsCmd::Purge {
            older_than_days: r.older_than_days,
        })
        .map_err(bad)?,
    ))
}

fn need_repo(app: &App) -> ApiResult<PathBuf> {
    repo(app).ok_or_else(|| {
        not_found("this console does not see the indra repository (start it inside a clone)")
    })
}

pub async fn skills_sync(State(app): State<Arc<App>>) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let r = need_repo(&app)?;
    let n = crate::skills::sync(&r).map_err(internal)?;
    Ok(Json(json!({ "files": n })))
}

#[derive(Deserialize)]
pub struct InstallReq {
    /// agents | claude | opencode
    to: String,
    /// user | workspace (a target of the harness)
    #[serde(default)]
    scope: String,
    h: Option<String>,
    #[serde(default)]
    target: usize,
}

pub async fn skills_install(
    State(app): State<Arc<App>>,
    Json(r): Json<InstallReq>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let repo = need_repo(&app)?;
    let ws = if r.scope == "workspace" {
        let h = app.harness(r.h.as_deref())?;
        Some(
            h.targets
                .get(r.target)
                .cloned()
                .ok_or_else(|| bad("unknown target"))?,
        )
    } else {
        None
    };
    let dest = crate::skills::install_dir(&r.to, ws.as_deref()).map_err(bad)?;
    let n = crate::skills::install(&repo, &dest).map_err(internal)?;
    Ok(Json(
        json!({ "skills": n, "dir": dest.display().to_string() }),
    ))
}

pub async fn site(State(app): State<Arc<App>>) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let repo = need_repo(&app)?;
    let out = repo.join("_site");
    let o = out.clone();
    let n = tokio::task::spawn_blocking(move || crate::site::build_site(&repo, &o))
        .await
        .map_err(|e| internal(e.to_string()))?
        .map_err(bad)?;
    Ok(Json(
        json!({ "pages": n, "out": out.display().to_string() }),
    ))
}

pub async fn dashes(State(app): State<Arc<App>>) -> ApiResult<Json<Value>> {
    let repo = need_repo(&app)?;
    let found = tokio::task::spawn_blocking(move || crate::site::find_dashes(&repo))
        .await
        .map_err(|e| internal(e.to_string()))?;
    let list: Vec<Value> = found
        .into_iter()
        .take(500)
        .map(|(f, n, t)| json!({ "file": f, "line": n, "text": t }))
        .collect();
    Ok(Json(json!({ "found": list })))
}

// ---- Ollama --------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct OllamaQ {
    url: Option<String>,
    model: Option<String>,
}

/// The Ollama server asked for, else the first profile's, else this machine's.
fn ollama_url(app: &App, asked: Option<String>) -> ApiResult<String> {
    let url = asked
        .filter(|u| !u.trim().is_empty())
        .or_else(|| {
            app.settings()
                .profiles
                .iter()
                .find_map(|p| p.ollama_url.clone().filter(|u| !u.trim().is_empty()))
        })
        .unwrap_or_else(|| crate::deck::nodes::OLLAMA_URL.to_string());
    crate::deck::router::check_url(&url).map_err(bad)?;
    Ok(url.trim_end_matches('/').to_string())
}

/// The Ollama server: its models (with tool support and context size, for
/// the model pickers and the Tools page), pulls in progress, and what the
/// model router has sent where.
pub async fn ollama(
    State(app): State<Arc<App>>,
    Query(q): Query<OllamaQ>,
) -> ApiResult<Json<Value>> {
    let url = ollama_url(&app, q.url)?;
    let (mut models, error) = match crate::deck::router::ollama_models(&url).await {
        Ok(m) => (m, None),
        Err(e) => (vec![], Some(e)),
    };
    // Each model's capabilities and window, a few at a time.
    let details = futures_util::future::join_all(models.iter().take(40).map(|m| {
        let name = m["name"].as_str().unwrap_or("").to_string();
        let url = url.clone();
        async move { crate::deck::ollama::details(&url, &name).await }
    }))
    .await;
    for (m, d) in models.iter_mut().zip(details) {
        if let (Some(o), Ok((caps, ctx))) = (m.as_object_mut(), d) {
            o.insert("tools".into(), json!(caps.iter().any(|c| c == "tools")));
            o.insert("capabilities".into(), json!(caps));
            o.insert("max_context".into(), json!(ctx));
        }
    }
    Ok(Json(json!({
        "url": url, "reachable": error.is_none(), "error": error, "models": models,
        "pulls": crate::deck::ollama::pull_status(),
        "router": crate::deck::router::stats(),
        "min_context": crate::deck::ollama::MIN_CONTEXT,
    })))
}

/// Whether a model will work for Claude Code (Launch asks before starting).
pub async fn ollama_check(
    State(app): State<Arc<App>>,
    Query(q): Query<OllamaQ>,
) -> ApiResult<Json<Value>> {
    let url = ollama_url(&app, q.url)?;
    let model = q
        .model
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| bad("which model?"))?;
    if !crate::deck::ollama::valid_model(&model) {
        return Err(bad("not a model name"));
    }
    Ok(Json(crate::deck::ollama::check(&url, &model).await))
}

#[derive(Deserialize)]
pub struct PullReq {
    url: Option<String>,
    model: String,
}

/// Pull a model onto the server; progress shows in GET /api/ollama.
pub async fn ollama_pull(
    State(app): State<Arc<App>>,
    Json(r): Json<PullReq>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let url = ollama_url(&app, r.url)?;
    crate::deck::ollama::start_pull(url, r.model.trim().to_string()).map_err(bad)?;
    Ok(Json(json!({ "started": true })))
}

// ---- triage and comparison ----------------------------------------------------------

/// The statuses a person can give a finding.
const TRIAGE: [&str; 3] = ["verified", "needs-runtime", "false-positive"];

/// One finding as the run page lists it.
fn brief(i: usize, f: &Value) -> Value {
    use crate::findings::field;
    json!({
        "index": i,
        "id": field(f, "id"),
        "title": field(f, "title"),
        "severity": field(f, "severity").to_lowercase(),
        "status": field(f, "status"),
        "class": field(f, "class"),
        "file": field(f, "file"),
        "line": f.get("line").cloned().unwrap_or(Value::Null),
        "fingerprint": crate::findings::fingerprint(f),
        "triage": f.get("triage").cloned().unwrap_or(Value::Null),
    })
}

/// A run's verified.jsonl, line by line, keeping lines that do not parse
/// so a rewrite never drops them.
fn lines_of(dir: &Path) -> ApiResult<Vec<Result<Value, String>>> {
    let p = dir.join("verified.jsonl");
    let text =
        std::fs::read_to_string(&p).map_err(|_| not_found("this run has no verified.jsonl"))?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).map_err(|_| l.to_string()))
        .collect())
}

/// The run's findings (verified.jsonl), for the triage table.
pub async fn findings(
    State(app): State<Arc<App>>,
    Query(q): Query<RunQ>,
) -> ApiResult<Json<Value>> {
    let dir = run_dir(&app, q.h.as_deref(), &q.key)?;
    let list: Vec<Value> = lines_of(&dir)?
        .iter()
        .enumerate()
        .filter_map(|(i, l)| l.as_ref().ok().map(|f| brief(i, f)))
        .collect();
    Ok(Json(json!({ "findings": list })))
}

#[derive(Deserialize)]
pub struct TriageReq {
    h: Option<String>,
    key: String,
    /// The finding's line in verified.jsonl, and its fingerprint (so a file
    /// that changed underneath is noticed rather than edited blindly).
    index: usize,
    fingerprint: String,
    status: String,
    #[serde(default)]
    reason: String,
}

/// Change a finding's status, keeping what it was and why it changed.
pub async fn triage(
    State(app): State<Arc<App>>,
    Json(r): Json<TriageReq>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    if !TRIAGE.contains(&r.status.as_str()) {
        return Err(bad(
            "status must be verified, needs-runtime or false-positive",
        ));
    }
    let reason = r.reason.trim().to_string();
    if r.status == "false-positive" && reason.is_empty() {
        return Err(bad("say why it is a false positive: the reason becomes the learning that stops it coming back"));
    }
    if reason.len() > 2000 || reason.chars().any(|c| c.is_control() && c != '\n') {
        return Err(bad("the reason is too long or has control characters"));
    }
    let dir = run_dir(&app, r.h.as_deref(), &r.key)?;
    let mut lines = lines_of(&dir)?;
    let f = match lines.get_mut(r.index) {
        Some(Ok(f)) if crate::findings::fingerprint(f) == r.fingerprint => f,
        _ => return Err(bad("that finding changed or moved; reload the run")),
    };
    let from = crate::findings::field(f, "status");
    if let Some(o) = f.as_object_mut() {
        o.insert("status".into(), json!(r.status));
        o.insert(
            "triage".into(),
            json!({ "from": from, "to": r.status, "reason": reason, "at": crate::mem::time::now_iso(), "by": "console" }),
        );
    }
    let out = brief(r.index, f);
    let text: String = lines
        .iter()
        .map(|l| match l {
            Ok(v) => v.to_string(),
            Err(raw) => raw.clone(),
        })
        .map(|l| l + "\n")
        .collect();
    let p = dir.join("verified.jsonl");
    let tmp = dir.join(".verified.jsonl.tmp");
    std::fs::write(&tmp, text).map_err(|e| internal(e.to_string()))?;
    std::fs::rename(&tmp, &p).map_err(|e| internal(e.to_string()))?;
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CompareQ {
    h: Option<String>,
    /// The newer run, and the one to compare it with.
    key: String,
    with: String,
}

/// Two runs' findings, matched by fingerprint: what is new in `key`, what
/// was fixed since `with`, what is still open, and what changed status.
/// False positives count as closed.
pub async fn compare(
    State(app): State<Arc<App>>,
    Query(q): Query<CompareQ>,
) -> ApiResult<Json<Value>> {
    let head = run_dir(&app, q.h.as_deref(), &q.key)?;
    let base = run_dir(&app, q.h.as_deref(), &q.with)?;
    let load = |d: &Path| -> ApiResult<std::collections::BTreeMap<String, Value>> {
        Ok(lines_of(d)?
            .iter()
            .enumerate()
            .filter_map(|(i, l)| l.as_ref().ok().map(|f| brief(i, f)))
            .map(|b| (b["fingerprint"].as_str().unwrap_or("").to_string(), b))
            .collect())
    };
    let (a, b) = (load(&base)?, load(&head)?);
    let open = |v: &Value| v["status"] != "false-positive";
    let (mut new, mut fixed, mut still, mut changed) = (vec![], vec![], vec![], vec![]);
    for (fp, f) in &b {
        match a.get(fp) {
            Some(old) => {
                if old["status"] != f["status"] {
                    changed.push(json!({ "finding": f, "from": old["status"], "to": f["status"] }));
                }
                if open(f) && open(old) {
                    still.push(f.clone());
                } else if open(f) {
                    new.push(f.clone());
                }
            }
            None if open(f) => new.push(f.clone()),
            None => {}
        }
    }
    for (fp, f) in &a {
        if open(f) && b.get(fp).map(|x| !open(x)).unwrap_or(true) {
            fixed.push(f.clone());
        }
    }
    let name = |d: &Path| crate::findings::run_id(d);
    Ok(Json(json!({
        "head": name(&head), "base": name(&base),
        "new": new, "fixed": fixed, "still_open": still, "changed": changed,
    })))
}

#[derive(Deserialize)]
pub struct PrReviewReq {
    h: Option<String>,
    /// A pull request URL, or a number for the target's own repository.
    pr: String,
    target: usize,
    #[serde(default)]
    node: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    runner: String,
    /// 0..3; empty lets the review pick from the diff.
    #[serde(default)]
    tier: Option<u8>,
    #[serde(default)]
    deep: bool,
    /// Leave the posting to the run page (the default).
    #[serde(default = "yes")]
    dry_run: bool,
    #[serde(default)]
    fail_on: Option<String>,
}

fn yes() -> bool {
    true
}

/// The `/sh-pr-review` prompt for a request, refusing anything that is not
/// a plain PR reference or a known flag.
pub(crate) fn pr_review_prompt(r: &PrReviewReq) -> ApiResult<String> {
    let pr = r.pr.trim();
    let url_ok = pr
        .strip_prefix("https://github.com/")
        .map(|rest| {
            let p: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
            p.len() == 4
                && crate::review::valid_repo(&format!("{}/{}", p[0], p[1]))
                && p[2] == "pull"
                && !p[3].is_empty()
                && p[3].chars().all(|c| c.is_ascii_digit())
        })
        .unwrap_or(false);
    let num_ok = !pr.is_empty() && pr.len() <= 9 && pr.chars().all(|c| c.is_ascii_digit());
    if !url_ok && !num_ok {
        return Err(bad(
            "give a pull request URL (https://github.com/owner/repo/pull/N) or a number",
        ));
    }
    let mut p = format!("/sh-pr-review {}", pr.trim_end_matches('/'));
    if let Some(t) = r.tier {
        if t > 3 {
            return Err(bad("tier is 0 to 3"));
        }
        p.push_str(&format!(" --tier={}", t));
    }
    if r.deep {
        p.push_str(" --deep");
    }
    if let Some(f) = r.fail_on.as_deref().filter(|f| !f.is_empty()) {
        if !["critical", "high", "medium", "low"].contains(&f) {
            return Err(bad("fail-on is critical, high, medium or low"));
        }
        p.push_str(&format!(" --fail-on={}", f));
    }
    if r.dry_run {
        p.push_str(" --dry-run");
    }
    Ok(p)
}

/// Start a PR review: `/sh-pr-review` in a target, with a profile.
pub async fn pr_review(
    State(app): State<Arc<App>>,
    Json(r): Json<PrReviewReq>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let prompt = pr_review_prompt(&r)?;
    // The same path as Launch: read-only, prompt, profile and node checks.
    let req: crate::deck::web::LaunchReq = serde_json::from_value(json!({
        "h": r.h, "node": r.node, "runner": r.runner, "prompt": prompt,
        "target": r.target, "profile": r.profile,
    }))
    .map_err(|e| internal(e.to_string()))?;
    let Json(started) = crate::deck::web::proc_start(State(app), Json(req)).await?;
    let id = started.get("id").cloned().unwrap_or(Value::Null);
    Ok(Json(json!({ "id": id, "prompt": prompt })))
}
