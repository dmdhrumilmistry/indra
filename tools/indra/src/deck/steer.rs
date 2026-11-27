//! Steering: a message for a running agent that reaches it mid-turn.
//!
//! A follow-up (`say`) is read only once the agent's current turn is over.
//! A steer is read after its next tool call instead: the console queues the
//! text in a file on the machine the agent runs on, and `indra guard steer`
//! (Claude Code's PostToolUse hook) hands it over as context with that
//! tool's result. If the turn ends before another tool call, the Stop hook
//! hands it over instead and the agent carries on with it. Nothing is
//! interrupted and nothing is lost.
//!
//! Each agent gets its own random token (INDRA_STEER); its queue is
//! `steer/<token>/queue.jsonl` in indra's data folder, readable by its owner
//! only.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

/// A steering token: 24 lowercase letters and digits.
pub fn valid_token(t: &str) -> bool {
    t.len() == 24
        && t.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// A new token for an agent.
pub fn new_token() -> String {
    crate::deck::link::random_token(12)
}

fn dir_in(base: &Path, token: &str) -> PathBuf {
    base.join("steer").join(token)
}

/// Queue `text` for the agent with `token`, in `base` (indra's data folder).
pub fn queue_in(base: &Path, token: &str, text: &str) -> Result<(), String> {
    if !valid_token(token) {
        return Err("bad steering token".into());
    }
    let text = text.trim();
    if text.is_empty() || text.len() > 20_000 {
        return Err("a steer is 1 to 20000 characters".into());
    }
    let dir = dir_in(base, token);
    let line = json!({ "ts": crate::deck::engage::now(), "text": text }).to_string();
    crate::deck::engage::locked(&dir, || -> std::io::Result<()> {
        let mut o = std::fs::OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let mut f = o.open(dir.join("queue.jsonl"))?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_data()
    })
    .and_then(|r| r)
    .map_err(|e| format!("cannot queue the message: {}", e))
}

/// Queue on this machine.
pub fn queue(token: &str, text: &str) -> Result<(), String> {
    queue_in(&crate::paths::data_dir(), token, text)
}

/// Take every queued message (oldest first), leaving the queue empty.
pub fn take_in(base: &Path, token: &str) -> Vec<String> {
    if !valid_token(token) {
        return vec![];
    }
    let dir = dir_in(base, token);
    if !dir.join("queue.jsonl").exists() {
        return vec![];
    }
    crate::deck::engage::locked(&dir, || {
        let p = dir.join("queue.jsonl");
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        let _ = std::fs::remove_file(&p);
        text.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v["text"].as_str().map(String::from))
            .collect()
    })
    .unwrap_or_default()
}

/// How the messages read to the agent.
fn wording(msgs: &[String]) -> String {
    let mut s = String::from(
        "Message from your operator in the indra console, sent while you were working. Take it into account from your next step on:\n",
    );
    for m in msgs {
        s.push_str("\n> ");
        s.push_str(&m.replace('\n', "\n> "));
        s.push('\n');
    }
    s
}

/// The hook's answer for `event` (PostToolUse or Stop) carrying `msgs`, or
/// None when there is nothing to hand over.
pub fn hook_answer(event: &str, msgs: &[String]) -> Option<Value> {
    if msgs.is_empty() {
        return None;
    }
    let text = wording(msgs);
    Some(match event {
        // Blocking the stop feeds the reason to the agent, which goes on.
        "Stop" | "SubagentStop" => json!({ "decision": "block", "reason": text }),
        _ => json!({ "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": text,
        } }),
    })
}

/// `indra guard steer`: hand queued messages to the agent (PostToolUse or
/// Stop hook). Never fails the tool call: with nothing queued it is silent.
pub fn hook_main(hook: &Value) {
    let Some(token) = std::env::var("INDRA_STEER").ok().filter(|t| valid_token(t)) else {
        return;
    };
    let event = hook["hook_event_name"].as_str().unwrap_or("PostToolUse");
    let msgs = take_in(&crate::paths::data_dir(), &token);
    if let Some(a) = hook_answer(event, &msgs) {
        println!("{}", a);
    }
}

/// Claude Code's `--settings` for an agent that can be steered (outside an
/// engagement, which has its own hooks).
pub fn hook_settings(exe: &Path) -> String {
    let cmd = format!("\"{}\" guard steer", exe.display());
    json!({ "hooks": {
        "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": cmd.clone(), "timeout": 30 }] }],
        "Stop": [{ "hooks": [{ "type": "command", "command": cmd, "timeout": 30 }] }],
    } })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_queued_taken_once_and_worded_per_hook() {
        let base = std::env::temp_dir().join(format!("indra-steer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let t = new_token();
        assert!(valid_token(&t), "{}", t);
        assert!(take_in(&base, &t).is_empty());
        queue_in(&base, &t, "Stop scanning port 22; focus on the login form").unwrap();
        queue_in(&base, &t, "  and skip /admin  ").unwrap();
        assert!(queue_in(&base, &t, "   ").is_err());
        assert!(queue_in(&base, "../../etc", "x").is_err());
        let got = take_in(&base, &t);
        assert_eq!(
            got,
            [
                "Stop scanning port 22; focus on the login form",
                "and skip /admin"
            ]
        );
        assert!(take_in(&base, &t).is_empty(), "taken once");

        let post = hook_answer("PostToolUse", &got).unwrap();
        let ctx = post["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("> and skip /admin"));
        assert_eq!(post["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        let stop = hook_answer("Stop", &got).unwrap();
        assert_eq!(stop["decision"], "block");
        assert!(stop["reason"].as_str().unwrap().contains("login form"));
        assert!(
            hook_answer("Stop", &[]).is_none(),
            "nothing queued: let it stop"
        );
        let s: Value = serde_json::from_str(&hook_settings(Path::new("/x/indra"))).unwrap();
        assert!(s["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("guard steer"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
