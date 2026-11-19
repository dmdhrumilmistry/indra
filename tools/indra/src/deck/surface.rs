//! The attack surface of a review run, as a graph, and which of its
//! vulnerabilities actually impact: those an attacker can reach.
//!
//! From the run's `codebase-map.json` (entry points, whether each needs a
//! login, dangerous sinks, dependencies and their known CVEs) and its
//! findings, it draws: Internet and Signed-in user -> entry points ->
//! components (the code's folders) -> findings, sinks and the dependencies
//! components use -> their CVEs. A vulnerability reached from Internet is
//! exposed; from Signed-in user only, exposed after login; otherwise
//! internal. Explicit verdicts win over the graph: a finding's
//! `reachability` (set by the verifier) and a CVE's (set by recon or
//! sh-attack-surface: `reachable`, `not_reachable` or `unknown`, with the
//! files that use the vulnerable code as `evidence`).

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

const INTERNET: &str = "x:internet";
const USER: &str = "x:user";

#[derive(Default)]
struct G {
    nodes: Vec<Value>,
    ids: HashMap<String, usize>,
    edges: Vec<(String, String, String)>,
}

impl G {
    fn node(&mut self, id: &str, kind: &str, title: &str, detail: Value) -> String {
        if !self.ids.contains_key(id) {
            self.ids.insert(id.to_string(), self.nodes.len());
            self.nodes
                .push(json!({ "id": id, "type": kind, "title": title, "detail": detail }));
        }
        id.to_string()
    }
    fn edge(&mut self, a: &str, b: &str, rel: &str) {
        let e = (a.to_string(), b.to_string(), rel.to_string());
        if a != b && !self.edges.contains(&e) {
            self.edges.push(e);
        }
    }
    /// Shortest paths from `start`: each reached node's predecessor.
    fn reach(&self, start: &str) -> HashMap<String, String> {
        let mut out = HashMap::new();
        let mut q = VecDeque::from([start.to_string()]);
        out.insert(start.to_string(), String::new());
        while let Some(n) = q.pop_front() {
            for (a, b, _) in &self.edges {
                if *a == n && !out.contains_key(b) {
                    out.insert(b.clone(), n.clone());
                    q.push_back(b.clone());
                }
            }
        }
        out
    }
    fn title(&self, id: &str) -> String {
        self.ids
            .get(id)
            .and_then(|i| self.nodes[*i]["title"].as_str())
            .unwrap_or(id)
            .to_string()
    }
}

