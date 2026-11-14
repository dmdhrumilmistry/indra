//! Tracker connectors: Jira, GitHub Issues and ClickUp.
//!
//! A connector lists open items matching a filter, comments on one, and moves
//! one to a named state. Requests go through the system `curl` (Linux, macOS
//! and Windows 10+ ship it), so indra carries no TLS stack of its own; the
//! request, credentials included, is passed on curl's stdin as a config file,
//! never as arguments other processes could read.

use crate::deck::config::ConnectorCfg;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

pub type Result<T> = std::result::Result<T, String>;

pub const KINDS: [&str; 3] = ["jira", "github", "clickup"];

/// An item in a tracker.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Remote {
    pub key: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub state: String,
}

/// A connector with its token resolved.
pub struct Tracker<'a> {
    pub cfg: &'a ConnectorCfg,
    pub token: String,
}

/// Escape a value for a curl config file (a double-quoted string).
pub(crate) fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Percent-encode a query value.
pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{:02X}", b),
        })
        .collect()
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// One HTTP request through curl. The JSON response, or an error that
/// names the status and the start of the body.
async fn http(method: &str, url: &str, auth: &str, body: Option<&Value>) -> Result<Value> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("not an http(s) URL: {}", url));
    }
    let mut cfg = String::new();
    cfg.push_str("silent\nshow-error\nconnect-timeout = 10\nmax-time = 30\n");
    cfg.push_str(&format!(
        "request = {}\nurl = {}\n",
        quote(method),
        quote(url)
    ));
    cfg.push_str(&format!(
        "header = {}\n",
        quote(&format!("Authorization: {}", auth))
    ));
    cfg.push_str("header = \"Accept: application/json\"\nheader = \"User-Agent: indra\"\n");
    if let Some(b) = body {
        cfg.push_str("header = \"Content-Type: application/json\"\n");
        cfg.push_str(&format!("data-binary = {}\n", quote(&b.to_string())));
    }
    cfg.push_str("write-out = \"\\n%{http_code}\"\n");
    let curl = crate::deck::config::which("curl")
        .ok_or("curl is not installed (connectors use it for HTTPS)")?;
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
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_str()));
    let code: u16 = code.trim().parse().unwrap_or(0);
    if code == 0 {
        return Err(format!(
            "{} {}: {}",
            method,
            url,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if !(200..300).contains(&code) {
        let snippet: String = body.chars().take(300).collect();
        return Err(format!(
            "{} {} returned {}: {}",
            method,
            url,
            code,
            snippet.trim()
        ));
    }
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(body).map_err(|e| format!("{} {}: not JSON ({})", method, url, e))
}

/// Text from an Atlassian Document Format value.
fn adf_text(v: &Value) -> String {
    let mut out = String::new();
    fn walk(v: &Value, out: &mut String) {
        if let Some(t) = v.get("text").and_then(Value::as_str) {
            out.push_str(t);
        }
        if let Some(c) = v.get("content").and_then(Value::as_array) {
            for x in c {
                walk(x, out);
            }
            if matches!(
                v.get("type").and_then(Value::as_str),
                Some("paragraph" | "heading" | "listItem" | "codeBlock")
            ) {
                out.push('\n');
            }
        }
    }
    walk(v, &mut out);
    out.trim().to_string()
}

fn adf(text: &str) -> Value {
    json!({ "type": "doc", "version": 1, "content": text.split("\n\n").map(|p| json!({
        "type": "paragraph", "content": [{ "type": "text", "text": if p.is_empty() { " " } else { p } }]
    })).collect::<Vec<_>>() })
}

fn s(v: &Value, ptr: &str) -> String {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

impl Tracker<'_> {
    fn base(&self) -> String {
        let d = match self.cfg.kind.as_str() {
            "github" => "https://api.github.com",
            "clickup" => "https://api.clickup.com",
            _ => "",
        };
        self.cfg
            .url
            .clone()
            .unwrap_or_else(|| d.into())
            .trim_end_matches('/')
            .to_string()
    }

    fn auth(&self) -> String {
        match self.cfg.kind.as_str() {
            "jira" => format!(
                "Basic {}",
                base64(
                    format!("{}:{}", self.cfg.email.as_deref().unwrap_or(""), self.token)
                        .as_bytes()
                )
            ),
            "clickup" => self.token.clone(),
            _ => format!("Bearer {}", self.token),
        }
    }

    /// Open items matching the connector's filter (at most 50).
    pub async fn list(&self) -> Result<Vec<Remote>> {
        let (b, p) = (self.base(), &self.cfg.project);
        match self.cfg.kind.as_str() {
            "jira" => {
                let jql = match &self.cfg.query {
                    Some(q) if !q.trim().is_empty() => {
                        format!("project = \"{}\" AND ({}) ORDER BY created DESC", p, q)
                    }
                    _ => format!(
                        "project = \"{}\" AND statusCategory != Done ORDER BY created DESC",
                        p
                    ),
                };
                let url = format!(
                    "{}/rest/api/3/search/jql?jql={}&fields=summary,description,status&maxResults=50",
                    b,
                    encode(&jql)
                );
                let v = http("GET", &url, &self.auth(), None).await?;
                Ok(v.get("issues")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|i| {
                        let key = s(i, "/key");
                        Remote {
                            url: format!("{}/browse/{}", b, key),
                            title: s(i, "/fields/summary"),
                            body: i
                                .pointer("/fields/description")
                                .map(adf_text)
                                .unwrap_or_default(),
                            state: s(i, "/fields/status/name"),
                            key,
                        }
                    })
                    .collect())
            }
            "github" => {
                let mut url = format!("{}/repos/{}/issues?state=open&per_page=50", b, p);
                if let Some(q) = self.cfg.query.as_ref().filter(|q| !q.trim().is_empty()) {
                    url.push_str(&format!("&labels={}", encode(q)));
                }
                let v = http("GET", &url, &self.auth(), None).await?;
                Ok(v.as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter(|i| i.get("pull_request").is_none())
                    .map(|i| Remote {
                        key: i.get("number").map(|n| n.to_string()).unwrap_or_default(),
                        title: s(i, "/title"),
                        body: s(i, "/body"),
                        url: s(i, "/html_url"),
                        state: s(i, "/state"),
                    })
                    .collect())
            }
            "clickup" => {
                let mut url = format!(
                    "{}/api/v2/list/{}/task?archived=false&include_closed=false",
                    b,
                    encode(p)
                );
                for st in self
                    .cfg
                    .query
                    .iter()
                    .flat_map(|q| q.split(','))
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                {
                    url.push_str(&format!("&statuses%5B%5D={}", encode(st)));
                }
                let v = http("GET", &url, &self.auth(), None).await?;
                Ok(v.get("tasks")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|t| Remote {
                        key: s(t, "/id"),
                        title: s(t, "/name"),
                        body: {
                            let tc = s(t, "/text_content");
                            if tc.is_empty() {
                                s(t, "/description")
                            } else {
                                tc
                            }
                        },
                        url: s(t, "/url"),
                        state: s(t, "/status/status"),
                    })
                    .collect())
            }
            k => Err(format!("unknown connector kind '{}'", k)),
        }
    }

    pub async fn comment(&self, key: &str, text: &str) -> Result<()> {
        let b = self.base();
        match self.cfg.kind.as_str() {
            "jira" => {
                http(
                    "POST",
                    &format!("{}/rest/api/3/issue/{}/comment", b, encode(key)),
                    &self.auth(),
                    Some(&json!({ "body": adf(text) })),
                )
                .await
            }
            "github" => {
                http(
                    "POST",
                    &format!(
                        "{}/repos/{}/issues/{}/comments",
                        b,
                        self.cfg.project,
                        encode(key)
                    ),
                    &self.auth(),
                    Some(&json!({ "body": text })),
                )
                .await
            }
            "clickup" => {
                http(
                    "POST",
                    &format!("{}/api/v2/task/{}/comment", b, encode(key)),
                    &self.auth(),
                    Some(&json!({ "comment_text": text, "notify_all": false })),
                )
                .await
            }
            k => Err(format!("unknown connector kind '{}'", k)),
        }
        .map(|_| ())
    }

    /// Move an item to a state, by its name in the tracker.
    pub async fn transition(&self, key: &str, state: &str) -> Result<()> {
        let b = self.base();
        match self.cfg.kind.as_str() {
            "jira" => {
                let url = format!("{}/rest/api/3/issue/{}/transitions", b, encode(key));
                let v = http("GET", &url, &self.auth(), None).await?;
                let want = state.to_lowercase();
                let t = v
                    .get("transitions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .find(|t| {
                        s(t, "/to/name").to_lowercase() == want
                            || s(t, "/name").to_lowercase() == want
                    })
                    .ok_or_else(|| {
                        format!("Jira offers no transition to '{}' for {}", state, key)
                    })?;
                http(
                    "POST",
                    &url,
                    &self.auth(),
                    Some(&json!({ "transition": { "id": s(&t, "/id") } })),
                )
                .await
                .map(|_| ())
            }
            "github" => {
                let issue = format!("{}/repos/{}/issues/{}", b, self.cfg.project, encode(key));
                match state {
                    "open" | "closed" => http(
                        "PATCH",
                        &issue,
                        &self.auth(),
                        Some(&json!({ "state": state })),
                    )
                    .await
                    .map(|_| ()),
                    label => {
                        // A state is a label; drop the labels of the other mapped states.
                        for other in self
                            .cfg
                            .states
                            .values()
                            .filter(|x| x.as_str() != label && *x != "open" && *x != "closed")
                        {
                            let _ = http(
                                "DELETE",
                                &format!("{}/labels/{}", issue, encode(other)),
                                &self.auth(),
                                None,
                            )
                            .await;
                        }
                        http(
                            "POST",
                            &format!("{}/labels", issue),
                            &self.auth(),
                            Some(&json!({ "labels": [label] })),
                        )
                        .await
                        .map(|_| ())
                    }
                }
            }
            "clickup" => http(
                "PUT",
                &format!("{}/api/v2/task/{}", b, encode(key)),
                &self.auth(),
                Some(&json!({ "status": state })),
            )
            .await
            .map(|_| ()),
            k => Err(format!("unknown connector kind '{}'", k)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(base64(b"me@x.com:tok"), "bWVAeC5jb206dG9r");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(encode("a b\"=&"), "a%20b%22%3D%26");
        assert_eq!(quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        let doc = json!({ "type": "doc", "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "Fix " }, { "type": "text", "text": "login" }] },
            { "type": "paragraph", "content": [{ "type": "text", "text": "in auth/" }] }] });
        assert_eq!(adf_text(&doc), "Fix login\nin auth/");
        assert_eq!(adf_text(&adf("one\n\ntwo")), "one\ntwo");
    }
}
