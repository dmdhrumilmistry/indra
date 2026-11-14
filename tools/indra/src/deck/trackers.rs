//! Tracker connectors: Jira, GitHub Issues, ClickUp and Linear.
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

pub const KINDS: [&str; 4] = ["jira", "github", "clickup", "linear"];

/// The most items one import reads, across pages.
pub const MAX_ITEMS: usize = 500;

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

pub(crate) fn base64(input: &[u8]) -> String {
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
            "linear" => "https://api.linear.app",
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
            // A Linear API key goes as it is; an OAuth token is a bearer.
            "linear" if self.token.starts_with("lin_api_") => self.token.clone(),
            _ => format!("Bearer {}", self.token),
        }
    }

    /// One Linear GraphQL call; GraphQL errors are errors.
    async fn linear(&self, query: &str, variables: Value) -> Result<Value> {
        let v = http(
            "POST",
            &format!("{}/graphql", self.base()),
            &self.auth(),
            Some(&json!({ "query": query, "variables": variables })),
        )
        .await?;
        if let Some(e) = v
            .get("errors")
            .and_then(Value::as_array)
            .filter(|e| !e.is_empty())
        {
            return Err(format!(
                "Linear: {}",
                e.iter()
                    .filter_map(|x| x.get("message").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        Ok(v.get("data").cloned().unwrap_or(Value::Null))
    }

    /// A Linear issue's id and its team's workflow states, from its key (ENG-12).
    async fn linear_issue(&self, key: &str) -> Result<(String, Vec<Value>)> {
        let d = self
            .linear(
                "query($id:String!){ issue(id:$id){ id team{ states{ nodes{ id name } } } } }",
                json!({ "id": key }),
            )
            .await?;
        let id = s(&d, "/issue/id");
        if id.is_empty() {
            return Err(format!("Linear has no issue {}", key));
        }
        let states = d
            .pointer("/issue/team/states/nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok((id, states))
    }

    /// Open items matching the connector's filter, across pages (at most
    /// MAX_ITEMS).
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
                let mut issues: Vec<Value> = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let mut url = format!(
                        "{}/rest/api/3/search/jql?jql={}&fields=summary,description,status&maxResults=100",
                        b,
                        encode(&jql)
                    );
                    if let Some(t) = &token {
                        url.push_str(&format!("&nextPageToken={}", encode(t)));
                    }
                    let v = http("GET", &url, &self.auth(), None).await?;
                    issues.extend(
                        v.get("issues")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                    );
                    token = v
                        .get("nextPageToken")
                        .and_then(Value::as_str)
                        .filter(|t| !t.is_empty())
                        .map(String::from);
                    let last = v
                        .get("isLast")
                        .and_then(Value::as_bool)
                        .unwrap_or(token.is_none());
                    if last || token.is_none() || issues.len() >= MAX_ITEMS {
                        break;
                    }
                }
                issues.truncate(MAX_ITEMS);
                Ok(issues
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
                let mut all: Vec<Value> = Vec::new();
                for page in 1..=(MAX_ITEMS / 100) {
                    let mut url = format!(
                        "{}/repos/{}/issues?state=open&per_page=100&page={}",
                        b, p, page
                    );
                    if let Some(q) = self.cfg.query.as_ref().filter(|q| !q.trim().is_empty()) {
                        url.push_str(&format!("&labels={}", encode(q)));
                    }
                    let v = http("GET", &url, &self.auth(), None).await?;
                    let got = v.as_array().cloned().unwrap_or_default();
                    let n = got.len();
                    all.extend(got);
                    if n < 100 {
                        break;
                    }
                }
                Ok(all
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
                let mut base_url = format!(
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
                    base_url.push_str(&format!("&statuses%5B%5D={}", encode(st)));
                }
                let mut tasks: Vec<Value> = Vec::new();
                for page in 0..(MAX_ITEMS / 100) {
                    let v = http(
                        "GET",
                        &format!("{}&page={}", base_url, page),
                        &self.auth(),
                        None,
                    )
                    .await?;
                    let got = v
                        .get("tasks")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let empty = got.is_empty();
                    tasks.extend(got);
                    if empty || v.get("last_page").and_then(Value::as_bool).unwrap_or(true) {
                        break;
                    }
                }
                tasks.truncate(MAX_ITEMS);
                Ok(tasks
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
            "linear" => {
                // The team by its key (ENG); open means not completed or
                // canceled; the filter, if any, is a label name.
                let mut filter = json!({
                    "team": { "key": { "eq": p } },
                    "state": { "type": { "nin": ["completed", "canceled"] } },
                });
                if let Some(q) = self.cfg.query.as_ref().filter(|q| !q.trim().is_empty()) {
                    filter["labels"] = json!({ "some": { "name": { "eq": q.trim() } } });
                }
                let mut nodes: Vec<Value> = Vec::new();
                let mut after: Option<String> = None;
                loop {
                    let d = self
                        .linear(
                            "query($filter:IssueFilter,$after:String){ issues(first:100, after:$after, filter:$filter){ nodes{ identifier title description url state{ name } } pageInfo{ hasNextPage endCursor } } }",
                            json!({ "filter": filter, "after": after }),
                        )
                        .await?;
                    nodes.extend(
                        d.pointer("/issues/nodes")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                    );
                    let more = d
                        .pointer("/issues/pageInfo/hasNextPage")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    after = d
                        .pointer("/issues/pageInfo/endCursor")
                        .and_then(Value::as_str)
                        .map(String::from);
                    if !more || after.is_none() || nodes.len() >= MAX_ITEMS {
                        break;
                    }
                }
                nodes.truncate(MAX_ITEMS);
                Ok(nodes
                    .iter()
                    .map(|i| Remote {
                        key: s(i, "/identifier"),
                        title: s(i, "/title"),
                        body: s(i, "/description"),
                        url: s(i, "/url"),
                        state: s(i, "/state/name"),
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
            "linear" => {
                let (id, _) = self.linear_issue(key).await?;
                self.linear(
                    "mutation($input:CommentCreateInput!){ commentCreate(input:$input){ success } }",
                    json!({ "input": { "issueId": id, "body": text } }),
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
            "linear" => {
                let (id, states) = self.linear_issue(key).await?;
                let want = state.to_lowercase();
                let st = states
                    .iter()
                    .find(|x| s(x, "/name").to_lowercase() == want)
                    .ok_or_else(|| format!("Linear's team has no state '{}' for {}", state, key))?;
                let d = self
                    .linear(
                        "mutation($id:String!,$input:IssueUpdateInput!){ issueUpdate(id:$id, input:$input){ success } }",
                        json!({ "id": id, "input": { "stateId": s(st, "/id") } }),
                    )
                    .await?;
                if d.pointer("/issueUpdate/success") == Some(&json!(true)) {
                    Ok(())
                } else {
                    Err(format!("Linear did not move {} to '{}'", key, state))
                }
            }
            k => Err(format!("unknown connector kind '{}'", k)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for all four trackers, paging the way each one does.
    async fn fake() -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        use axum::extract::{Query, Request};
        use std::collections::HashMap;
        let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
        let s2 = seen.clone();
        let issues = |n: usize, from: usize| -> Vec<Value> {
            (from..from + n).map(|i| json!({ "number": i, "title": format!("t{}", i), "body": "", "html_url": "", "state": "open" })).collect()
        };
        let app = axum::Router::new()
            .route("/repos/o/r/issues", axum::routing::get(move |Query(q): Query<HashMap<String, String>>| async move {
                let mut v = if q.get("page").map(String::as_str) == Some("1") { issues(100, 0) } else { issues(30, 100) };
                v[0]["pull_request"] = json!({});
                axum::Json(json!(v))
            }))
            .route("/rest/api/3/search/jql", axum::routing::get(|Query(q): Query<HashMap<String, String>>| async move {
                let page2 = q.get("nextPageToken").map(String::as_str) == Some("t2");
                let n = if page2 { 5 } else { 100 };
                let list: Vec<Value> = (0..n).map(|i| json!({ "key": format!("SEC-{}", i), "fields": { "summary": "s", "status": { "name": "To Do" } } })).collect();
                axum::Json(if page2 { json!({ "issues": list, "isLast": true }) } else { json!({ "issues": list, "nextPageToken": "t2", "isLast": false }) })
            }))
            .route("/api/v2/list/L1/task", axum::routing::get(|Query(q): Query<HashMap<String, String>>| async move {
                let p1 = q.get("page").map(String::as_str) == Some("1");
                let n = if p1 { 3 } else { 100 };
                let list: Vec<Value> = (0..n).map(|i| json!({ "id": format!("c{}", i), "name": "n", "status": { "status": "open" } })).collect();
                axum::Json(json!({ "tasks": list, "last_page": p1 }))
            }))
            .route("/graphql", axum::routing::post(move |req: Request| {
                let seen = s2.clone();
                async move {
                    let auth = req.headers().get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    let b: Value = serde_json::from_slice(&axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap()).unwrap();
                    seen.lock().unwrap().push(json!({ "auth": auth, "body": b }));
                    let q = b["query"].as_str().unwrap_or("");
                    let v = &b["variables"];
                    axum::Json(if q.contains("issues(") {
                        if v["after"].is_null() {
                            json!({ "data": { "issues": { "nodes": [{ "identifier": "ENG-1", "title": "a", "url": "u1", "state": { "name": "Todo" } }, { "identifier": "ENG-2", "title": "b", "state": { "name": "Todo" } }], "pageInfo": { "hasNextPage": true, "endCursor": "c1" } } } })
                        } else {
                            json!({ "data": { "issues": { "nodes": [{ "identifier": "ENG-3", "title": "c", "state": { "name": "In Progress" } }], "pageInfo": { "hasNextPage": false, "endCursor": "c2" } } } })
                        }
                    } else if q.contains("issue(id") {
                        if v["id"] == "ENG-404" {
                            json!({ "data": { "issue": null }, "errors": [{ "message": "Entity not found" }] })
                        } else {
                            json!({ "data": { "issue": { "id": "uuid-1", "team": { "states": { "nodes": [{ "id": "s-rev", "name": "In Review" }, { "id": "s-done", "name": "Done" }] } } } } })
                        }
                    } else if q.contains("commentCreate") {
                        json!({ "data": { "commentCreate": { "success": true } } })
                    } else {
                        json!({ "data": { "issueUpdate": { "success": true } } })
                    })
                }
            }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (url, seen)
    }

    fn cfg(kind: &str, url: &str, project: &str, query: Option<&str>) -> ConnectorCfg {
        serde_json::from_value(json!({ "name": "c", "kind": kind, "url": url, "project": project, "query": query, "token": "secret:t" })).unwrap()
    }

    #[tokio::test]
    async fn every_tracker_pages_and_linear_works() {
        if crate::deck::config::which("curl").is_none() {
            return;
        }
        let (url, seen) = fake().await;
        fn t(c: &ConnectorCfg) -> Tracker<'_> {
            Tracker {
                cfg: c,
                token: "lin_api_k".into(),
            }
        }
        let gh = cfg("github", &url, "o/r", None);
        assert_eq!(t(&gh).list().await.unwrap().len(), 128); // 130 less a pull request per page
        let jira = cfg("jira", &url, "SEC", None);
        assert_eq!(t(&jira).list().await.unwrap().len(), 105);
        let cu = cfg("clickup", &url, "L1", None);
        assert_eq!(t(&cu).list().await.unwrap().len(), 103);

        let lin = cfg("linear", &url, "ENG", Some("agent"));
        let items = t(&lin).list().await.unwrap();
        assert_eq!(
            items.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
            vec!["ENG-1", "ENG-2", "ENG-3"]
        );
        assert_eq!(items[2].state, "In Progress");
        t(&lin).comment("ENG-1", "done by indra").await.unwrap();
        t(&lin).transition("ENG-1", "in review").await.unwrap();
        assert!(t(&lin)
            .transition("ENG-1", "Shipped")
            .await
            .unwrap_err()
            .contains("no state 'Shipped'"));
        assert!(t(&lin)
            .comment("ENG-404", "x")
            .await
            .unwrap_err()
            .contains("Entity not found"));
        let log = seen.lock().unwrap().clone();
        // A Linear API key goes as it is, not as a bearer token.
        assert_eq!(log[0]["auth"], "lin_api_k");
        let f = &log[0]["body"]["variables"]["filter"];
        assert_eq!(f["team"]["key"]["eq"], "ENG");
        assert_eq!(f["labels"]["some"]["name"]["eq"], "agent");
        assert!(log.iter().any(
            |x| x["body"]["variables"]["input"]["body"] == "done by indra"
                && x["body"]["variables"]["input"]["issueId"] == "uuid-1"
        ));
        assert!(log
            .iter()
            .any(|x| x["body"]["variables"]["input"]["stateId"] == "s-rev"));
    }

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
