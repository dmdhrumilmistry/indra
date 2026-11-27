//! Recorded traffic: what the recording proxy kept for an engagement, and
//! findings that cite it as evidence.
//!
//! A finding cites an exchange in `evidence_traffic`:
//!
//! ```json
//! "evidence_traffic": [
//!   { "engagement": "abcd1234", "exchange": "1764200000000-3", "role": "baseline" },
//!   { "engagement": "abcd1234", "exchange": "1764200000000-7", "role": "proof",
//!     "quote": "\"role\":\"admin\"", "sha256": "<the response body's SHA-256>" }
//! ]
//! ```
//!
//! Roles: `baseline` (the normal behaviour), `proof` (the vulnerability),
//! `verification` (more confirmation), `supporting` (the default). `indra
//! traffic verify` checks each citation: the exchange exists, its kept body
//! still has the SHA-256 the citation names, and the quote appears
//! verbatim in its response (or request). A finding is evidence-verified
//! only when every citation holds and at least one is a `proof` with a
//! quote. IDs are never guessed: they come from `indra traffic list`.

use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::deck::proxy::{blob_path, traffic_dir};

/// An exchange id: `<unix ms>-<n>`.
pub fn valid_id(id: &str) -> bool {
    let mut parts = id.splitn(2, '-');
    let (Some(a), Some(b)) = (parts.next(), parts.next()) else {
        return false;
    };
    id.len() <= 40
        && !a.is_empty()
        && !b.is_empty()
        && a.bytes().all(|c| c.is_ascii_digit())
        && b.bytes().all(|c| c.is_ascii_digit())
}

pub fn valid_hash(h: &str) -> bool {
    h.len() == 64
        && h.bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

pub const ROLES: [&str; 4] = ["baseline", "proof", "verification", "supporting"];

/// Header values never shown in the console in full.
const SECRET_HEADERS: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-auth-token",
];

