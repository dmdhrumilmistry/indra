//! MCP & tools: everything agents can be given beyond their own setup, in
//! one `[[tool]]` list.
//!
//! - `kind = "mcp"`: any third-party MCP server, started from a command
//!   (stdio) or reached at a URL (HTTP). EDR, threat intel, ticketing,
//!   scanners, a vendor's own SIEM server: whatever speaks MCP.
//! - `kind = "wazuh" | "elastic" | "opensearch" | "splunk" | "files"`: a
//!   read-only data source (see `siem.rs`), searched by `indra siem`, by
//!   detection checks, and by agents through indra's own MCP server.
//!
//! A tool goes to the agents of the profiles it names (`*`: every Claude
//! Code agent), or to a launch that picks it. Values may be `secret:<name>`
//! references (scope `tool:<name>`), resolved at launch and written to a
//! private config file the agent reads, never put on a command line.

use crate::deck::siem::{SiemCfg, KINDS as SOURCE_KINDS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const CATEGORIES: [&str; 8] = [
    "siem",
    "edr",
    "threat-intel",
    "ticketing",
    "scanner",
    "cloud",
    "identity",
    "other",
];

/// A `[[tool]]` in indra.toml.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolCfg {
    pub name: String,
    /// mcp | wazuh | elastic | opensearch | splunk | files
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// siem | edr | threat-intel | ticketing | scanner | cloud | identity | other
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    /// Agents of these profiles get it; "*" for every Claude Code agent.
    /// Others can still pick it when they are launched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// mcp, started locally: the program and its arguments.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// mcp: its environment ("secret:<name>" for keys).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// mcp over HTTP: its URL. A data source: its search API.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// mcp over HTTP: request headers ("secret:<name>" for tokens).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    // A data source's own settings (see siem.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure_tls: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

fn env_name_ok(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 128
        && !k.starts_with(|c: char| c.is_ascii_digit())
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn header_name_ok(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 128
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl ToolCfg {
    /// A data source, not an MCP server.
    pub fn is_source(&self) -> bool {
        SOURCE_KINDS.contains(&self.kind.as_str())
    }

    pub fn validate(&self) -> Result<(), String> {
        let at = |m: &str| format!("tool '{}': {}", self.name, m);
        crate::deck::nodes::validate_name(&self.name).map_err(|e| at(&e))?;
        if !self.category.is_empty() && !CATEGORIES.contains(&self.category.as_str()) {
            return Err(at(&format!(
                "category must be one of {}",
                CATEGORIES.join(", ")
            )));
        }
        for p in &self.profiles {
            if p != "*" {
                crate::deck::nodes::validate_name(p).map_err(|e| at(&format!("profile {}", e)))?;
            }
        }
        if self.description.chars().count() > 500 {
            return Err(at("the description is longer than 500 characters"));
        }
        if self.is_source() {
            if !self.command.is_empty()
                || !self.args.is_empty()
                || !self.env.is_empty()
                || !self.headers.is_empty()
            {
                return Err(at(
                    "command, args, env and headers are for MCP servers, not data sources",
                ));
            }
            return self.source().map(|s| s.validate()).unwrap_or(Ok(()));
        }
        if self.kind != "mcp" {
            return Err(at(&format!(
                "kind must be mcp or one of {}",
                SOURCE_KINDS.join(", ")
            )));
        }
        match (self.command.is_empty(), self.url.is_empty()) {
            (false, true) => {
                if self.command.contains('\0')
                    || self.args.len() > 64
                    || self.args.iter().any(|a| a.contains('\0') || a.len() > 4096)
                {
                    return Err(at("bad command or arguments"));
                }
                if !self.headers.is_empty() {
                    return Err(at("headers are for an MCP server reached at a URL"));
                }
            }
            (true, false) => {
                if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
                    return Err(at("url must be an http(s) URL"));
                }
                if !self.args.is_empty() || !self.env.is_empty() {
                    return Err(at(
                        "args and env are for an MCP server started from a command",
                    ));
                }
            }
            _ => return Err(at(
                "an MCP server has a command (it runs on the agent's machine) or a url, not both",
            )),
        }
        for (k, v) in &self.env {
            if !env_name_ok(k) || v.contains('\0') || v.len() > 8192 {
                return Err(at(&format!("bad environment variable {:?}", k)));
            }
        }
        for (k, v) in &self.headers {
            if !header_name_ok(k) || v.contains(['\r', '\n', '\0']) || v.len() > 8192 {
                return Err(at(&format!("bad header {:?}", k)));
            }
        }
        if self.username.is_some()
            || self.password.is_some()
            || self.token.is_some()
            || self.api_key.is_some()
            || self.index.is_some()
            || !self.paths.is_empty()
        {
            return Err(at("username, password, token, api_key, index and paths are for data sources; an MCP server takes env or headers"));
        }
        Ok(())
    }

    /// As a data source.
    pub fn source(&self) -> Option<SiemCfg> {
        self.is_source().then(|| SiemCfg {
            name: self.name.clone(),
            kind: self.kind.clone(),
            url: self.url.clone(),
            index: self.index.clone(),
            time_field: self.time_field.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            token: self.token.clone(),
            api_key: self.api_key.clone(),
            ca_file: self.ca_file.clone(),
            insecure_tls: self.insecure_tls,
            paths: self.paths.clone(),
        })
    }

    /// Does an agent with this profile get it without asking?
    pub fn for_profile(&self, profile: Option<&str>) -> bool {
        !self.disabled
            && self
                .profiles
                .iter()
                .any(|p| p == "*" || Some(p.as_str()) == profile)
    }

    /// What the console shows: secret values never, only which keys exist.
    pub fn view(&self) -> Value {
        json!({
            "name": self.name, "kind": self.kind, "category": self.category, "description": self.description,
            "profiles": self.profiles, "disabled": self.disabled,
            "transport": if self.is_source() { "source" } else if self.url.is_empty() { "stdio" } else { "http" },
            "command": self.command, "url": self.url,
            "env": self.env.keys().collect::<Vec<_>>(), "headers": self.headers.keys().collect::<Vec<_>>(),
        })
    }
}

/// One MCP server an agent is started with: references until launch,
/// values after.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct McpServer {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

impl McpServer {
    pub fn validate(&self, name: &str) -> Result<(), String> {
        crate::deck::nodes::validate_name(name)?;
        let t = ToolCfg {
            name: name.into(),
            kind: "mcp".into(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            url: self.url.clone(),
            headers: self.headers.clone(),
            ..Default::default()
        };
        t.validate()
    }

    fn json(&self) -> Value {
        if self.url.is_empty() {
            json!({ "type": "stdio", "command": self.command, "args": self.args, "env": self.env })
        } else {
            json!({ "type": "http", "url": self.url, "headers": self.headers })
        }
    }
}

/// The MCP servers for a launch: the tools of its profile, the ones it
/// picked, and indra's own server for the data sources among them (only
/// on the hub's machine, where their configuration and secrets are).
pub fn servers_for(
    tools: &[ToolCfg],
    profile: Option<&str>,
    picked: &[String],
    local: bool,
    config: Option<&Path>,
) -> Result<BTreeMap<String, McpServer>, String> {
    for p in picked {
        if !tools.iter().any(|t| &t.name == p) {
            return Err(format!("no tool named '{}' (MCP & tools)", p));
        }
    }
    let chosen: Vec<&ToolCfg> = tools
        .iter()
        .filter(|t| !t.disabled && (t.for_profile(profile) || picked.contains(&t.name)))
        .collect();
    let mut out = BTreeMap::new();
    for t in chosen.iter().filter(|t| !t.is_source()) {
        out.insert(
            t.name.clone(),
            McpServer {
                command: t.command.clone(),
                args: t.args.clone(),
                env: t.env.clone(),
                url: t.url.clone(),
                headers: t.headers.clone(),
            },
        );
    }
    let sources: Vec<&str> = chosen
        .iter()
        .filter(|t| t.is_source())
        .map(|t| t.name.as_str())
        .collect();
    if !sources.is_empty() && local {
        let exe = std::env::current_exe().map_err(|e| format!("where is indra? {}", e))?;
        let mut args = vec!["siem".to_string()];
        if let Some(c) = config {
            args.push("--config".into());
            args.push(c.display().to_string());
        }
        args.push("mcp".into());
        args.push("--only".into());
        args.push(sources.join(","));
        out.insert(
            "indra-sources".into(),
            McpServer {
                command: exe.display().to_string(),
                args,
                ..Default::default()
            },
        );
    }
    Ok(out)
}

/// Write the MCP config file an agent is started with: owner-only, in
/// indra's data folder. The caller deletes it when the agent exits.
pub fn write_config(servers: &BTreeMap<String, McpServer>) -> Result<PathBuf, String> {
    use std::io::Write;
    let dir = crate::paths::data_dir().join("run");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let mut b = [0u8; 12];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    let name: String = b.iter().map(|x| format!("{:02x}", x)).collect();
    let path = dir.join(format!("mcp-{}.json", name));
    let v = json!({ "mcpServers": servers.iter().map(|(k, s)| (k.clone(), s.json())).collect::<serde_json::Map<String, Value>>() });
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(&path)
        .and_then(|mut f| {
            f.write_all(
                serde_json::to_string_pretty(&v)
                    .unwrap_or_default()
                    .as_bytes(),
            )
        })
        .map_err(|e| format!("{}: {}", path.display(), e))?;
    Ok(path)
}

/// Resolve the secret references of a launch's MCP servers (scope
/// `tool:<name>`). The values, and the pairs to redact from output.
pub fn resolve(
    servers: &BTreeMap<String, McpServer>,
    vault: &crate::deck::secrets::Vault,
    node: &str,
) -> Result<
    (
        BTreeMap<String, McpServer>,
        crate::deck::secrets::Redactions,
    ),
    String,
> {
    let mut out = BTreeMap::new();
    let mut red = Vec::new();
    for (name, s) in servers {
        let scope = format!("tool:{}", name);
        let (env, r1) = vault.resolve(&s.env, Some(&scope), node)?;
        let (headers, r2) = vault.resolve(&s.headers, Some(&scope), node)?;
        red.extend(r1);
        red.extend(r2);
        out.insert(
            name.clone(),
            McpServer {
                env,
                headers,
                ..s.clone()
            },
        );
    }
    Ok((out, red))
}

/// Start an MCP server (or call it at its URL), ask it for its tools, and
/// stop it: does it work as configured?
pub async fn probe(
    t: &ToolCfg,
    vault: &crate::deck::secrets::Vault,
) -> Result<Vec<String>, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let one: BTreeMap<String, McpServer> = [(
        t.name.clone(),
        McpServer {
            command: t.command.clone(),
            args: t.args.clone(),
            env: t.env.clone(),
            url: t.url.clone(),
            headers: t.headers.clone(),
        },
    )]
    .into_iter()
    .collect();
    let (one, _) = resolve(&one, vault, crate::deck::nodes::LOCAL)?;
    let s = &one[&t.name];
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "indra", "version": env!("CARGO_PKG_VERSION") } } });
    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let names = |v: &Value| -> Vec<String> {
        v["result"]["tools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    if s.url.is_empty() {
        let program =
            crate::deck::config::which(&s.command).unwrap_or_else(|| PathBuf::from(&s.command));
        let mut child = tokio::process::Command::new(program)
            .args(&s.args)
            .envs(&s.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("cannot start {}: {}", s.command, e))?;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let mut lines = tokio::io::BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let send = format!("{}\n{}\n{}\n", init, initialized, list);
        stdin
            .write_all(send.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let found = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while let Ok(Some(l)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&l) {
                    if v["id"] == 2 {
                        return if v.get("error").is_some() {
                            Err(format!("tools/list: {}", v["error"]))
                        } else {
                            Ok(names(&v))
                        };
                    }
                }
            }
            Err("it exited before answering".to_string())
        })
        .await
        .map_err(|_| "no answer in 30 seconds".to_string())?;
        let _ = child.kill().await;
        found
    } else {
        // Streamable HTTP: initialize, then list with the session it gave.
        let (head, body) = http_post(&s.url, &s.headers, &init, None).await?;
        if !body
            .iter()
            .any(|v| v["id"] == 1 && v.get("result").is_some())
        {
            return Err(format!(
                "initialize: no result ({})",
                head.lines().next().unwrap_or("")
            ));
        }
        let session = head.lines().find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("mcp-session-id"))
                .map(|(_, v)| v.trim().to_string())
        });
        let (_, body) = http_post(&s.url, &s.headers, &list, session.as_deref()).await?;
        body.iter()
            .find(|v| v["id"] == 2)
            .map(names)
            .ok_or_else(|| "tools/list: no result".to_string())
    }
}

