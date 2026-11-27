//! indra's hooks in each agent: one `indra guard` program, wired into
//! Claude Code, Codex and Gemini CLI the way each one takes hooks.
//!
//! | phase | Claude Code | Codex | Gemini CLI | what it does |
//! |---|---|---|---|---|
//! | `pre` | PreToolUse | PreToolUse | BeforeTool | engagement: scope, window, pace, audit; deny rules |
//! | `rules` | PreToolUse | PreToolUse | BeforeTool | deny rules only (outside an engagement) |
//! | `post` | PostToolUse | PostToolUse | AfterTool | engagement: record the result, back off; steer |
//! | `steer` | PostToolUse, Stop | PostToolUse, Stop | AfterTool, AfterAgent | hand over a queued steer |
//! | `approve` | - | PermissionRequest | - | allow rules decide what Codex would ask about |
//!
//! - **Claude Code** takes them as `--settings` JSON. Its approvals go over
//!   its control channel, so it gets no `approve` hook.
//! - **Codex** takes them as `-c hooks.<Event>=[...]` overrides. Codex runs a
//!   hook only once a person has reviewed it, so indra passes
//!   `--dangerously-bypass-hook-trust`: that also lets hooks already enabled
//!   in the user's Codex config run without a new review.
//! - **Gemini CLI** reads hooks from settings files only, so indra writes a
//!   private copy of the system settings file (the real one, if any, plus
//!   its hooks) and points `GEMINI_CLI_SYSTEM_SETTINGS_PATH` at it. Gemini
//!   hooks can deny or ask but not allow, so allow rules go into a Gemini
//!   policy file (`--policy`), and the hook asks a person about anything
//!   those rules would allow by text that indra's stricter reading would not.
//!
//! Hooks get the agent's environment; Gemini hooks also get INDRA_* in
//! their own `env`, in case Gemini's environment redaction is on.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Which CLI a runner is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Gemini,
    Other,
}

impl Agent {
    pub fn of(r: &crate::deck::config::Runner) -> Agent {
        Agent::named(&crate::deck::accounts::agent_of(
            &r.name, &r.command, &r.format,
        ))
    }

    pub fn named(n: &str) -> Agent {
        match n {
            "claude" => Agent::Claude,
            "codex" => Agent::Codex,
            "gemini" => Agent::Gemini,
            _ => Agent::Other,
        }
    }
}

/// What an agent's hooks do.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Wanted {
    /// Scope, window, pace and audit (an engagement).
    pub engagement: bool,
    /// Queued steers.
    pub steer: bool,
    /// Deny rules (and, for Codex and Gemini, allow rules).
    pub rules: bool,
    /// Allow rules too (Codex PermissionRequest).
    pub allow: bool,
}

impl Wanted {
    pub fn any(&self) -> bool {
        self.engagement || self.steer || self.rules
    }
}

fn cmd(exe: &Path, phase: &str) -> String {
    format!("\"{}\" guard {}", exe.display(), phase)
}

/// The phases for each point in the agent's loop.
fn phases(
    w: Wanted,
) -> (
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
) {
    let before = if w.engagement {
        Some("pre")
    } else if w.rules {
        Some("rules")
    } else {
        None
    };
    let after = if w.engagement {
        Some("post")
    } else if w.steer {
        Some("steer")
    } else {
        None
    };
    let stop = if w.steer || w.engagement {
        Some("steer")
    } else {
        None
    };
    (before, after, stop)
}