/// Recorded exchanges (their index lines), newest first: those whose host
/// is `host` and whose URL or kept bodies contain `grep`, at most `limit`.
pub fn list(dir: &Path, host: Option<&str>, grep: Option<&str>, limit: usize) -> Vec<Value> {
    let text = std::fs::read_to_string(traffic_dir(dir).join("index.jsonl")).unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines().rev() {
        if out.len() >= limit {
            break;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(h) = host.filter(|h| !h.is_empty()) {
            if v["host"].as_str() != Some(&h.to_ascii_lowercase()) {
                continue;
            }
        }
        if let Some(g) = grep.filter(|g| !g.is_empty()) {
            let in_url = v["url"].as_str().unwrap_or("").contains(g);
            let in_body = || {
                get(dir, v["id"].as_str().unwrap_or(""))
                    .map(|r| {
                        ["request", "response"].iter().any(|side| {
                            body(dir, &r[side])
                                .map(|b| String::from_utf8_lossy(&b).contains(g))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            };
            if !in_url && !in_body() {
                continue;
            }
        }
        out.push(v);
    }
    out
}

/// One exchange, as recorded.
pub fn get(dir: &Path, id: &str) -> Result<Value, String> {
    if !valid_id(id) {
        return Err(format!(
            "{:?} is not an exchange id (see `indra traffic list`)",
            id
        ));
    }
    let p = traffic_dir(dir).join(format!("{}.json", id));
    let t = std::fs::read_to_string(&p).map_err(|_| format!("no exchange {}", id))?;
    serde_json::from_str(&t).map_err(|e| format!("{}: {}", id, e))
}

/// The kept body of one side (`request` or `response`) of an exchange,
/// checked against its SHA-256.
pub fn body(dir: &Path, side: &Value) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    let Some(h) = side["body_sha256"].as_str() else {
        return Ok(vec![]);
    };
    if !valid_hash(h) {
        return Err("bad body hash".into());
    }
    let b = std::fs::read(blob_path(dir, h)).map_err(|e| format!("body {}: {}", h, e))?;
    let got: String = Sha256::digest(&b)
        .iter()
        .map(|x| format!("{:02x}", x))
        .collect();
    if got != h {
        return Err(format!("body {} was changed on disk", h));
    }
    Ok(b)
}

/// An exchange with secret header values masked and each body's start, as
/// the console shows it.
pub fn view(dir: &Path, rec: &Value, max_body: usize) -> Value {
    let mut v = rec.clone();
    for side in ["request", "response"] {
        if let Some(hs) = v[side]["headers"].as_array_mut() {
            for h in hs.iter_mut() {
                let name = h[0].as_str().unwrap_or("").to_ascii_lowercase();
                if SECRET_HEADERS.contains(&name.as_str()) {
                    let val = h[1].as_str().unwrap_or("");
                    let shown: String = val.chars().take(6).collect();
                    h[1] = json!(format!(
                        "{}... ({} characters, hidden)",
                        shown,
                        val.chars().count()
                    ));
                }
            }
        }
        let b = body(dir, &rec[side]);
        v[side]["body_preview"] = match &b {
            Ok(b) => json!(String::from_utf8_lossy(&b[..b.len().min(max_body)])),
            Err(e) => json!(format!("(unavailable: {})", e)),
        };
        v[side]["body_preview_truncated"] = json!(b.map(|b| b.len() > max_body).unwrap_or(false));
    }
    v
}

/// Check one finding's `evidence_traffic` against the recordings in
/// `dir_of(engagement)`: each citation, and whether the finding as a whole
/// is evidence-verified.
pub fn verify_finding(
    f: &Value,
    dir_of: &dyn Fn(&str) -> Option<PathBuf>,
    default_eng: Option<&str>,
) -> Value {
    let refs = f["evidence_traffic"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut all_ok = !refs.is_empty();
    let mut proof = false;
    for r in &refs {
        let eng = r["engagement"].as_str().or(default_eng).unwrap_or("");
        let xid = r["exchange"].as_str().unwrap_or("");
        let role = r["role"].as_str().unwrap_or("supporting");
        let quote = r["quote"].as_str().filter(|q| !q.is_empty());
        let res: Result<(), String> = (|| {
            if !ROLES.contains(&role) {
                return Err(format!(
                    "role {:?} is not one of {}",
                    role,
                    ROLES.join(", ")
                ));
            }
            let dir =
                dir_of(eng).ok_or_else(|| format!("no engagement {:?} on this machine", eng))?;
            let rec = get(&dir, xid)?;
            if let Some(h) = r["sha256"].as_str() {
                if rec["response"]["body_sha256"].as_str() != Some(h) {
                    return Err("the response it cites has a different SHA-256".into());
                }
            }
            if let Some(q) = quote {
                let mut found = false;
                for side in ["response", "request"] {
                    let b = body(&dir, &rec[side])?;
                    let heads = rec[side]["headers"].to_string();
                    if String::from_utf8_lossy(&b).contains(q) || heads.contains(q) {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err("the quote is not in the recorded exchange".into());
                }
            }
            Ok(())
        })();
        if res.is_ok() && role == "proof" && quote.is_some() {
            proof = true;
        }
        all_ok &= res.is_ok();
        out.push(json!({ "exchange": xid, "engagement": eng, "role": role, "ok": res.is_ok(), "why": res.err() }));
    }
    json!({
        "id": f["id"], "title": f["title"], "citations": out,
        "verified": all_ok && proof,
        "why": if refs.is_empty() { json!("no evidence_traffic") }
               else if !all_ok { json!("a citation does not hold") }
               else if !proof { json!("no proof citation with a quote") }
               else { Value::Null },
    })
}

/// A request from the console about an engagement's traffic on this
/// machine (directly on the hub, or over a node's link): `list`, `show`
/// (masked) or `verify` (findings).
pub fn local_request(args: &Value) -> Result<Value, String> {
    let id = args["id"].as_str().unwrap_or("");
    if !crate::deck::engage::valid_id(id) {
        return Err("bad engagement id".into());
    }
    let dir = crate::deck::engage::local_dir(id);
    match args["op"].as_str().unwrap_or("list") {
        "list" => Ok(json!(list(
            &dir,
            args["host"].as_str(),
            args["grep"].as_str(),
            args["limit"].as_u64().unwrap_or(200).clamp(1, 2000) as usize
        ))),
        "show" => Ok(view(
            &dir,
            &get(&dir, args["exchange"].as_str().unwrap_or(""))?,
            64 << 10,
        )),
        "verify" => {
            let dir_of = |e: &str| -> Option<PathBuf> {
                (e == id).then(|| crate::deck::engage::local_dir(e))
            };
            Ok(json!(args["findings"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|f| verify_finding(f, &dir_of, Some(id)))
                .collect::<Vec<_>>()))
        }
        _ => Err("op is list, show or verify".into()),
    }
}

// ---- CLI --------------------------------------------------------------------------

#[derive(Args, Debug)]
#[command(
    after_help = "Inside an engagement (an agent started in one) the engagement is known; elsewhere pass --engagement. Recordings live in indra's data folder, engagements/<id>/traffic/."
)]
pub struct TrafficArgs {
    /// The engagement (default: the one this agent runs in)
    #[arg(long, global = true)]
    engagement: Option<String>,
    #[command(subcommand)]
    cmd: TrafficCmd,
}

#[derive(Subcommand, Debug)]
pub enum TrafficCmd {
    /// Recorded exchanges, newest first
    List {
        /// Only this host
        #[arg(long)]
        host: Option<String>,
        /// Only exchanges whose URL or kept bodies contain this text
        #[arg(long)]
        grep: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// One exchange: headers and bodies (secret headers masked unless --full)
    Show {
        id: String,
        #[arg(long)]
        full: bool,
    },
    /// Check the traffic evidence that findings cite (a findings.jsonl or verified.jsonl)
    Verify { file: PathBuf },
    /// Send a recorded request again: same scope checks, pacing and recording
    Replay { id: String },
}

fn engagement_spec() -> Option<crate::deck::engage::Spec> {
    std::env::var("INDRA_ENGAGEMENT")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn run(a: TrafficArgs) -> std::process::ExitCode {
    use std::process::ExitCode;
    let env_spec = engagement_spec();
    let id = a
        .engagement
        .clone()
        .or_else(|| env_spec.as_ref().map(|s| s.id.clone()));
    let need = || -> Result<(String, PathBuf), String> {
        let id = id
            .clone()
            .ok_or("which engagement? pass --engagement <id>")?;
        if !crate::deck::engage::valid_id(&id) {
            return Err("bad engagement id".into());
        }
        Ok((id.clone(), crate::deck::engage::local_dir(&id)))
    };
    let res: Result<(), String> = (|| match a.cmd {
        TrafficCmd::List {
            host,
            grep,
            limit,
            json,
        } => {
            let (_, dir) = need()?;
            let v = list(&dir, host.as_deref(), grep.as_deref(), limit.clamp(1, 5000));
            if json {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else if v.is_empty() {
                println!("No recorded exchanges.");
            } else {
                for x in v {
                    println!(
                        "{}  {:>4}  {} {}{}",
                        x["id"].as_str().unwrap_or(""),
                        x["status"]
                            .as_u64()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "err".into()),
                        x["method"].as_str().unwrap_or(""),
                        x["url"].as_str().unwrap_or(""),
                        x["replay_of"]
                            .as_str()
                            .map(|r| format!("  (replay of {})", r))
                            .unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
        TrafficCmd::Show { id: xid, full } => {
            let (_, dir) = need()?;
            let rec = get(&dir, &xid)?;
            let v = if full {
                view_full(&dir, &rec)
            } else {
                view(&dir, &rec, 64 << 10)
            };
            // What the target sent back is the target's text, not instructions.
            println!(
                "{}",
                crate::deck::untrusted::frame(
                    &format!("recorded exchange {}", xid),
                    &serde_json::to_string_pretty(&v).unwrap_or_default()
                )
            );
            Ok(())
        }
        TrafficCmd::Verify { file } => {
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {}", file.display(), e))?;
            let default = id.clone();
            let dir_of = |e: &str| -> Option<PathBuf> {
                crate::deck::engage::valid_id(e).then(|| crate::deck::engage::local_dir(e))
            };
            let mut bad = 0;
            let mut n = 0;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let Ok(f) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if f["evidence_traffic"]
                    .as_array()
                    .map(|a| a.is_empty())
                    .unwrap_or(true)
                {
                    continue;
                }
                n += 1;
                let r = verify_finding(&f, &dir_of, default.as_deref());
                if r["verified"] != true {
                    bad += 1;
                }
                println!("{}", r);
            }
            if n == 0 {
                println!("No finding in {} cites recorded traffic.", file.display());
            }
            if bad > 0 {
                return Err(format!(
                    "{} of {} findings are not evidence-verified",
                    bad, n
                ));
            }
            Ok(())
        }
        TrafficCmd::Replay { id: xid } => {
            let (eid, dir) = need()?;
            let spec = env_spec
                .clone()
                .filter(|s| s.id == eid)
                .ok_or("replay runs inside the engagement: from an agent started in it (its scope and window apply)")?;
            spec.validate()?;
            let rec = get(&dir, &xid)?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            let out = rt.block_on(crate::deck::proxy::replay(spec, dir.clone(), &rec))?;
            println!(
                "{}",
                crate::deck::untrusted::frame(
                    &format!("replay of {}", xid),
                    &serde_json::to_string_pretty(&out).unwrap_or_default()
                )
            );
            if let Some(x) = list(&dir, None, None, 20)
                .into_iter()
                .find(|x| x["replay_of"].as_str() == Some(xid.as_str()))
            {
                println!("Recorded as {}.", x["id"].as_str().unwrap_or(""));
            }
            Ok(())
        }
    })();
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("indra traffic: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn view_full(dir: &Path, rec: &Value) -> Value {
    let mut v = rec.clone();
    for side in ["request", "response"] {
        v[side]["body"] = match body(dir, &rec[side]) {
            Ok(b) => json!(String::from_utf8_lossy(&b)),
            Err(e) => json!(format!("(unavailable: {})", e)),
        };
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn put(dir: &Path, id: &str, req: &str, resp: &str) -> String {
        let t = traffic_dir(dir);
        std::fs::create_dir_all(&t).unwrap();
        let hash = |b: &str| {
            let h: String = Sha256::digest(b.as_bytes())
                .iter()
                .map(|x| format!("{:02x}", x))
                .collect();
            let p = blob_path(dir, &h);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b).unwrap();
            h
        };
        let (rq, rs) = (hash(req), hash(resp));
        let rec = json!({ "id": id, "method": "GET", "url": "https://app.example.com/api/me", "host": "app.example.com",
            "request": { "headers": [["cookie", "session=supersecretvalue"]], "body_sha256": rq, "body_len": req.len() },
            "response": { "status": 200, "headers": [["content-type", "application/json"]], "body_sha256": rs, "body_len": resp.len() } });
        std::fs::write(t.join(format!("{}.json", id)), rec.to_string()).unwrap();
        let line = json!({ "id": id, "url": rec["url"], "host": "app.example.com", "status": 200, "method": "GET" });
        let mut idx = std::fs::read_to_string(t.join("index.jsonl")).unwrap_or_default();
        idx.push_str(&format!("{}\n", line));
        std::fs::write(t.join("index.jsonl"), idx).unwrap();
        rs
    }

    #[test]
    fn citations_are_checked_against_the_recordings() {
        let dir = std::env::temp_dir().join(format!("indra-traffic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        put(
            &dir,
            "1700000000000-1",
            "",
            "{\"user\":\"alice\",\"role\":\"user\"}",
        );
        let h = put(
            &dir,
            "1700000000000-2",
            "",
            "{\"user\":\"bob\",\"role\":\"admin\"}",
        );
        assert_eq!(list(&dir, None, None, 10).len(), 2);
        assert_eq!(
            list(&dir, None, Some("\"admin\""), 10)[0]["id"],
            "1700000000000-2"
        );
        assert!(list(&dir, Some("other.example.com"), None, 10).is_empty());

        let d = dir.clone();
        let dir_of = move |e: &str| (e == "abcd1234").then(|| d.clone());
        let f = json!({ "id": "SH-AC-001", "evidence_traffic": [
            { "engagement": "abcd1234", "exchange": "1700000000000-1", "role": "baseline" },
            { "engagement": "abcd1234", "exchange": "1700000000000-2", "role": "proof", "quote": "\"role\":\"admin\"", "sha256": h },
        ] });
        let r = verify_finding(&f, &dir_of, None);
        assert_eq!(r["verified"], true, "{}", r);
        // A quote that is not there, a wrong hash, an unknown id, no proof.
        let mut bad = f.clone();
        bad["evidence_traffic"][1]["quote"] = json!("\"role\":\"root\"");
        assert_eq!(verify_finding(&bad, &dir_of, None)["verified"], false);
        let mut bad = f.clone();
        bad["evidence_traffic"][1]["sha256"] = json!("0".repeat(64));
        assert_eq!(verify_finding(&bad, &dir_of, None)["verified"], false);
        let mut bad = f.clone();
        bad["evidence_traffic"][0]["exchange"] = json!("1700000000000-9");
        assert_eq!(verify_finding(&bad, &dir_of, None)["verified"], false);
        let mut bad = f.clone();
        bad["evidence_traffic"][1]["role"] = json!("supporting");
        assert_eq!(
            verify_finding(&bad, &dir_of, None)["why"],
            "no proof citation with a quote"
        );
        // A body edited on disk no longer counts.
        std::fs::write(blob_path(&dir, &h), "{\"role\":\"admin\",\"edited\":1}").unwrap();
        assert_eq!(verify_finding(&f, &dir_of, None)["verified"], false);
        // Secret headers are masked for the console.
        let v = view(&dir, &get(&dir, "1700000000000-1").unwrap(), 100);
        let c = v["request"]["headers"][0][1].as_str().unwrap();
        assert!(
            c.starts_with("sessio") && c.contains("hidden") && !c.contains("supersecret"),
            "{}",
            c
        );
        assert!(get(&dir, "../../etc/passwd").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
