//! The console in your terminal: `indra agents` lists the agents a running
//! `indra serve` knows about, `indra attach` follows one live and steers it
//! (approve tool requests, change mode or model, send messages, pivot).
//!
//! It talks to the console's own API with its token, so it works against a
//! console on this machine (found through `console.json` in the data
//! directory, which `indra serve` writes and removes) or anywhere else
//! (`--url` and `--token`, or `INDRA_URL` and `INDRA_TOKEN`). Requests go
//! through the system `curl`, with the token on its stdin, never in argv.

use serde_json::{json, Value};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const FILE: &str = "console.json";

/// Where a running console is, and its token.
#[derive(Clone)]
pub struct Console {
    pub url: String,
    pub token: String,
}

fn discovery_path() -> PathBuf {
    crate::deck::config::data_dir().join(FILE)
}

/// Write `console.json` (owner-only) so terminal clients on this machine can
/// find the console. Returns the path, to remove it on shutdown.
pub fn advertise(_data_dir: &Path, url: &str, token: &str) -> Option<PathBuf> {
    let path = discovery_path();
    std::fs::create_dir_all(path.parent()?).ok()?;
    let body = json!({ "url": url, "token": token, "pid": std::process::id() }).to_string();
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp).ok()?;
    std::io::Write::write_all(&mut f, body.as_bytes()).ok()?;
    drop(f);
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