/// Claude Code's `--settings` JSON, or None when no hook is wanted.
pub fn claude_settings(exe: &Path, w: Wanted) -> Option<String> {
    let (before, after, stop) = phases(w);
    let mut hooks = serde_json::Map::new();
    if let Some(p) = before {
        hooks.insert("PreToolUse".into(), json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": cmd(exe, p), "timeout": 120 }] }]));
    }
    if let Some(p) = after {
        hooks.insert("PostToolUse".into(), json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": cmd(exe, p), "timeout": 30 }] }]));
    }
    if let Some(p) = stop {
        hooks.insert(
            "Stop".into(),
            json!([{ "hooks": [{ "type": "command", "command": cmd(exe, p), "timeout": 30 }] }]),
        );
    }
    (!hooks.is_empty()).then(|| json!({ "hooks": hooks }).to_string())
}

/// Codex's arguments for the hooks (`-c hooks.<Event>=...` and the trust
/// bypass), empty when no hook is wanted.
pub fn codex_args(exe: &Path, w: Wanted) -> Vec<String> {
    let (before, after, stop) = phases(w);
    let group = |matcher: Option<&str>, phase: &str, timeout: i64| -> toml::Value {
        let mut h = toml::map::Map::new();
        h.insert("type".into(), "command".into());
        h.insert("command".into(), cmd(exe, phase).into());
        h.insert("timeout".into(), timeout.into());
        let mut g = toml::map::Map::new();
        if let Some(m) = matcher {
            g.insert("matcher".into(), m.into());
        }
        g.insert(
            "hooks".into(),
            toml::Value::Array(vec![toml::Value::Table(h)]),
        );
        toml::Value::Array(vec![toml::Value::Table(g)])
    };
    let mut out = Vec::new();
    let mut add = |event: &str, v: toml::Value| {
        out.push("-c".to_string());
        out.push(format!("hooks.{}={}", event, v));
    };
    if let Some(p) = before {
        add("PreToolUse", group(Some(".*"), p, 120));
    }
    if let Some(p) = after {
        add("PostToolUse", group(Some(".*"), p, 30));
    }
    if let Some(p) = stop {
        add("Stop", group(None, p, 30));
    }
    if w.allow {
        add("PermissionRequest", group(Some(".*"), "approve", 30));
    }
    if !out.is_empty() {
        out.push("--dangerously-bypass-hook-trust".into());
    }
    out
}

/// Where Gemini CLI reads its system settings from, unless told otherwise.
fn gemini_system_settings() -> PathBuf {
    if let Ok(p) = std::env::var("GEMINI_CLI_SYSTEM_SETTINGS_PATH") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/GeminiCli/settings.json")
    } else if cfg!(windows) {
        PathBuf::from("C:\\ProgramData\\gemini-cli\\settings.json")
    } else {
        PathBuf::from("/etc/gemini-cli/settings.json")
    }
}

/// `text` without `//` and `/* */` comments (outside strings), which
/// Gemini's settings files may have.
fn strip_comments(text: &str) -> String {
    let b: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
        } else if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
        } else if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// The system settings Gemini CLI would read, with indra's hooks added.
pub fn gemini_settings(
    base: Option<&str>,
    exe: &Path,
    w: Wanted,
    env: &BTreeMap<String, String>,
) -> Result<Value, String> {
    let mut s: Value = match base {
        Some(t) if !t.trim().is_empty() => {
            serde_json::from_str(&strip_comments(t)).map_err(|e| {
                format!(
                    "Gemini's system settings cannot be read ({}); indra adds its hooks to them",
                    e
                )
            })?
        }
        _ => json!({}),
    };
    if !s.is_object() {
        s = json!({});
    }
    let (before, after, stop) = phases(w);
    let hook = |phase: &str, ms: u64| json!({ "name": format!("indra-{}", phase), "type": "command", "command": cmd(exe, phase), "timeout": ms, "env": env });
    s["hooksConfig"]["enabled"] = json!(true);
    // A disabled list naming indra's hooks would switch them off.
    if let Some(d) = s["hooksConfig"]["disabled"].as_array_mut() {
        d.retain(|x| !x.as_str().unwrap_or("").starts_with("indra-"));
    }
    let mut add = |event: &str, group: Value| {
        let h = &mut s["hooks"][event];
        if !h.is_array() {
            *h = json!([]);
        }
        h.as_array_mut().unwrap().push(group);
    };
    if let Some(p) = before {
        add(
            "BeforeTool",
            json!({ "matcher": "*", "hooks": [hook(p, 120_000)] }),
        );
    }
    if let Some(p) = after {
        add(
            "AfterTool",
            json!({ "matcher": "*", "hooks": [hook(p, 30_000)] }),
        );
    }
    if let Some(p) = stop {
        add("AfterAgent", json!({ "hooks": [hook(p, 30_000)] }));
    }
    Ok(s)
}

/// A Gemini policy file for the rules: allow rules as `commandPrefix`
/// rules, deny rules too (the hook also enforces those).
pub fn gemini_policy(rules: &crate::deck::approve::Rules) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut out = String::from("# Written by indra for one agent: its approval rules.\n");
    for (list, decision, prio) in [(&rules.deny, "deny", 999), (&rules.allow, "allow", 900)] {
        for r in list {
            out.push_str(&format!(
                "\n[[rule]]\ntoolName = \"run_shell_command\"\ncommandPrefix = {}\ndecision = \"{}\"\npriority = {}\n",
                q(&r.join(" ")),
                decision,
                prio
            ));
        }
    }
    out
}

