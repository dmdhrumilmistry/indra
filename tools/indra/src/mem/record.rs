//! `indra mem record-run`: record a finished security review run into a memory
//! vault.
//!
//! Turning verified.jsonl into notes is exact bookkeeping, so it is done here
//! and not by a model: a finding is never "remembered" with a different title,
//! a false positive is never silently dropped, and a re-run of the same issue
//! lands on the same note instead of a near-duplicate.
//!
//! It writes one hub note per vulnerability class (type vuln-class) and per
//! affected file (type file); verified and needs-runtime findings (type
//! finding, instance-of the class hub, located-in the file hub); false
//! positives as learnings (tag false-positive, refines the class hub), which
//! the next run's hunters read so they stop re-reporting a safe pattern; and
//! chains-with relations between findings of the same run. A finding seen
//! again is matched by its fingerprint (an alias) and gets a "seen again" line.
//!
//! It leaves out `evidence`, `payload`, `poc` and `data_flow`: evidence for a
//! secrets finding *is* the secret, and payloads do not belong in a long-lived
//! store.

use crate::findings::{clip, field, fingerprint, list, load, run_id, truthy};
use crate::mem::cli::{add_note, append_to, link_notes, NewNote};
use crate::mem::vault::Vault;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const MAX_TEXT: usize = 500;

fn class_name(cls: &str) -> String {
    match cls {
        "access-control" => "Broken access control",
        "sqli" => "SQL injection",
        "xss" => "Cross-site scripting",
        "ssrf" => "Server-side request forgery",
        "injection" => "Command and code injection",
        "auth" => "Authentication and session flaws",
        "deserialization" => "Insecure deserialization",
        "path-traversal" => "Path traversal",
        "secrets" => "Hardcoded secrets",
        "csrf" => "Cross-site request forgery",
        "xxe" => "XML external entities",
        "open-redirect" => "Open redirect",
        "crypto" => "Cryptographic misuse",
        "race-conditions" => "Race conditions",
        "file-upload" => "Unrestricted file upload",
        "dependency" => "Vulnerable dependencies",
        other => other,
    }
    .to_string()
}

fn or(f: &Value, key: &str, dflt: &str) -> String {
    if truthy(f, key) {
        field(f, key)
    } else {
        dflt.to_string()
    }
}

fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || "-_./=:".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Writes to the vault, or with `dry_run`, prints the equivalent commands.
struct Mem<'a> {
    vault: &'a Path,
    dry: bool,
}

impl Mem<'_> {
    fn show(&self, args: &[String]) {
        let mut all = vec![
            "mem".to_string(),
            "--vault".into(),
            self.vault.display().to_string(),
        ];
        all.extend(args.iter().cloned());
        println!(
            "indra {}",
            all.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
        );
    }

    fn exists(&self, name: &str) -> bool {
        !self.dry
            && Vault::open(self.vault)
                .map(|v| v.resolve(name).is_some())
                .unwrap_or(false)
    }

    fn add(&self, n: NewNote) -> Result<(), String> {
        if self.dry {
            let mut a = vec!["add".to_string(), n.title.clone()];
            if let Some(k) = &n.kind {
                a.extend(["-t".into(), k.clone()]);
            }
            for t in &n.tags {
                a.extend(["--tag".into(), t.clone()]);
            }
            for al in &n.aliases {
                a.extend(["--alias".into(), al.clone()]);
            }
            for (r, t) in &n.rels {
                a.extend(["--rel".into(), format!("{}={}", r, t)]);
            }
            if let Some(s) = &n.source {
                a.extend(["--source".into(), s.clone()]);
            }
            if let Some(f) = &n.folder {
                a.extend(["--folder".into(), f.clone()]);
            }
            if !n.body.is_empty() {
                a.push(format!("--body={}", n.body));
            }
            a.push("--upsert".into());
            self.show(&a);
            return Ok(());
        }
        add_note(self.vault, &n).map(|_| ())
    }

    fn append(&self, note: &str, text: &str) -> Result<(), String> {
        if self.dry {
            self.show(&["append".into(), note.into(), "--".into(), text.into()]);
            return Ok(());
        }
        append_to(self.vault, note, text).map(|_| ())
    }

    fn link(&self, a: &str, b: &str) -> Result<(), String> {
        if self.dry {
            self.show(&[
                "link".into(),
                a.into(),
                b.into(),
                "--rel".into(),
                "chains-with".into(),
            ]);
            return Ok(());
        }
        link_notes(self.vault, a, b, "chains-with").map(|_| ())
    }
}