/// Remove `console.json`, unless another console has replaced it since.
pub fn withdraw(path: &Path) {
    if let Ok(t) = std::fs::read_to_string(path) {
        let mine = serde_json::from_str::<Value>(&t)
            .ok()
            .and_then(|v| v.get("pid").and_then(Value::as_u64))
            == Some(std::process::id() as u64);
        if mine {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The console to talk to: flags or environment first, else `console.json`.
pub fn find(url: Option<String>, token: Option<String>) -> Result<Console, String> {
    let saved = std::fs::read_to_string(discovery_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let from_file = |k: &str| {
        saved
            .as_ref()
            .and_then(|v| v.get(k).and_then(Value::as_str).map(str::to_string))
    };
    let url = url
        .filter(|u| !u.is_empty())
        .or_else(|| from_file("url"))
        .ok_or(
            "no console found: start `indra serve` on this machine, or pass --url and --token",
        )?;
    let token = token
        .filter(|t| !t.is_empty())
        .or_else(|| from_file("token"))
        .ok_or("no token: pass --token or set INDRA_TOKEN (the token= part of the URL `indra serve` printed)")?;
    let url = url.trim_end_matches('/').to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("not an http(s) URL: {}", url));
    }
    Ok(Console { url, token })
}

fn curl_config(
    c: &Console,
    method: &str,
    path: &str,
    body: Option<&Value>,
    stream: bool,
) -> String {
    use crate::deck::trackers::quote;
    let mut cfg = String::from("silent\nshow-error\nconnect-timeout = 5\n");
    if stream {
        cfg.push_str("no-buffer\n");
    } else {
        cfg.push_str("max-time = 30\nwrite-out = \"\\n%{http_code}\"\n");
    }
    cfg.push_str(&format!(
        "request = {}\nurl = {}\nheader = {}\n",
        quote(method),
        quote(&format!("{}{}", c.url, path)),
        quote(&format!("Authorization: Bearer {}", c.token))
    ));
    if let Some(b) = body {
        cfg.push_str("header = \"Content-Type: application/json\"\n");
        cfg.push_str(&format!("data-binary = {}\n", quote(&b.to_string())));
    }
    cfg
}

async fn spawn_curl(cfg: String) -> Result<tokio::process::Child, String> {
    let curl =
        crate::deck::config::which("curl").ok_or("curl is not installed (indra attach uses it)")?;
    let mut child = tokio::process::Command::new(curl)
        .args(["-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot run curl: {}", e))?;
    if let Some(mut i) = child.stdin.take() {
        i.write_all(cfg.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(child)
}

impl Console {
    /// One API call; the JSON answer, or the console's error message.
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let child = spawn_curl(curl_config(self, method, path, body.as_ref(), false)).await?;
        let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_str()));
        let code: u16 = code.trim().parse().unwrap_or(0);
        if code == 0 {
            return Err(format!(
                "cannot reach the console at {}: {}",
                self.url,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        if !(200..300).contains(&code) {
            let msg = v
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| body.chars().take(200).collect());
            return Err(if code == 401 {
                format!("{} (is the token right?)", msg)
            } else {
                msg
            });
        }
        Ok(v)
    }
}

// ---- output -------------------------------------------------------------------

struct Paint(bool);

impl Paint {
    fn c(&self, code: &str, s: &str) -> String {
        if self.0 {
            format!("\x1b[{}m{}\x1b[0m", code, s)
        } else {
            s.to_string()
        }
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn clip(t: &str, n: usize) -> String {
    let one = t.replace('\n', " ");
    if one.chars().count() <= n {
        one
    } else {
        format!(
            "{}...",
            one.chars().take(n.saturating_sub(3)).collect::<String>()
        )
    }
}

fn status_color(st: &str) -> &'static str {
    match st {
        "running" => "34",
        "exited" => "32",
        "failed" => "31",
        _ => "33",
    }
}

/// `indra agents`: the console's agents, newest first.
pub async fn list(c: &Console, all: bool, as_json: bool) -> Result<(), String> {
    let v = c.call("GET", "/api/procs", None).await?;
    let mut procs: Vec<Value> = v.as_array().cloned().unwrap_or_default();
    if !all {
        procs.retain(|p| {
            s(p, "status") == "running" || p["pending"].as_array().is_some_and(|a| !a.is_empty())
        });
    }
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&procs).unwrap_or_default()
        );
        return Ok(());
    }
    if procs.is_empty() {
        println!(
            "{}",
            if all {
                "no agents yet"
            } else {
                "no running agents (--all shows finished ones too)"
            }
        );
        return Ok(());
    }
    let p = Paint(std::io::stdout().is_terminal());
    println!(
        "{:>4}  {:<8}  {:<10}  {:<12}  {:<17}  {:>4}  LAST",
        "ID", "STATUS", "NODE", "RUNNER", "MODE / MODEL", "WAIT"
    );
    for x in &procs {
        let st = s(x, "status");
        let mm = format!(
            "{}{}",
            x["mode"]
                .as_str()
                .or(x["opts"]["permission_mode"].as_str())
                .unwrap_or("-"),
            x["model"]
                .as_str()
                .or(x["opts"]["model"].as_str())
                .map(|m| format!(" / {}", m))
                .unwrap_or_default()
        );
        let wait = x["pending"].as_array().map(|a| a.len()).unwrap_or(0);
        println!(
            "{:>4}  {}  {:<10}  {:<12}  {:<17}  {}  {}",
            x["id"].as_u64().unwrap_or(0),
            p.c(status_color(st), &format!("{:<8}", st)),
            clip(s(x, "node"), 10),
            clip(s(x, "runner"), 12),
            clip(&mm, 17),
            if wait > 0 {
                p.c("33;1", &format!("{:>4}", wait))
            } else {
                format!("{:>4}", "")
            },
            clip(x["last"].as_str().unwrap_or(s(x, "prompt")), 60),
        );
    }
    println!("\nindra attach <ID> follows one and steers it.");
    Ok(())
}

/// The agent to attach to when none is named: one waiting for an answer, else
/// the newest running one, else the newest.
async fn pick(c: &Console) -> Result<u64, String> {
    let v = c.call("GET", "/api/procs", None).await?;
    let procs = v.as_array().cloned().unwrap_or_default();
    let newest = |f: &dyn Fn(&Value) -> bool| {
        procs
            .iter()
            .filter(|p| f(p))
            .filter_map(|p| p["id"].as_u64())
            .max()
    };
    newest(&|p| p["pending"].as_array().is_some_and(|a| !a.is_empty()))
        .or_else(|| newest(&|p| s(p, "status") == "running"))
        .or_else(|| newest(&|_| true))
        .ok_or_else(|| "no agents yet: launch one in the console or with `indra run`".to_string())
}

fn print_event(p: &Paint, e: &Value) {
    let text = s(e, "text");
    let who = e
        .get("agent")
        .and_then(Value::as_str)
        .map(|a| p.c("2", &format!("[{}] ", a)))
        .unwrap_or_default();
    let line = match s(e, "kind") {
        "text" => format!("{}{}", who, text),
        "tool" => format!("{}{}", who, p.c("36", &format!("  > {}", text))),
        "agent" => format!("{}{}", who, p.c("35", &format!("  * {}", text))),
        "permission" => format!("{}{}", who, p.c("33;1", &format!("? {}", text))),
        "result" => p.c("32;1", &format!("= {}", text)),
        "error" => p.c("31;1", &format!("! {}", text)),
        "stderr" => p.c("2", &format!("  {}", text)),
        "user" => p.c("1", &format!("you: {}", text)),
        _ => p.c("2", &format!("- {}", text)),
    };
    println!("{}", line);
}

fn print_head(p: &Paint, v: &Value) {
    println!(
        "{} {}  {} on {}  in {}{}",
        p.c("1", &format!("process {}", v["id"])),
        p.c(status_color(s(v, "status")), s(v, "status")),
        s(v, "runner"),
        s(v, "node"),
        s(v, "cwd"),
        v["session"]
            .as_str()
            .map(|x| format!("  session {}", x))
            .unwrap_or_default()
    );
    let mode = v["mode"].as_str().or(v["opts"]["permission_mode"].as_str());
    let model = v["model"].as_str().or(v["opts"]["model"].as_str());
    if mode.is_some() || model.is_some() {
        println!(
            "  mode {}  model {}",
            mode.unwrap_or("-"),
            model.unwrap_or("-")
        );
    }
    let mut ctx = Vec::new();
    if let Some(t) = v["context_tokens"].as_u64() {
        let w = v["context_window"].as_u64().unwrap_or(200_000);
        ctx.push(format!("context {}k of {}k tokens", t / 1000, w / 1000));
    }
    if let Some(m) = v["memory"].as_array().filter(|m| !m.is_empty()) {
        ctx.push(format!("{} memory note(s) given", m.len()));
    }
    if v["reflect"].as_bool() == Some(true) {
        ctx.push("proposes learnings at the end".into());
    }
    if !ctx.is_empty() {
        println!("  {}", ctx.join(", "));
    }
}

fn print_pending(p: &Paint, v: &Value) {
    let pending = v["pending"].as_array().cloned().unwrap_or_default();
    for (i, a) in pending.iter().enumerate() {
        println!(
            "{}",
            p.c(
                "33;1",
                &format!(
                    "  [{}] {}: {}",
                    i + 1,
                    s(a, "tool"),
                    clip(&a["input"].to_string(), 160)
                )
            )
        );
    }
    if !pending.is_empty() {
        println!(
            "{}",
            p.c("2", "  /allow [n], /always [n] or /deny [n] [reason]")
        );
    }
}

const HELP: &str = "\
  text            send it to the agent as a message (same session)
  /allow [n]      approve the waiting tool request n (default: the oldest)
  /always [n]     approve it and allow the same again for the session
  /deny [n] [why] deny it, with an optional reason the agent sees
  /mode <mode>    manual, acceptEdits, auto, plan, dontAsk, bypassPermissions
  /model <model>  e.g. opus, sonnet, or a full model id
  /interrupt      stop the current turn, keep the session
  /pivot [--fresh] [--keep] <text>
                  new direction: continue the session (or start fresh), stop
                  the agent unless --keep, then follow the new process
  /learn <text>   put a learning about this agent's work in the inbox
  /status         where it is and what it waits for
  /agents         list running agents      /switch <id>  follow another
  /stop           stop the agent           /quit         leave (Ctrl-D)";

/// Follow `id` from event `after` on, printing as it goes; ends with the stream.
async fn follow(c: Console, id: u64, mut after: u64, paint: bool) {
    let p = Paint(paint);
    loop {
        let cfg = curl_config(
            &c,
            "GET",
            &format!("/api/procs/{}/events?after={}", id, after),
            None,
            true,
        );
        let Ok(mut child) = spawn_curl(cfg).await else {
            return;
        };
        let Some(out) = child.stdout.take() else {
            return;
        };
        let mut lines = BufReader::new(out).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let Some(data) = l.strip_prefix("data:") else {
                continue;
            };
            let Ok(e) = serde_json::from_str::<Value>(data.trim_start()) else {
                continue;
            };
            after = after.max(e["seq"].as_u64().unwrap_or(0));
            print_event(&p, &e);
            if s(&e, "kind") == "exit" {
                // Its last event (the stream itself stays open).
                println!(
                    "{}",
                    p.c(
                        "2",
                        &format!(
                            "[process {} ended; /pivot to take it further, /quit to leave]",
                            id
                        )
                    )
                );
                return;
            }
            if s(&e, "kind") == "permission" && s(&e, "text").starts_with("asks") {
                if let Ok(v) = c.call("GET", &format!("/api/procs/{}", id), None).await {
                    print_pending(&p, &v);
                }
            }
        }
        let _ = child.kill().await;
        // The stream ends when the agent does, or when the connection drops.
        match c.call("GET", &format!("/api/procs/{}", id), None).await {
            Ok(v) if s(&v, "status") == "running" => {
                // The connection dropped: pick up where it stopped.
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            Ok(v) => {
                println!(
                    "{}",
                    p.c(
                        "2",
                        &format!(
                            "[process {} {}{}; /pivot to take it further, /quit to leave]",
                            id,
                            s(&v, "status"),
                            v["exit_code"]
                                .as_i64()
                                .map(|c| format!(", exit code {}", c))
                                .unwrap_or_default()
                        )
                    )
                );
                return;
            }
            Err(_) => {}
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn pick_approval(v: &Value, arg: &str) -> Result<(String, String), String> {
    let pending = v["pending"].as_array().cloned().unwrap_or_default();
    if pending.is_empty() {
        return Err("nothing is waiting for an answer".into());
    }
    let (n, rest) = match arg.split_once(' ') {
        Some((a, b)) => (a, b.trim()),
        None => (arg, ""),
    };
    let (i, rest) = match n.parse::<usize>() {
        Ok(i) if i >= 1 && i <= pending.len() => (i - 1, rest.to_string()),
        Ok(_) => return Err(format!("pick 1 to {}", pending.len())),
        Err(_) => (0, arg.to_string()),
    };
    Ok((s(&pending[i], "request_id").to_string(), rest))
}

/// What a typed line leads to.
enum Step {
    Quiet,
    Say(String),
    Quit,
    Switch(u64),
}

/// `indra attach`: follow one agent and steer it from the terminal.
pub async fn attach(c: Console, id: Option<u64>) -> Result<(), String> {
    let paint = std::io::stdout().is_terminal();
    let p = Paint(paint);
    let mut id = match id {
        Some(i) => i,
        None => pick(&c).await?,
    };
    let v = c.call("GET", &format!("/api/procs/{}", id), None).await?;
    print_head(&p, &v);
    println!("{}", p.c("2", "type a message, or /help"));
    let mut task = tokio::spawn(follow(c.clone(), id, 0, paint));
    let interactive = std::io::stdin().is_terminal();
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    loop {
        let line = match input.next_line().await {
            Ok(Some(l)) => l,
            _ => {
                if !interactive {
                    // Input was a pipe and it ended: keep following until the agent does.
                    let _ = (&mut task).await;
                }
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (cmd, arg) = match line.strip_prefix('/') {
            Some(r) => match r.split_once(' ') {
                Some((a, b)) => (a, b.trim()),
                None => (r, ""),
            },
            None => ("", line),
        };
        let base = format!("/api/procs/{}", id);
        let r: Result<Step, String> = async {
            match cmd {
                "" => {
                    c.call("POST", &format!("{}/message", base), Some(json!({ "text": arg }))).await?;
                    Ok(Step::Quiet)
                }
                "help" | "h" | "?" => Ok(Step::Say(HELP.to_string())),
                "quit" | "q" | "exit" | "detach" => Ok(Step::Quit),
                "allow" | "approve" | "always" | "deny" => {
                    let v = c.call("GET", &base, None).await?;
                    let (rid, rest) = pick_approval(&v, arg)?;
                    let allow = cmd != "deny";
                    let mut body = json!({ "request_id": rid, "allow": allow, "always": cmd == "always" });
                    if !allow && !rest.is_empty() {
                        body["message"] = json!(rest);
                    }
                    c.call("POST", &format!("{}/permission", base), Some(body)).await?;
                    Ok(Step::Quiet)
                }
                "mode" if !arg.is_empty() => {
                    c.call("POST", &format!("{}/control", base), Some(json!({ "mode": arg }))).await?;
                    Ok(Step::Say(format!("mode: {}", arg)))
                }
                "model" if !arg.is_empty() => {
                    c.call("POST", &format!("{}/control", base), Some(json!({ "model": arg }))).await?;
                    Ok(Step::Say(format!("model: {}", arg)))
                }
                "interrupt" | "int" => {
                    c.call("POST", &format!("{}/control", base), Some(json!({ "interrupt": true }))).await?;
                    Ok(Step::Say("interrupted".into()))
                }
                "stop" => {
                    c.call("POST", &format!("{}/stop", base), None).await?;
                    Ok(Step::Say("stopping".into()))
                }
                "learn" if !arg.is_empty() => {
                    let title: String = arg
                        .split(['.', '\n'])
                        .next()
                        .unwrap_or(arg)
                        .chars()
                        .take(100)
                        .collect();
                    c.call(
                        "POST",
                        "/api/learnings",
                        Some(json!({ "title": title.trim(), "body": arg, "proc": id })),
                    )
                    .await?;
                    Ok(Step::Say("added to the learnings inbox (Knowledge > Learnings in the console)".into()))
                }
                "status" | "st" => {
                    let v = c.call("GET", &base, None).await?;
                    print_head(&p, &v);
                    print_pending(&p, &v);
                    Ok(Step::Quiet)
                }
                "agents" | "ls" => {
                    list(&c, false, false).await?;
                    Ok(Step::Quiet)
                }
                "pivot" => {
                    let mut fresh = false;
                    let mut keep = false;
                    let mut words: Vec<&str> = arg.split(' ').collect();
                    while let Some(w) = words.first() {
                        match *w {
                            "--fresh" => fresh = true,
                            "--keep" => keep = true,
                            _ => break,
                        }
                        words.remove(0);
                    }
                    let text = words.join(" ");
                    if text.trim().is_empty() {
                        return Err("/pivot [--fresh] [--keep] <new direction>".into());
                    }
                    let r = c
                        .call(
                            "POST",
                            &format!("{}/pivot", base),
                            Some(json!({ "prompt": text, "mode": if fresh { "fresh" } else { "continue" }, "keep_running": keep })),
                        )
                        .await?;
                    let new = r["id"].as_u64().ok_or("the console did not return the new process")?;
                    Ok(Step::Switch(new))
                }
                "switch" | "attach" => {
                    let new: u64 = arg.parse().map_err(|_| "/switch <process id>".to_string())?;
                    c.call("GET", &format!("/api/procs/{}", new), None).await?;
                    Ok(Step::Switch(new))
                }
                _ => Err(format!("unknown command /{}; /help lists them", cmd)),
            }
        }
        .await;
        match r {
            Ok(Step::Say(msg)) => println!("{}", p.c("2", &msg)),
            Ok(Step::Quiet) => {}
            Ok(Step::Quit) => break,
            Ok(Step::Switch(new)) => {
                id = new;
                task.abort();
                let v = c.call("GET", &format!("/api/procs/{}", id), None).await?;
                println!();
                print_head(&p, &v);
                task = tokio::spawn(follow(c.clone(), id, 0, paint));
            }
            Err(e) => println!("{}", p.c("31", &format!("! {}", e))),
        }
    }
    task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_are_picked_by_number_or_the_oldest() {
        let v = json!({ "pending": [{ "request_id": "a" }, { "request_id": "b" }] });
        assert_eq!(pick_approval(&v, "").unwrap(), ("a".into(), "".into()));
        assert_eq!(pick_approval(&v, "2").unwrap(), ("b".into(), "".into()));
        assert_eq!(
            pick_approval(&v, "2 not that one").unwrap(),
            ("b".into(), "not that one".into())
        );
        assert_eq!(
            pick_approval(&v, "too risky").unwrap(),
            ("a".into(), "too risky".into())
        );
        assert!(pick_approval(&v, "3").is_err());
        assert!(pick_approval(&json!({ "pending": [] }), "").is_err());
    }

    #[test]
    fn a_console_named_on_the_command_line_wins() {
        let c = find(Some("http://127.0.0.1:9/".into()), Some("t".repeat(20))).unwrap();
        assert_eq!(c.url, "http://127.0.0.1:9");
        assert!(find(Some("ftp://x".into()), Some("t".repeat(20))).is_err());
        let cfg = curl_config(
            &c,
            "POST",
            "/api/procs/1/message",
            Some(&json!({ "text": "a \"b\"" })),
            false,
        );
        assert!(cfg.contains("url = \"http://127.0.0.1:9/api/procs/1/message\""));
        assert!(cfg.contains("Authorization: Bearer tttt"));
        assert!(
            cfg.contains(r#"data-binary = "{\"text\":\"a \\\"b\\\"\"}""#),
            "{}",
            cfg
        );
        assert!(curl_config(&c, "GET", "/x", None, true).contains("no-buffer"));
    }
}
