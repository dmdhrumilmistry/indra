//! Retests: is a finding still there? A retest re-checks one finding of a
//! review run, statically against the code as it is now or live by
//! replaying the traffic it cites inside an engagement, and records one
//! verdict:
//!
//! - `reproducible`: the vulnerable path (or the response) is still there;
//! - `fixed`: the path is broken, or the replayed response no longer shows it;
//! - `cannot-confirm`: neither can be shown (the code moved, the target is
//!   down, the evidence cannot be replayed).
//!
//! Retests are appended to the run's `retests.jsonl`, never written into
//! its findings: the original evidence and reports stay as they were. Each
//! one also becomes a lesson for the codebase (see `lessons`).

use clap::Args;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const VERDICTS: [&str; 3] = ["reproducible", "fixed", "cannot-confirm"];

/// The latest retest of each finding in a run, by fingerprint.
pub fn latest(run_dir: &Path) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let text = std::fs::read_to_string(run_dir.join("retests.jsonl")).unwrap_or_default();
    for l in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(l) {
            if let Some(fp) = v["fingerprint"].as_str() {
                out.insert(fp.to_string(), v);
            }
        }
    }
    out
}

/// Record a retest of the finding `which` (its id or fingerprint) in
/// `run_dir`, and its lesson in `lessons`.
#[allow(clippy::too_many_arguments)]
pub fn record(
    run_dir: &Path,
    which: &str,
    verdict: &str,
    why: &str,
    mode: &str,
    exchange: Option<&str>,
    commit: Option<&str>,
    lessons: &crate::deck::lessons::Lessons,
) -> Result<Value, String> {
    if !VERDICTS.contains(&verdict) {
        return Err(format!("the verdict is one of {}", VERDICTS.join(", ")));
    }
    if !["static", "live"].contains(&mode) {
        return Err("the mode is static or live".into());
    }
    let why = why.trim();
    if why.is_empty() || why.len() > 4000 {
        return Err(
            "say why (1 to 4000 characters): which guard, which line, which response".into(),
        );
    }
    if let Some(x) = exchange {
        if !crate::deck::traffic::valid_id(x) {
            return Err(format!(
                "{:?} is not an exchange id (see `indra traffic list`)",
                x
            ));
        }
    }
    if let Some(c) = commit {
        if c.is_empty() || c.len() > 64 || !c.chars().all(|x| x.is_ascii_hexdigit()) {
            return Err("the commit is a git hash".into());
        }
    }
    let findings = crate::findings::load(run_dir)?;
    let f = findings
        .iter()
        .find(|f| {
            crate::findings::field(f, "id") == which || crate::findings::fingerprint(f) == which
        })
        .ok_or_else(|| {
            format!(
                "no finding {:?} in {}",
                which,
                run_dir.join("verified.jsonl").display()
            )
        })?;
    let fp = crate::findings::fingerprint(f);
    let entry = json!({
        "finding": crate::findings::field(f, "id"), "fingerprint": fp,
        "title": crate::findings::field(f, "title"), "status": crate::findings::field(f, "status"),
        "verdict": verdict, "why": why, "mode": mode, "exchange": exchange, "commit": commit,
        "at": crate::mem::time::now_iso(),
    });
    {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.create(true).append(true);
        let mut file = o
            .open(run_dir.join("retests.jsonl"))
            .map_err(|e| format!("retests.jsonl: {}", e))?;
        writeln!(file, "{}", entry).map_err(|e| e.to_string())?;
    }
    let run = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let lesson = lessons.record(
        &crate::deck::lessons::target_of(run_dir),
        &run,
        f,
        "retest",
        verdict,
        why,
    )?;
    let mut out = entry;
    out["lesson"] = lesson.map(|l| json!(l.id)).unwrap_or(Value::Null);
    Ok(out)
}

/// `indra review retest`: record a retest's verdict (the sh-retest skill
/// calls it).
#[derive(Args, Debug)]
pub struct RetestArgs {
    /// The review run's folder (`<target>/.security-harness/<run>`)
    #[arg(long)]
    run: PathBuf,
    /// The finding: its id (SH-SQLI-001) or fingerprint
    #[arg(long)]
    finding: String,
    /// reproducible | fixed | cannot-confirm
    #[arg(long)]
    verdict: String,
    /// Why: the guard or line that now breaks the path, or what the response shows
    #[arg(long)]
    why: String,
    /// static (the code) or live (replayed traffic in an engagement)
    #[arg(long, default_value = "static")]
    mode: String,
    /// The replayed exchange, for a live retest
    #[arg(long)]
    exchange: Option<String>,
    /// The commit retested (default: the target's HEAD, when it is a git repository)
    #[arg(long)]
    commit: Option<String>,
}

pub fn cli(a: RetestArgs) -> Result<std::process::ExitCode, String> {
    let commit = a.commit.clone().or_else(|| {
        let target = crate::deck::lessons::target_of(&a.run);
        std::process::Command::new("git")
            .args(["-C", &target, "rev-parse", "HEAD"])
            .stdin(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    });
    let lessons = crate::deck::lessons::Lessons::open(&crate::paths::data_dir());
    let v = record(
        &a.run,
        &a.finding,
        &a.verdict,
        &a.why,
        &a.mode,
        a.exchange.as_deref(),
        commit.as_deref(),
        &lessons,
    )?;
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retests_append_and_teach() {
        let base = std::env::temp_dir().join(format!("indra-retest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let run = base.join("repo/.security-harness/run-1");
        std::fs::create_dir_all(&run).unwrap();
        let f = json!({ "id": "SH-SQLI-001", "class": "sqli", "title": "SQLi in search", "file": "app/db.py", "line": 9, "status": "verified" });
        std::fs::write(run.join("verified.jsonl"), f.to_string() + "\n").unwrap();
        let lessons = crate::deck::lessons::Lessons::open(&base.join("data"));
        assert!(record(
            &run,
            "SH-SQLI-001",
            "gone",
            "x",
            "static",
            None,
            None,
            &lessons
        )
        .is_err());
        assert!(record(
            &run,
            "SH-SQLI-001",
            "fixed",
            " ",
            "static",
            None,
            None,
            &lessons
        )
        .is_err());
        assert!(record(&run, "SH-NOPE", "fixed", "x", "static", None, None, &lessons).is_err());
        assert!(record(
            &run,
            "SH-SQLI-001",
            "fixed",
            "x",
            "live",
            Some("../x"),
            None,
            &lessons
        )
        .is_err());
        let v = record(
            &run,
            "SH-SQLI-001",
            "fixed",
            "search() now uses bound parameters (app/db.py:12)",
            "static",
            None,
            Some("abc123"),
            &lessons,
        )
        .unwrap();
        assert_eq!(v["verdict"], "fixed");
        assert!(
            v["lesson"].is_u64(),
            "a verified finding found fixed: a caution"
        );
        let fp = crate::findings::fingerprint(&f);
        let r = record(
            &run,
            &fp,
            "reproducible",
            "the parameter is concatenated again",
            "static",
            None,
            None,
            &lessons,
        )
        .unwrap();
        assert_eq!(r["finding"], "SH-SQLI-001");
        let l = latest(&run);
        assert_eq!(l[&fp]["verdict"], "reproducible", "the latest wins");
        let all = lessons.list();
        assert_eq!(
            all.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
            ["caution", "positive"]
        );
        // The original findings are untouched.
        assert_eq!(
            std::fs::read_to_string(run.join("verified.jsonl")).unwrap(),
            f.to_string() + "\n"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
