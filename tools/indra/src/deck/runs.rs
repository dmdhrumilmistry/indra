//! Monitoring runs: a run is a directory under a target (for the security
//! harness, `<target>/.security-harness/run-*`). Progress comes from which
//! stage files exist, results from a JSONL file, activity from timestamps.

use crate::deck::config::Harness;
use crate::deck::discover::{glob, mtime};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A run counts as live while something in it changed this recently.
pub const ACTIVE_SECS: u64 = 120;

#[derive(Debug, Clone, Serialize)]
pub struct StageState {
    pub name: String,
    pub agent: Option<String>,
    /// done | running | pending
    pub state: &'static str,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Counts {
    pub total: usize,
    pub by_status: BTreeMap<String, usize>,
    pub by_severity: BTreeMap<String, usize>,
    /// Per `group_by` value: the agent or class that produced the result.
    pub by_group: BTreeMap<String, usize>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileInfo {
    pub rel: String,
    pub size: u64,
    pub modified: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Run {
    /// `<target index>:<directory name>`; stable while the config is.
    pub key: String,
    pub name: String,
    pub target: String,
    pub path: String,
    pub modified: u64,
    pub active: bool,
    pub stages: Vec<StageState>,
    pub counts: Counts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<FileInfo>>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Run directories of a harness, newest first.
pub fn run_dirs(h: &Harness) -> Vec<(String, PathBuf)> {
    let Some(pattern) = &h.runs else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (ti, t) in h.targets.iter().enumerate() {
        for d in glob(t, pattern) {
            if d.is_dir() {
                let name = d
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push((format!("{}:{}", ti, name), d));
            }
        }
    }
    out
}

pub fn list(h: &Harness, limit: usize) -> Vec<Run> {
    let mut runs: Vec<Run> = run_dirs(h)
        .into_iter()
        .map(|(key, dir)| summarize(h, &key, &dir, false))
        .collect();
    runs.sort_by(|a, b| b.modified.cmp(&a.modified).then(b.name.cmp(&a.name)));
    runs.truncate(limit);
    runs
}

pub fn find(h: &Harness, key: &str) -> Option<PathBuf> {
    run_dirs(h)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, d)| d)
}

fn files_in(dir: &Path) -> Vec<FileInfo> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() && depth < 4 {
                stack.push((p, depth + 1));
            } else if ft.is_file() {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(FileInfo {
                    rel,
                    size,
                    modified: mtime(&p),
                });
            }
        }
        if out.len() > 2000 {
            break;
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

pub fn summarize(h: &Harness, key: &str, dir: &Path, with_files: bool) -> Run {
    let files = files_in(dir);
    let modified = files
        .iter()
        .map(|f| f.modified)
        .max()
        .unwrap_or_else(|| mtime(dir));
    let active = now().saturating_sub(modified) <= ACTIVE_SECS;

    // A stage is done once its file exists. While the run is live, the stage
    // whose file changed last is the one still being written.
    let mut stages: Vec<StageState> = Vec::new();
    let mut latest: Option<(usize, u64)> = None;
    for (i, s) in h.stages.iter().enumerate() {
        let hit = s
            .done_when
            .as_ref()
            .map(|g| glob(dir, g))
            .unwrap_or_default();
        let t = hit.iter().map(|p| mtime(p)).max();
        if let Some(t) = t {
            if latest.map(|(_, lt)| t >= lt).unwrap_or(true) {
                latest = Some((i, t));
            }
        }
        stages.push(StageState {
            name: s.name.clone(),
            agent: s.agent.clone(),
            state: if t.is_some() { "done" } else { "pending" },
        });
    }
    if active {
        match latest {
            Some((i, _)) if i + 1 < stages.len() => stages[i].state = "running",
            None if !stages.is_empty() => stages[0].state = "running",
            _ => {}
        }
    }

    Run {
        key: key.to_string(),
        name: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        target: dir
            .ancestors()
            .find(|a| h.targets.iter().any(|t| t == a))
            .map(|t| t.display().to_string())
            .unwrap_or_default(),
        path: dir.display().to_string(),
        modified,
        active,
        stages,
        counts: count_results(h, dir),
        files: with_files.then_some(files),
    }
}

fn count_results(h: &Harness, dir: &Path) -> Counts {
    let Some(file) = h.findings.iter().map(|f| dir.join(f)).find(|p| p.is_file()) else {
        return Counts::default();
    };
    let mut c = Counts {
        source: file.file_name().map(|n| n.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let Ok(text) = std::fs::read_to_string(&file) else {
        return c;
    };
    let field = |v: &serde_json::Value, k: &str| -> Option<String> {
        v.get(k).and_then(|x| x.as_str()).map(|s| s.to_string())
    };
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        c.total += 1;
        if let Some(s) = field(&v, "status") {
            *c.by_status.entry(s).or_insert(0) += 1;
        }
        if let Some(s) = field(&v, "severity") {
            *c.by_severity.entry(s).or_insert(0) += 1;
        }
        if let Some(g) = h.group_by.as_deref().and_then(|g| field(&v, g)) {
            *c.by_group.entry(g).or_insert(0) += 1;
        }
    }
    c
}

/// Resolve `rel` inside `dir`, refusing anything that escapes it (`..`,
/// absolute paths, or a symlink pointing outside).
pub fn confined(dir: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.replace('\\', "/");
    if rel.is_empty()
        || rel.starts_with('/')
        || rel.split('/').any(|s| s == ".." || s.contains(':'))
    {
        return None;
    }
    let base = std::fs::canonicalize(dir).ok()?;
    let p = std::fs::canonicalize(base.join(&rel)).ok()?;
    (p.starts_with(&base) && p.is_file()).then_some(p)
}

/// The last `max_bytes` of a file, cut at a line start, as lossy UTF-8.
pub fn tail(p: &Path, max_bytes: u64) -> std::io::Result<(String, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(p)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.take(max_bytes).read_to_end(&mut buf)?;
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        if let Some(i) = s.find('\n') {
            s = s[i + 1..].to_string();
        }
    }
    Ok((s, start > 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::config::security_harness_profile;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("indra-runs-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn stages_counts_and_confinement() {
        let t = tmp("a");
        let run = t.join(".security-harness").join("run-1");
        std::fs::create_dir_all(run.join("reports")).unwrap();
        std::fs::write(run.join("recon.md"), "x").unwrap();
        std::fs::write(
            run.join("findings.jsonl"),
            "{\"class\":\"sqli\",\"status\":\"candidate\",\"severity\":\"high\"}\n\n{\"class\":\"xss\",\"status\":\"candidate\",\"severity\":\"low\"}\nnot json\n",
        )
        .unwrap();
        let h = security_harness_profile(Path::new("/nowhere"), std::slice::from_ref(&t));
        let runs = list(&h, 10);
        assert_eq!(runs.len(), 1);
        let r = &runs[0];
        assert_eq!(r.key, "0:run-1");
        assert!(r.active);
        let states: Vec<&str> = r.stages.iter().map(|s| s.state).collect();
        assert_eq!(
            states,
            vec!["done", "running", "pending", "pending", "pending"]
        );
        assert_eq!(r.counts.total, 2);
        assert_eq!(r.counts.by_group["sqli"], 1);
        assert_eq!(r.counts.source.as_deref(), Some("findings.jsonl"));

        assert!(confined(&run, "recon.md").is_some());
        assert!(confined(&run, "../run-1/recon.md").is_none());
        assert!(confined(&run, "/etc/passwd").is_none());
        assert!(confined(&run, "missing.md").is_none());
        assert!(
            confined(&run, "reports").is_none(),
            "directories are not files"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", run.join("link")).unwrap();
            assert!(
                confined(&run, "link").is_none(),
                "symlink escaping the run is refused"
            );
        }
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn tail_cuts_at_a_line() {
        let t = tmp("b");
        let f = t.join("log");
        std::fs::write(&f, "first line\nsecond line\nthird\n").unwrap();
        let (s, cut) = tail(&f, 12).unwrap();
        assert!(cut);
        assert_eq!(s, "third\n");
        let (s, cut) = tail(&f, 1000).unwrap();
        assert!(!cut && s.starts_with("first"));
        let _ = std::fs::remove_dir_all(&t);
    }
}
