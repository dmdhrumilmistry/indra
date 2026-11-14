//! `indra publish plan`: turn a finished review run into a publish plan for
//! Jira, Confluence, GitHub Issues or Notion.
//!
//! What leaves the machine must be exact and safe, so this decides
//! deterministically which findings are published, what each record says, and
//! how it is de-duplicated; an agent only carries the plan out with the
//! destination's MCP tools.
//!
//! A record never contains `evidence`, `payload`, `poc` or `data_flow`:
//! evidence for a secrets finding *is* the secret, and exploit payloads do not
//! belong in a ticket tracker. Every record carries `indra-fingerprint: <12 hex>`
//! (the fingerprint sh-pr-review and sh-memory use): search the destination for
//! it before creating anything, and comment or update instead when it exists.

use crate::findings::{clip, field, fingerprint, list, load, run_id, truthy};
use clap::{Args, ValueEnum};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::ExitCode;

const SEVERITIES: [&str; 5] = ["info", "low", "medium", "high", "critical"];
const NEVER: [&str; 4] = ["evidence", "payload", "poc", "data_flow"];
const MAX_TEXT: usize = 700;

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum Dest {
    Jira,
    Github,
    Confluence,
    Notion,
}

impl Dest {
    fn name(self) -> &'static str {
        match self {
            Dest::Jira => "jira",
            Dest::Github => "github",
            Dest::Confluence => "confluence",
            Dest::Notion => "notion",
        }
    }
}

#[derive(Args)]
pub struct PublishArgs {
    #[command(subcommand)]
    cmd: PublishCmd,
}

#[derive(clap::Subcommand)]
enum PublishCmd {
    /// Print the publish plan for a finished run as JSON
    Plan {
        /// The run directory (<TARGET>/.security-harness/<run-id>)
        run_dir: PathBuf,
        #[arg(long, value_enum)]
        to: Dest,
        /// info | low | medium | high | critical
        #[arg(long, default_value = "medium")]
        min_severity: String,
        /// Comma-separated: verified,needs-runtime
        #[arg(long, default_value = "verified")]
        status: String,
        #[arg(long, default_value_t = 25)]
        max: usize,
    },
}

fn or(f: &Value, key: &str, dflt: &str) -> String {
    if truthy(f, key) {
        field(f, key)
    } else {
        dflt.to_string()
    }
}

