//! The review tooling, from the console: what `indra mem record-run`,
//! `indra publish plan`, `indra review` and `indra skills` do on the command
//! line, as API calls for the run pages and the Tools page. Both call the same
//! functions, so the CLI and the console always agree.

use crate::deck::runs;
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
    runs::find(&h, key).ok_or_else(|| not_found("no such run"))
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