/// A folder removed when it is dropped (an agent's private hook settings).
#[derive(Debug)]
pub struct TempDir(pub PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A private folder under indra's data folder, for one agent's files.
pub fn private_dir(kind: &str) -> Result<TempDir, String> {
    let base = crate::paths::data_dir().join(kind);
    std::fs::create_dir_all(&base).map_err(|e| format!("{}: {}", base.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
    }
    let d = base.join(crate::deck::link::random_token(8));
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {}", d.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    }
    Ok(TempDir(d))
}

/// Write a private file (owner only).
pub fn write_private(p: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(p)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| format!("{}: {}", p.display(), e))
}

/// What Gemini CLI's hooks need: the folder that holds its files (kept
/// while the agent runs), the environment and the arguments to add.
pub type GeminiSetup = (TempDir, Vec<(String, String)>, Vec<String>);

/// Gemini CLI's hooks for one agent: a private system settings file (and
/// a policy file for its rules). Returns the folder (kept while the agent
/// runs), the environment and the arguments to add.
pub fn gemini_setup(
    exe: &Path,
    w: Wanted,
    rules: &crate::deck::approve::Rules,
    hook_env: &BTreeMap<String, String>,
) -> Result<GeminiSetup, String> {
    let dir = private_dir("agent-hooks")?;
    let real = gemini_system_settings();
    let base = std::fs::read_to_string(&real).ok();
    let settings = gemini_settings(base.as_deref(), exe, w, hook_env)?;
    let file = dir.0.join("settings.json");
    write_private(
        &file,
        &serde_json::to_string_pretty(&settings).unwrap_or_default(),
    )?;
    let mut env = vec![(
        "GEMINI_CLI_SYSTEM_SETTINGS_PATH".to_string(),
        file.display().to_string(),
    )];
    let mut args = Vec::new();
    if !rules.is_empty() {
        let p = dir.0.join("policy.toml");
        write_private(&p, &gemini_policy(rules))?;
        args.push("--policy".to_string());
        args.push(p.display().to_string());
    }
    env.sort();
    Ok((dir, env, args))
}

/// What a hook prints to refuse a tool call, for the event that called it.
pub fn deny(event: &str, reason: &str) -> Value {
    match event {
        "BeforeTool" => json!({ "decision": "deny", "reason": reason }),
        _ => json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        } }),
    }
}

/// What a hook prints to have a person decide (Gemini only; elsewhere the
/// agent's own permission settings already ask).
pub fn ask(event: &str, reason: &str) -> Option<Value> {
    (event == "BeforeTool").then(|| json!({ "decision": "ask", "reason": reason }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_per_agent() {
        let exe = Path::new("/opt/indra");
        let eng = Wanted {
            engagement: true,
            steer: true,
            rules: true,
            allow: true,
        };
        let s: Value = serde_json::from_str(&claude_settings(exe, eng).unwrap()).unwrap();
        assert!(s["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("guard pre"));
        assert!(s["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("guard steer"));
        assert!(claude_settings(exe, Wanted::default()).is_none());
        let rules_only = Wanted {
            rules: true,
            ..Default::default()
        };
        let s: Value = serde_json::from_str(&claude_settings(exe, rules_only).unwrap()).unwrap();
        assert!(s["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("guard rules"));
        assert!(s["hooks"].get("Stop").is_none());

        let a = codex_args(exe, eng);
        assert_eq!(a.last().unwrap(), "--dangerously-bypass-hook-trust");
        let pre = a
            .iter()
            .find(|x| x.starts_with("hooks.PreToolUse="))
            .unwrap();
        // A TOML value Codex parses: an array of matcher groups.
        let v: toml::Value = toml::from_str(&format!(
            "x = {}",
            pre.trim_start_matches("hooks.PreToolUse=")
        ))
        .unwrap();
        assert_eq!(
            v["x"][0]["hooks"][0]["command"].as_str().unwrap(),
            "\"/opt/indra\" guard pre"
        );
        assert!(a.iter().any(|x| x.starts_with("hooks.PermissionRequest=")));
        assert!(codex_args(exe, Wanted::default()).is_empty());

        let env: BTreeMap<String, String> = [("INDRA_STEER".to_string(), "t".to_string())].into();
        let base = "{\n  // the admin's settings\n  \"hooksConfig\": { \"disabled\": [\"indra-pre\", \"theirs\"] },\n  \"hooks\": { \"BeforeTool\": [{ \"matcher\": \"x\", \"hooks\": [] }] },\n  \"url\": \"https://x//y\" /* kept */\n}";
        let g = gemini_settings(Some(base), exe, eng, &env).unwrap();
        assert_eq!(g["hooksConfig"]["enabled"], true);
        assert_eq!(g["hooksConfig"]["disabled"], json!(["theirs"]));
        assert_eq!(
            g["hooks"]["BeforeTool"].as_array().unwrap().len(),
            2,
            "theirs kept, indra's added"
        );
        assert_eq!(
            g["hooks"]["AfterAgent"][0]["hooks"][0]["env"]["INDRA_STEER"],
            "t"
        );
        assert_eq!(g["url"], "https://x//y");
        assert!(gemini_settings(Some("{ nope"), exe, eng, &env).is_err());

        let r = crate::deck::approve::Rules {
            allow: vec![vec!["git".into(), "status".into()]],
            deny: vec![vec!["rm".into(), "-rf".into()]],
        };
        let p = gemini_policy(&r);
        let v: toml::Value = toml::from_str(&p).unwrap();
        assert_eq!(v["rule"][0]["decision"], "deny".into());
        assert_eq!(v["rule"][1]["commandPrefix"], "git status".into());
        assert_eq!(deny("BeforeTool", "no")["decision"], "deny");
        assert_eq!(
            deny("PreToolUse", "no")["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
    }
}