/// Record `run_dir` into `vault`. Returns the counts as JSON.
pub fn record(run_dir: &Path, vault: &Path, dry_run: bool) -> Result<Value, String> {
    let run = run_id(run_dir);
    let findings = load(run_dir)?;
    let m = Mem {
        vault,
        dry: dry_run,
    };
    let mut made: HashSet<String> = HashSet::new();
    let mut by_id: HashMap<String, String> = HashMap::new();
    let (mut new, mut again, mut fps, mut skipped) = (0, 0, 0, 0);
    let id_of = |f: &Value| field(f, "id");

    for f in &findings {
        let status = field(f, "status");
        if !matches!(
            status.as_str(),
            "verified" | "needs-runtime" | "false-positive"
        ) {
            skipped += 1;
            continue;
        }
        let cls = or(f, "class", "unknown");
        let alias = format!("fp-{}", fingerprint(f));
        let loc = format!("{}:{}", or(f, "file", "?"), or(f, "line", "?"));
        let ver = f.get("verification").cloned().unwrap_or(Value::Null);

        if m.exists(&alias) {
            m.append(
                &alias,
                &format!("- Seen again in {} (status: {}, {}).", run, status, loc),
            )?;
            by_id.insert(id_of(f), alias);
            again += 1;
            continue;
        }

        // The class hub.
        let hub = class_name(&cls);
        if made.insert(hub.clone()) {
            m.add(NewNote {
                title: hub.clone(),
                kind: Some("vuln-class".into()),
                tags: vec![cls.clone()],
                aliases: vec![cls.clone()],
                upsert: true,
                ..Default::default()
            })?;
        }
        // The file hub.
        let path = field(f, "file").replace('\\', "/").trim().to_string();
        let file_hub = if path.is_empty() {
            None
        } else {
            let title = format!("file: {}", path);
            if made.insert(title.clone()) {
                m.add(NewNote {
                    title: title.clone(),
                    kind: Some("file".into()),
                    aliases: vec![path.clone()],
                    folder: Some("files".into()),
                    body: "Code location seen in security reviews. Backlinks list what was found here.".into(),
                    upsert: true,
                    ..Default::default()
                })?;
            }
            Some(title)
        };

        let mut lines = vec![
            format!("- status: {}", status),
            format!("- location: {}", loc),
            format!("- first seen: {} ({})", run, or(f, "id", "no id")),
        ];
        if truthy(f, "cwe") {
            lines.push(format!("- cwe: {}", list(f, "cwe").join(", ")));
        }
        if truthy(&ver, "rationale") {
            lines.push(format!(
                "- verifier: {}",
                clip(&field(&ver, "rationale"), MAX_TEXT)
            ));
        }
        let what = if truthy(f, "title") {
            field(f, "title")
        } else {
            loc.clone()
        };
        let mut note = if status == "false-positive" {
            lines.extend([
                String::new(),
                "Hunters: this pattern was reviewed and is not exploitable. Do not report it again unless the guard named above changes.".into(),
            ]);
            fps += 1;
            NewNote {
                title: format!("False positive: {}", clip(&what, 90)),
                kind: Some("learning".into()),
                tags: vec![cls.clone(), "false-positive".into()],
                rels: vec![("refines".into(), hub.clone())],
                folder: Some("learnings".into()),
                body: lines.join("\n"),
                ..Default::default()
            }
        } else {
            let sev = or(f, "severity", "unknown");
            if truthy(f, "why_it_matters") {
                lines.push(format!(
                    "- impact: {}",
                    clip(&field(f, "why_it_matters"), MAX_TEXT)
                ));
            }
            if truthy(f, "mitigation") {
                lines.push(format!(
                    "- mitigation: {}",
                    clip(&field(f, "mitigation"), MAX_TEXT)
                ));
            }
            new += 1;
            NewNote {
                title: format!("{}: {}", or(f, "id", &cls.to_uppercase()), clip(&what, 90)),
                kind: Some("finding".into()),
                tags: vec![cls.clone(), sev, status.clone()],
                rels: vec![("instance-of".into(), hub.clone())],
                folder: Some("findings".into()),
                body: lines.join("\n"),
                ..Default::default()
            }
        };
        note.aliases = vec![alias.clone()];
        note.source = Some(run.clone());
        if let Some(fh) = file_hub {
            note.rels.push(("located-in".into(), fh));
        }
        // If an earlier note in this run already took the title, merge into it.
        note.upsert = true;
        m.add(note)?;
        by_id.insert(id_of(f), alias);
    }

    for f in &findings {
        let Some(a) = by_id.get(&id_of(f)) else {
            continue;
        };
        for other in list(f, "chained_with") {
            // chained_with is symmetric; one relation per pair is enough.
            if let Some(b) = by_id.get(&other) {
                if a < b {
                    m.link(a, b)?;
                }
            }
        }
    }
    Ok(json!({
        "vault": vault.display().to_string(),
        "new": new, "seen-again": again, "false-positive": fps, "skipped": skipped,
    }))
}