fn sev_index(s: &str) -> Option<usize> {
    SEVERITIES.iter().position(|x| *x == s)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// The record text: Markdown, which every destination's MCP server accepts.
fn body_markdown(f: &Value, fp: &str, run: &str) -> String {
    let sev = or(f, "severity", "unknown");
    let ver = f.get("verification").cloned().unwrap_or(Value::Null);
    let mut lines = vec![
        format!("**Severity:** {}  ", capitalize(&sev)),
        format!("**Status:** {}  ", or(f, "status", "unknown")),
        format!(
            "**Location:** `{}:{}`  ",
            or(f, "file", "?"),
            or(f, "line", "?")
        ),
    ];
    if truthy(f, "cwe") {
        lines.push(format!("**CWE:** {}  ", list(f, "cwe").join(", ")));
    }
    if truthy(f, "owasp") {
        lines.push(format!("**OWASP:** {}  ", list(f, "owasp").join(", ")));
    }
    if f.get("confidence").map(|v| !v.is_null()).unwrap_or(false) {
        lines.push(format!("**Confidence:** {}%  ", field(f, "confidence")));
    }
    lines.push(String::new());
    if truthy(f, "why_it_matters") {
        lines.extend([
            "### Impact".into(),
            clip(&field(f, "why_it_matters"), MAX_TEXT),
            String::new(),
        ]);
    }
    if truthy(&ver, "rationale") {
        lines.extend([
            "### Why it was confirmed".into(),
            clip(&field(&ver, "rationale"), MAX_TEXT),
            String::new(),
        ]);
    }
    if truthy(f, "mitigation") {
        lines.extend([
            "### Fix".into(),
            clip(&field(f, "mitigation"), MAX_TEXT),
            String::new(),
        ]);
    }
    if truthy(f, "chained_with") {
        lines.extend([
            format!("Combines with: {}", list(f, "chained_with").join(", ")),
            String::new(),
        ]);
    }
    lines.extend([
        "---".into(),
        format!(
            "Found by an indra security review ({}, {}). Exploit details stay in the local run directory.",
            run,
            or(f, "id", "no id")
        ),
        String::new(),
        format!("indra-fingerprint: {}", fp),
    ]);
    lines.join("\n")
}

/// The plan for `run_dir`.
pub fn plan(
    run_dir: &std::path::Path,
    to: Dest,
    min_sev: &str,
    statuses: &[String],
    limit: usize,
) -> Result<Value, String> {
    let run = run_id(run_dir);
    let floor = sev_index(min_sev).ok_or("unknown --min-severity")?;
    let sev_of = |f: &Value| or(f, "severity", "info").to_lowercase();
    let mut picked: Vec<Value> = load(run_dir)?
        .into_iter()
        .filter(|f| statuses.contains(&field(f, "status")))
        .filter(|f| sev_index(&sev_of(f)).map(|i| i >= floor).unwrap_or(false))
        .collect();
    // Python's str(None) is "None": keep the same order for findings without an id.
    let id_key = |f: &Value| match f.get("id") {
        None | Some(Value::Null) => "None".to_string(),
        Some(v) => crate::findings::text(v),
    };
    picked.sort_by(|a, b| {
        sev_index(&sev_of(b))
            .cmp(&sev_index(&sev_of(a)))
            .then_with(|| id_key(a).cmp(&id_key(b)))
    });
    let held_back = picked.len().saturating_sub(limit);
    picked.truncate(limit);

    let mut records = Vec::new();
    for f in &picked {
        let fp = fingerprint(f);
        let sev = sev_of(f);
        let cls = or(f, "class", "security");
        let what = if truthy(f, "title") {
            field(f, "title")
        } else {
            or(f, "file", "finding")
        };
        let title = format!("[{}] {}: {}", sev.to_uppercase(), cls, clip(&what, 120));
        let body = body_markdown(f, &fp, &run);
        let mut rec = json!({
            "fingerprint": fp,
            "dedupe_query": format!("indra-fingerprint: {}", fp),
            "finding_id": f.get("id").cloned().unwrap_or(Value::Null),
            "severity": sev,
            "title": title,
            "body_markdown": body,
            "labels": ["security", "indra", cls, format!("severity:{}", sev)],
        });
        if to == Dest::Jira {
            let priority = match sev.as_str() {
                "critical" => "Highest",
                "high" => "High",
                "medium" => "Medium",
                "low" => "Low",
                "info" => "Lowest",
                _ => "Medium",
            };
            rec["jira"] = json!({ "issue_type": "Bug", "priority": priority });
        }
        // A defensive check: nothing sensitive may leave through the plan.
        let blob = format!("{}\n{}", rec, [title_of(&rec), body_of(&rec)].join("\n"));
        for k in NEVER {
            if let Some(Value::String(v)) = f.get(k) {
                if v.chars().count() >= 8 && blob.contains(v.as_str()) {
                    return Err(format!(
                        "refusing to publish: {} of {} leaked into the record",
                        k,
                        field(f, "id")
                    ));
                }
            }
        }
        records.push(rec);
    }
    let mut sorted_status = statuses.to_vec();
    sorted_status.sort();
    let mut out = json!({
        "destination": to.name(),
        "run": run,
        "records": records,
        "held_back_over_limit": held_back,
        "filters": { "min_severity": min_sev, "statuses": sorted_status, "max": limit },
    });
    if matches!(to, Dest::Confluence | Dest::Notion) {
        // One page per run, with a section per finding, is easier to read there.
        let recs = out["records"].as_array().cloned().unwrap_or_default();
        let sections: Vec<String> = recs
            .iter()
            .map(|r| format!("## {}\n\n{}", title_of(r), body_of(r)))
            .collect();
        let mut counts: Vec<(usize, &str)> = Vec::new();
        for s in SEVERITIES.iter().rev() {
            let n = recs.iter().filter(|r| r["severity"] == *s).count();
            if n > 0 {
                counts.push((n, s));
            }
        }
        let summary = if counts.is_empty() {
            "no findings at this threshold".to_string()
        } else {
            counts
                .iter()
                .map(|(n, s)| format!("{} {}", n, s))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out["page"] = json!({
            "title": format!("Security review {}", run),
            "dedupe_query": format!("indra-run: {}", run),
            "body_markdown": format!("Summary: {}.\n\n{}\n\nindra-run: {}\n", summary, sections.join("\n\n"), run),
        });
    }
    Ok(out)
}

fn title_of(r: &Value) -> String {
    r["title"].as_str().unwrap_or("").to_string()
}

fn body_of(r: &Value) -> String {
    r["body_markdown"].as_str().unwrap_or("").to_string()
}

pub fn run(a: PublishArgs) -> Result<ExitCode, String> {
    match a.cmd {
        PublishCmd::Plan {
            run_dir,
            to,
            min_severity,
            status,
            max,
        } => {
            if sev_index(&min_severity).is_none() {
                eprintln!("--min-severity must be one of {}", SEVERITIES.join(", "));
                return Ok(ExitCode::from(2));
            }
            let mut statuses: Vec<String> = status
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            statuses.dedup();
            let bad: Vec<&String> = statuses
                .iter()
                .filter(|s| !matches!(s.as_str(), "verified" | "needs-runtime"))
                .collect();
            if !bad.is_empty() {
                let mut b: Vec<&str> = bad.iter().map(|s| s.as_str()).collect();
                b.sort();
                eprintln!(
                    "unsupported status: {} (false positives are never published)",
                    b.join(", ")
                );
                return Ok(ExitCode::from(2));
            }
            let dir = std::path::absolute(&run_dir).map_err(|e| e.to_string())?;
            let p = plan(&dir, to, &min_severity, &statuses, max.max(1))
                .map_err(|e| format!("publish plan: {}", e))?;
            println!("{}", serde_json::to_string_pretty(&p).unwrap_or_default());
            Ok(ExitCode::SUCCESS)
        }
    }
}