fn s(v: &Value, k: &str) -> String {
    match &v[k] {
        Value::String(x) => x.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The folder a file is in: the component it belongs to.
fn component_of(file: &str) -> String {
    let f = file.trim().trim_start_matches("./").replace('\\', "/");
    match f.rsplit_once('/') {
        Some((dir, _)) if !dir.is_empty() => dir.to_string(),
        _ => "(root)".into(),
    }
}

fn file_of(loc: &str) -> String {
    // "app/db.py:88" -> "app/db.py"
    match loc.rsplit_once(':') {
        Some((f, l)) if l.chars().all(|c| c.is_ascii_digit()) => f.to_string(),
        _ => loc.to_string(),
    }
}

fn sev_rank(s: &str) -> u8 {
    match s.to_ascii_lowercase().as_str() {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    }
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn read_findings(dir: &Path) -> Vec<Value> {
    for f in ["verified.jsonl", "findings.jsonl"] {
        if let Ok(t) = std::fs::read_to_string(dir.join(f)) {
            return t
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter(|v| v.is_object())
                .collect();
        }
    }
    vec![]
}

/// The graph and the vulnerabilities, for a run directory. None when the
/// run has no codebase map.
pub fn build(dir: &Path) -> Option<Value> {
    let map = read_json(&dir.join("codebase-map.json"))?;
    let findings = read_findings(dir);
    let mut g = G::default();
    g.node(INTERNET, "exposure", "Internet (anyone)", json!({}));
    g.node(USER, "exposure", "Signed-in user", json!({}));

    // Entry points, and the components they lead into.
    let mut entries: Vec<(String, Value)> = Vec::new();
    for (i, e) in map["entry_points"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let label = format!("{} {}", s(e, "method"), s(e, "path"))
            .trim()
            .to_string();
        let label = if label.is_empty() {
            format!("{} {}", s(e, "kind"), s(e, "handler"))
        } else {
            label
        };
        let id = g.node(&format!("e:{}", i), "entry point", &label, e.clone());
        let auth = e["auth_required"].as_bool().unwrap_or(false);
        g.edge(
            if auth { USER } else { INTERNET },
            &id,
            if auth { "after login" } else { "reaches" },
        );
        let file = s(e, "file");
        if !file.is_empty() {
            let c = component_of(&file);
            let cid = g.node(&format!("c:{}", c), "component", &c, json!({ "folder": c }));
            g.edge(&id, &cid, "handled by");
        }
        entries.push((id, e.clone()));
    }
    // Dangerous sinks, inside their components.
    for (i, k) in map["dangerous_sinks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let file = s(k, "file");
        let id = g.node(
            &format!("s:{}", i),
            "sink",
            &format!("{} {}", s(k, "class"), s(k, "symbol")),
            k.clone(),
        );
        if !file.is_empty() {
            let c = component_of(&file);
            let cid = g.node(&format!("c:{}", c), "component", &c, json!({ "folder": c }));
            g.edge(&cid, &id, "calls");
        }
    }

    // Findings: from their entry point, else from the component they are in.
    let mut vulns: Vec<(String, Value, String)> = Vec::new(); // (node, item, explicit reach)
    for (i, f) in findings.iter().enumerate() {
        let status = s(f, "status");
        if status == "false-positive" {
            continue;
        }
        let fid = s(f, "id");
        let key = if fid.is_empty() {
            format!("f:#{}", i)
        } else {
            format!("f:{}", fid)
        };
        let title = format!(
            "{}{}",
            if fid.is_empty() {
                String::new()
            } else {
                format!("{}: ", fid)
            },
            s(f, "title")
        );
        let id = g.node(&key, "finding", &title, f.clone());
        let ep = s(f, "entry_point");
        let first_hop = f["data_flow"].as_array().and_then(|a| a.first()).map(|h| {
            file_of(&{
                let x = s(h, "file");
                if x.is_empty() {
                    s(h, "location")
                } else {
                    x
                }
            })
        });
        let mut linked = false;
        for (eid, e) in &entries {
            let path = s(e, "path");
            let hit = (!ep.is_empty()
                && (ep.contains(&path) && !path.is_empty()
                    || ep == format!("{}:{}", s(e, "file"), s(e, "line"))
                    || ep == s(e, "handler")))
                || (ep.is_empty()
                    && first_hop
                        .as_deref()
                        .map(|h| !h.is_empty() && h == s(e, "file"))
                        .unwrap_or(false));
            if hit {
                g.edge(eid, &id, "leads to");
                linked = true;
            }
        }
        if !linked {
            let file = s(f, "file");
            if !file.is_empty() {
                let c = component_of(&file);
                let cid = g.node(&format!("c:{}", c), "component", &c, json!({ "folder": c }));
                g.edge(&cid, &id, "contains");
            }
        }
        vulns.push((id, f.clone(), s(f, "reachability")));
    }

    // Dependencies with known CVEs: used by the components their evidence names.
    let mut by_pkg: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for c in map["known_cves"].as_array().cloned().unwrap_or_default() {
        by_pkg.entry(s(&c, "package")).or_default().push(c);
    }
    for (pkg, cves) in &by_pkg {
        let ver = s(&cves[0], "version");
        let did = g.node(
            &format!("d:{}", pkg),
            "dependency",
            format!("{} {}", pkg, ver).trim(),
            json!({ "package": pkg, "version": ver }),
        );
        for c in cves {
            let r = s(c, "reachability");
            let vid = g.node(
                &format!("v:{}:{}", pkg, s(c, "id")),
                "cve",
                &format!("{} ({})", s(c, "id"), pkg),
                c.clone(),
            );
            g.edge(&did, &vid, "has");
            if r != "not_reachable" {
                for ev in c["evidence"].as_array().cloned().unwrap_or_default() {
                    let file = file_of(ev.as_str().unwrap_or(""));
                    if !file.is_empty() {
                        let comp = component_of(&file);
                        let cid = g.node(
                            &format!("c:{}", comp),
                            "component",
                            &comp,
                            json!({ "folder": comp }),
                        );
                        g.edge(&cid, &did, "uses");
                    }
                }
            }
            vulns.push((vid, c.clone(), r));
        }
    }

    // Who reaches what.
    let from_internet = g.reach(INTERNET);
    let from_user = g.reach(USER);
    let path_to = |parents: &HashMap<String, String>, id: &str| -> Vec<String> {
        let mut out = vec![g.title(id)];
        let mut cur = id.to_string();
        while let Some(p) = parents.get(&cur).filter(|p| !p.is_empty()) {
            out.push(g.title(p));
            cur = p.clone();
        }
        out.reverse();
        out
    };
    let mut items: Vec<Value> = vulns
        .iter()
        .map(|(id, v, explicit)| {
            let is_cve = id.starts_with("v:");
            let (reach, path) = match explicit.as_str() {
                "unauthenticated" => ("internet", vec![]),
                "authenticated" => ("login", vec![]),
                "internal" => ("internal", vec![]),
                "unreachable" | "not_reachable" => ("not reachable", vec![]),
                _ if from_internet.contains_key(id) => ("internet", path_to(&from_internet, id)),
                _ if from_user.contains_key(id) => ("login", path_to(&from_user, id)),
                "reachable" => ("reachable", vec![]),
                _ if is_cve => ("unknown", vec![]),
                _ => ("internal", vec![]),
            };
            // With an explicit verdict, still show the graph's path if there is one.
            let path = if path.is_empty() {
                if from_internet.contains_key(id) {
                    path_to(&from_internet, id)
                } else if from_user.contains_key(id) {
                    path_to(&from_user, id)
                } else {
                    path
                }
            } else {
                path
            };
            let severity = s(v, "severity");
            let impacting = matches!(reach, "internet" | "login" | "reachable");
            json!({
                "node": id, "kind": if is_cve { "cve" } else { "finding" },
                "id": s(v, "id"),
                "title": if is_cve { format!("{} in {} {}", s(v, "id"), s(v, "package"), s(v, "version")) } else { s(v, "title") },
                "severity": severity, "status": s(v, "status"), "reach": reach, "impacting": impacting, "path": path,
                "file": s(v, "file"), "fixed_in": s(v, "fixed_in"),
            })
        })
        .collect();
    let order = |r: &str| match r {
        "internet" => 0,
        "login" => 1,
        "reachable" => 2,
        "unknown" => 3,
        "internal" => 4,
        _ => 5,
    };
    items.sort_by_key(|x| {
        (
            order(x["reach"].as_str().unwrap_or("")),
            sev_rank(x["severity"].as_str().unwrap_or("")),
        )
    });
    for (id, n) in g
        .nodes
        .iter_mut()
        .map(|n| (n["id"].as_str().unwrap_or("").to_string(), n))
    {
        if let Some(it) = items.iter().find(|x| x["node"] == id.as_str()) {
            n["impacting"] = it["impacting"].clone();
            n["reach"] = it["reach"].clone();
        }
    }
    let count = |r: &str| items.iter().filter(|x| x["reach"] == r).count();
    let exposed_entries = entries
        .iter()
        .filter(|(_, e)| !e["auth_required"].as_bool().unwrap_or(false))
        .count();
    Some(json!({
        "nodes": g.nodes,
        "edges": g.edges.iter().map(|(a, b, r)| json!({ "source": a, "target": b, "rel": r })).collect::<Vec<_>>(),
        "vulns": items,
        "summary": {
            "entry_points": entries.len(), "unauthenticated_entry_points": exposed_entries,
            "vulns": vulns.len(), "impacting": items.iter().filter(|x| x["impacting"] == true).count(),
            "internet": count("internet"), "login": count("login"), "reachable": count("reachable"),
            "internal": count("internal"), "unknown": count("unknown"), "not_reachable": count("not reachable"),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_follows_the_graph_and_explicit_verdicts() {
        let dir = std::env::temp_dir().join(format!("indra-surface-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("codebase-map.json"), json!({
            "entry_points": [
                { "kind": "http-route", "method": "POST", "path": "/api/login", "file": "app/auth/views.py", "line": 20, "auth_required": false },
                { "kind": "http-route", "method": "GET", "path": "/api/invoices/{id}", "file": "app/billing/views.py", "line": 31, "auth_required": true }
            ],
            "dangerous_sinks": [{ "class": "sqli", "symbol": "cursor.execute", "file": "app/auth/db.py", "line": 88 }],
            "known_cves": [
                { "package": "pyyaml", "version": "5.3", "id": "CVE-2020-14343", "severity": "critical", "reachability": "reachable", "evidence": ["app/auth/config.py:12"] },
                { "package": "lxml", "version": "4.6", "id": "CVE-2021-43818", "severity": "high", "reachability": "not_reachable" },
                { "package": "jinja2", "version": "2.10", "id": "CVE-2019-10906", "severity": "high" }
            ]
        }).to_string()).unwrap();
        let lines = [
            json!({ "id": "SH-SQLI-001", "title": "SQL injection in login", "severity": "high", "status": "verified", "file": "app/auth/db.py", "line": 88,
                    "data_flow": [{ "file": "app/auth/views.py", "line": 22 }] }),
            json!({ "id": "SH-AC-002", "title": "Invoice IDOR", "severity": "high", "status": "verified", "file": "app/billing/views.py", "entry_point": "GET /api/invoices/{id}" }),
            json!({ "id": "SH-XSS-003", "title": "Admin XSS", "severity": "medium", "status": "verified", "file": "tools/admin/render.py" }),
            json!({ "id": "SH-SSRF-004", "title": "Fetcher", "severity": "high", "status": "false-positive", "file": "app/net/f.py" }),
        ];
        std::fs::write(
            dir.join("verified.jsonl"),
            lines
                .iter()
                .map(|l| l.to_string() + "\n")
                .collect::<String>(),
        )
        .unwrap();
        let r = build(&dir).unwrap();
        let reach = |id: &str| {
            r["vulns"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["id"] == id)
                .map(|v| v["reach"].as_str().unwrap().to_string())
        };
        assert_eq!(
            reach("SH-SQLI-001").as_deref(),
            Some("internet"),
            "data flow starts at the public login"
        );
        assert_eq!(
            reach("SH-AC-002").as_deref(),
            Some("login"),
            "behind a login"
        );
        assert_eq!(
            reach("SH-XSS-003").as_deref(),
            Some("internal"),
            "no entry point reaches it"
        );
        assert_eq!(reach("SH-SSRF-004"), None, "false positives are left out");
        assert_eq!(
            reach("CVE-2020-14343").as_deref(),
            Some("internet"),
            "used by the component the login leads into"
        );
        assert_eq!(reach("CVE-2021-43818").as_deref(), Some("not reachable"));
        assert_eq!(reach("CVE-2019-10906").as_deref(), Some("unknown"));
        assert_eq!(r["summary"]["impacting"], 3);
        assert_eq!(r["summary"]["unauthenticated_entry_points"], 1);
        // Exposed first, and each with the path that reaches it.
        assert_eq!(r["vulns"][0]["reach"], "internet");
        let sqli = r["vulns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == "SH-SQLI-001")
            .unwrap();
        assert_eq!(
            sqli["path"],
            json!([
                "Internet (anyone)",
                "POST /api/login",
                "SH-SQLI-001: SQL injection in login"
            ])
        );
        assert!(build(&dir.join("nope")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
