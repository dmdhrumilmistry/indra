//! A finished review run's findings (`verified.jsonl`), shared by the tools
//! that turn them into memory notes and publish plans.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Collapse whitespace and cut to `n` characters (with "..." when cut).
pub fn clip(s: &str, n: usize) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n {
        s
    } else {
        format!(
            "{}...",
            s.chars().take(n.saturating_sub(3)).collect::<String>()
        )
    }
}

/// A JSON value as text the way the reports print it (strings bare, null empty).
pub fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `f[key]` as text, or "" when absent or null.
pub fn field(f: &Value, key: &str) -> String {
    f.get(key).map(text).unwrap_or_default()
}

/// A list field (`cwe`, `owasp`, `chained_with`) as strings.
pub fn list(f: &Value, key: &str) -> Vec<String> {
    f.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().map(text).collect())
        .unwrap_or_default()
}

/// Truthy in the Python sense: present, not null, not empty, not false or 0.
pub fn truthy(f: &Value, key: &str) -> bool {
    match f.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64().map(|x| x != 0.0).unwrap_or(true),
    }
}

/// The finding's 12-hex fingerprint: its own when it has one, else derived
/// the same way sh-pr-review derives it (minus the snippet).
pub fn fingerprint(f: &Value) -> String {
    let fp = field(f, "fingerprint").trim().to_lowercase();
    if fp.len() >= 8 && fp.chars().all(|c| c.is_ascii_hexdigit()) {
        return fp.chars().take(12).collect();
    }
    let cwe = list(f, "cwe").join(",");
    let path = field(f, "file")
        .replace('\\', "/")
        .trim_start_matches(['.', '/'])
        .to_lowercase();
    let key = [
        cwe,
        path,
        field(f, "class"),
        field(f, "title").to_lowercase(),
    ]
    .join("|");
    let h = Sha256::digest(key.as_bytes());
    h.iter().map(|b| format!("{:02x}", b)).collect::<String>()[..12].to_string()
}

/// Every finding in `<run_dir>/verified.jsonl`; bad lines are reported and skipped.
pub fn load(run_dir: &Path) -> Result<Vec<Value>, String> {
    let path = run_dir.join("verified.jsonl");
    if !path.is_file() {
        return Err(format!(
            "no verified.jsonl in {} (run the verify stage first)",
            run_dir.display()
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => out.push(v),
            Err(e) => eprintln!("skipping verified.jsonl:{}: {}", n + 1, e),
        }
    }
    Ok(out)
}

/// The run id: the run directory's name.
pub fn run_id(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fingerprints_match_the_review_recipe() {
        assert_eq!(
            fingerprint(&json!({ "fingerprint": "ABCDEF0123456789" })),
            "abcdef012345"
        );
        // Derived: sha256("CWE-89|app/db.py|sqli|raw query")[..12]
        let f = json!({ "cwe": ["CWE-89"], "file": "./app/db.py", "class": "sqli", "title": "Raw query" });
        let want = Sha256::digest(b"CWE-89|app/db.py|sqli|raw query");
        let want: String = want.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(fingerprint(&f), want[..12]);
        assert_eq!(clip("a  b\n c", 500), "a b c");
        assert_eq!(clip("abcdefghij", 6), "abc...");
    }
}