/// POST one JSON-RPC message through curl (headers on stdin, never on the
/// command line). The response headers, and the JSON messages in the body
/// (plain JSON or an event stream).
async fn http_post(
    url: &str,
    headers: &BTreeMap<String, String>,
    msg: &Value,
    session: Option<&str>,
) -> Result<(String, Vec<Value>), String> {
    use tokio::io::AsyncWriteExt;
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut cfg = String::from(
        "silent\nshow-error\nconnect-timeout = 10\nmax-time = 30\ninclude\nrequest = \"POST\"\n",
    );
    cfg.push_str(&format!("url = {}\n", q(url)));
    cfg.push_str("header = \"Content-Type: application/json\"\nheader = \"Accept: application/json, text/event-stream\"\n");
    for (k, v) in headers {
        cfg.push_str(&format!("header = {}\n", q(&format!("{}: {}", k, v))));
    }
    if let Some(s) = session {
        cfg.push_str(&format!(
            "header = {}\n",
            q(&format!("Mcp-Session-Id: {}", s))
        ));
    }
    cfg.push_str(&format!("data-binary = {}\n", q(&msg.to_string())));
    let curl = crate::deck::config::which("curl").ok_or("curl is not installed")?;
    let mut child = tokio::process::Command::new(curl)
        .args(["-K", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot run curl: {}", e))?;
    if let Some(mut i) = child.stdin.take() {
        i.write_all(cfg.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let (head, body) = text.split_once("\n\n").unwrap_or((text.as_str(), ""));
    let status = head.lines().next().unwrap_or("");
    if !status
        .split_whitespace()
        .nth(1)
        .map(|c| c.starts_with('2'))
        .unwrap_or(false)
    {
        return Err(format!(
            "{}: {}",
            status,
            body.chars().take(300).collect::<String>()
        ));
    }
    let mut msgs: Vec<Value> = Vec::new();
    if let Ok(v) = serde_json::from_str::<Value>(body.trim()) {
        msgs.push(v);
    } else {
        for l in body.lines() {
            if let Some(d) = l.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(d.trim()) {
                    msgs.push(v);
                }
            }
        }
    }
    Ok((head.to_string(), msgs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mcp(name: &str) -> ToolCfg {
        ToolCfg {
            name: name.into(),
            kind: "mcp".into(),
            category: "threat-intel".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "some-mcp-server".into()],
            env: [("API_KEY".to_string(), "secret:ti-key".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn tools_are_checked() {
        mcp("intel").validate().unwrap();
        let http = ToolCfg {
            name: "edr".into(),
            kind: "mcp".into(),
            url: "https://edr.example.com/mcp".into(),
            headers: [("Authorization".to_string(), "secret:edr-token".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        http.validate().unwrap();
        assert!(
            ToolCfg {
                url: "https://x/mcp".into(),
                ..mcp("both")
            }
            .validate()
            .is_err(),
            "command and url"
        );
        assert!(
            ToolCfg {
                command: String::new(),
                ..mcp("none")
            }
            .validate()
            .is_err(),
            "neither"
        );
        assert!(ToolCfg {
            headers: [("Bad Header".to_string(), "x".to_string())]
                .into_iter()
                .collect(),
            ..http.clone()
        }
        .validate()
        .is_err());
        assert!(ToolCfg {
            headers: [("X".to_string(), "a\r\nInjected: 1".to_string())]
                .into_iter()
                .collect(),
            ..http.clone()
        }
        .validate()
        .is_err());
        assert!(ToolCfg {
            kind: "qradar".into(),
            ..mcp("q")
        }
        .validate()
        .is_err());
        assert!(ToolCfg {
            category: "toys".into(),
            ..mcp("c")
        }
        .validate()
        .is_err());
        // A data source is a tool too, with its own rules.
        let wz = ToolCfg {
            name: "wazuh".into(),
            kind: "wazuh".into(),
            url: "https://wazuh:9200".into(),
            username: Some("ro".into()),
            password: Some("secret:wz".into()),
            ..Default::default()
        };
        wz.validate().unwrap();
        assert_eq!(wz.source().unwrap().kind, "wazuh");
        assert!(ToolCfg {
            password: Some("plain".into()),
            ..wz.clone()
        }
        .validate()
        .is_err());
        assert!(ToolCfg {
            command: "x".into(),
            ..wz.clone()
        }
        .validate()
        .is_err());
        assert!(mcp("intel").source().is_none());
        // The view shows which keys exist, never their values.
        let v = mcp("intel").view();
        assert_eq!(v["env"], json!(["API_KEY"]));
        assert!(!v.to_string().contains("secret:ti-key"));
    }

    #[test]
    fn launches_get_their_profiles_tools_and_picks() {
        let wz = ToolCfg {
            name: "wazuh".into(),
            kind: "wazuh".into(),
            url: "https://wazuh:9200".into(),
            profiles: vec!["blue".into()],
            ..Default::default()
        };
        let all = ToolCfg {
            profiles: vec!["*".into()],
            ..mcp("intel")
        };
        let pick = mcp("tickets");
        let off = ToolCfg {
            profiles: vec!["*".into()],
            disabled: true,
            ..mcp("off")
        };
        let tools = vec![wz, all, pick, off];
        let s = servers_for(&tools, Some("blue"), &[], true, None).unwrap();
        assert_eq!(s.keys().collect::<Vec<_>>(), ["indra-sources", "intel"]);
        assert!(s["indra-sources"].args.ends_with(&[
            "mcp".to_string(),
            "--only".to_string(),
            "wazuh".to_string()
        ]));
        let s = servers_for(&tools, Some("red"), &["tickets".into()], true, None).unwrap();
        assert_eq!(s.keys().collect::<Vec<_>>(), ["intel", "tickets"]);
        // Data sources live on the hub: not for an agent on another machine.
        let s = servers_for(&tools, Some("blue"), &[], false, None).unwrap();
        assert_eq!(s.keys().collect::<Vec<_>>(), ["intel"]);
        assert!(servers_for(&tools, None, &["nope".into()], true, None).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_stdio_server_is_asked_for_its_tools() {
        let dir = std::env::temp_dir().join(format!("indra-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("server.sh");
        std::fs::write(&script, "#!/bin/sh\nwhile read l; do case \"$l\" in *'\"id\":1'*) echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}';; *'\"id\":2'*) echo \"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":2,\\\"result\\\":{\\\"tools\\\":[{\\\"name\\\":\\\"lookup_$TI_REGION\\\"}]}}\";; esac; done\n").unwrap();
        let t = ToolCfg {
            name: "intel".into(),
            kind: "mcp".into(),
            command: "sh".into(),
            args: vec![script.display().to_string()],
            env: [("TI_REGION".to_string(), "eu".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let vault = crate::deck::secrets::Vault::open(dir.join("secrets"));
        assert_eq!(probe(&t, &vault).await.unwrap(), ["lookup_eu"]);
        let missing = ToolCfg {
            env: [("KEY".to_string(), "secret:nope".to_string())]
                .into_iter()
                .collect(),
            ..t.clone()
        };
        assert!(
            probe(&missing, &vault).await.unwrap_err().contains("nope"),
            "an unset secret fails before anything starts"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_config_file_is_private() {
        let s: BTreeMap<String, McpServer> = [(
            "intel".to_string(),
            McpServer {
                command: "x".into(),
                env: [("K".to_string(), "v".to_string())].into_iter().collect(),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect();
        let p = write_config(&s).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["intel"]["type"], "stdio");
        assert_eq!(v["mcpServers"]["intel"]["env"]["K"], "v");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_file(p);
    }
}
