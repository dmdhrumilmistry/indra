//! Claude Code on a model served by Ollama: the environment that points it
//! there, the checks that say whether a model will work, and pulling one.
//!
//! One place for all of it, so a console launch, the browser terminal and
//! `indra claude` in your own terminal set Claude Code up the same way.

use crate::deck::router;
use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Claude Code's system prompt and tool definitions alone run to about
/// 20k tokens, so a smaller context loses the start of every request.
pub const MIN_CONTEXT: u64 = 32_768;
/// What we suggest: room for the prompt, the tools and some work.
pub const GOOD_CONTEXT: u64 = 65_536;

/// The environment that puts Claude Code, every model it asks for included,
/// on `model` at the Ollama server `ollama`, through the model router (which
/// keeps every credential off Ollama). What `env` already sets wins, except
/// the base URL, which always goes through the router.
///
/// `context` is the window the server runs the model with, when known.
/// Claude Code does not know local models, assumes a huge window and would
/// never compact; CLAUDE_CODE_MAX_CONTEXT_TOKENS tells it the real one.
pub fn claude_env(
    env: &mut BTreeMap<String, String>,
    model: &str,
    ollama: &str,
    context: Option<u64>,
) -> Result<(), String> {
    // A base URL the profile set itself is where Ollama is.
    let upstream = env
        .get("ANTHROPIC_BASE_URL")
        .filter(|u| !u.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| ollama.trim_end_matches('/').to_string());
    let base = router::register(router::Route {
        ollama: upstream,
        local: vec![model.to_string()],
        all_local: true,
        ..Default::default()
    })?;
    env.insert("ANTHROPIC_BASE_URL".into(), base);
    let mut set = |k: &str, v: &str| {
        env.entry(k.to_string()).or_insert_with(|| v.to_string());
    };
    set("ANTHROPIC_AUTH_TOKEN", "ollama");
    set("ANTHROPIC_API_KEY", "");
    set("CLAUDE_CODE_OAUTH_TOKEN", "");
    // Agent files say `model: opus` or `haiku`, and Claude Code makes
    // background calls with its small model: all of them local too.
    for k in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
    ] {
        set(k, model);
    }
    // Nothing else of Anthropic's to reach: no telemetry, no update checks.
    set("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
    set(
        "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
        &context.unwrap_or(GOOD_CONTEXT).to_string(),
    );
    Ok(())
}

async fn call(base: &str, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
    let mut h = HeaderMap::new();
    h.insert("accept", HeaderValue::from_static("application/json"));
    let bytes = match &body {
        Some(b) => {
            h.insert("content-type", HeaderValue::from_static("application/json"));
            Bytes::from(b.to_string())
        }
        None => Bytes::new(),
    };
    let resp = tokio::time::timeout(
        Duration::from_secs(15),
        router::forward(base, method, path, &h, bytes, true),
    )
    .await
    .map_err(|_| format!("{}: no answer", base))??;
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 16 << 20)
        .await
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = v
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| status.to_string());
        return Err(msg);
    }
    Ok(v)
}

/// Whether two names are the same model: `qwen3` is `qwen3:latest`.
pub fn same_model(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        if s.contains(':') {
            s.to_string()
        } else {
            format!("{}:latest", s)
        }
    };
    norm(a) == norm(b)
}

/// A model's capabilities and the most context it takes (`/api/show`).
pub async fn details(base: &str, model: &str) -> Result<(Vec<String>, Option<u64>), String> {
    show(base, model).await
}

/// What `/api/show` says about a model: its capabilities and the most
/// context it can take.
async fn show(base: &str, model: &str) -> Result<(Vec<String>, Option<u64>), String> {
    let v = call(base, "POST", "api/show", Some(json!({ "model": model }))).await?;
    let caps = v
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let ctx = v
        .get("model_info")
        .and_then(Value::as_object)
        .and_then(|m| {
            m.iter()
                .find(|(k, _)| k.ends_with(".context_length"))
                .and_then(|(_, v)| v.as_u64())
        });
    Ok((caps, ctx))
}

/// Will `model` at `base` work for Claude Code? Everything found, and a
/// warning for each thing that would get in the way. `ok` is false only for
/// what stops it outright (no server, no model); the rest are warnings.
pub async fn check(base: &str, model: &str) -> Value {
    let mut warnings: Vec<String> = vec![];
    if let Err(e) = router::check_url(base) {
        return json!({ "ok": false, "reachable": false, "error": e, "warnings": [] });
    }
    let version = match call(base, "GET", "api/version", None).await {
        Ok(v) => v
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        Err(e) => {
            return json!({
                "ok": false, "reachable": false, "url": base,
                "error": format!("Ollama is not answering at {} ({}). Start it with `ollama serve`, or set the server in the profile.", base, e),
                "warnings": [],
            })
        }
    };
    if older(&version, (0, 14)) {
        warnings.push(format!(
            "Ollama {} predates the Anthropic API Claude Code speaks (0.14): update Ollama.",
            version
        ));
    }
    let models = router::ollama_models(base).await.unwrap_or_default();
    let present = models
        .iter()
        .filter_map(|m| m.get("name").and_then(Value::as_str))
        .any(|n| same_model(n, model));
    if !present {
        return json!({
            "ok": false, "reachable": true, "url": base, "version": version, "present": false,
            "error": format!("{} is not pulled on {}: pull it here, or run `ollama pull {}`.", model, base, model),
            "warnings": warnings,
        });
    }
    let (caps, max_ctx) = show(base, model).await.unwrap_or_default();
    let tools = caps.iter().any(|c| c == "tools");
    if !caps.is_empty() && !tools {
        warnings.push(format!(
            "{} does not support tool calling, and Claude Code works through tools: pick one that does (qwen3-coder, gpt-oss, qwen3, llama3.1...).",
            model
        ));
    }
    if let Some(c) = max_ctx.filter(|c| *c < MIN_CONTEXT) {
        warnings.push(format!(
            "{} takes at most {} tokens of context; Claude Code's prompt and tools alone are about 20k. Pick a model with 32k or more.",
            model, c
        ));
    }
    // The context the server actually runs it with, once it is loaded.
    let loaded = call(base, "GET", "api/ps", None)
        .await
        .ok()
        .and_then(|v| v.get("models").and_then(Value::as_array).cloned())
        .and_then(|ms| {
            ms.into_iter().find(|m| {
                m.get("name")
                    .or_else(|| m.get("model"))
                    .and_then(Value::as_str)
                    .map(|n| same_model(n, model))
                    .unwrap_or(false)
            })
        })
        .and_then(|m| m.get("context_length").and_then(Value::as_u64));
    if let Some(c) = loaded.filter(|c| *c < MIN_CONTEXT) {
        warnings.push(format!(
            "Ollama is running {} with a {}-token context, too small for Claude Code: start the server with OLLAMA_CONTEXT_LENGTH={} (or set the context length in the Ollama app).",
            model, c, GOOD_CONTEXT
        ));
    }
    // The window to tell Claude Code: what the server runs it with, else
    // what we recommend running it with (never more than the model takes).
    let context =
        loaded.or_else(|| Some(max_ctx.map(|m| m.min(GOOD_CONTEXT)).unwrap_or(GOOD_CONTEXT)));
    json!({
        "ok": true, "reachable": true, "url": base, "version": version, "present": true,
        "capabilities": caps, "tools": tools, "max_context": max_ctx, "loaded_context": loaded,
        "context": context, "warnings": warnings,
    })
}

/// `version` (like "0.13.5") is below `min`. Unparsable versions are not.
fn older(version: &str, min: (u64, u64)) -> bool {
    let mut it = version
        .trim_start_matches('v')
        .split(['.', '-'])
        .map(|p| p.parse::<u64>());
    match (it.next(), it.next()) {
        (Some(Ok(a)), Some(Ok(b))) => (a, b) < min,
        _ => false,
    }
}

/// A model name Ollama would accept, and nothing that could be an option or
/// a path: `qwen3-coder`, `gpt-oss:20b`, `hf.co/org/model:Q4_K_M`.
pub fn valid_model(m: &str) -> bool {
    !m.is_empty()
        && m.len() <= 200
        && !m.starts_with(['-', '/', '.'])
        && !m.contains("..")
        && m.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/-".contains(c))
}

/// Progress of the pulls the console started, by model.
fn pulls() -> &'static Mutex<HashMap<String, Value>> {
    static P: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();
    P.get_or_init(Mutex::default)
}

/// What the console's pulls have done so far.
pub fn pull_status() -> Value {
    json!(pulls().lock().unwrap().clone())
}

/// Pull `model` onto the server, reporting each progress line to
/// `progress` (`status`, and `completed`/`total` bytes while downloading).
pub async fn pull<F: FnMut(&Value)>(
    base: &str,
    model: &str,
    mut progress: F,
) -> Result<(), String> {
    use futures_util::StreamExt;
    router::check_url(base)?;
    if !valid_model(model) {
        return Err(format!("not a model name: {:?}", model));
    }
    let mut h = HeaderMap::new();
    h.insert("content-type", HeaderValue::from_static("application/json"));
    let resp = router::forward(
        base,
        "POST",
        "api/pull",
        &h,
        Bytes::from(json!({ "model": model, "stream": true }).to_string()),
        true,
    )
    .await?;
    if !resp.status().is_success() {
        let b = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap_or_default();
        return Err(format!("{}: {}", model, String::from_utf8_lossy(&b).trim()));
    }
    let mut stream = resp.into_body().into_data_stream();
    let mut buf = Vec::new();
    let mut last = Value::Null;
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
        while let Some(i) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            let Ok(v) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if let Some(e) = v.get("error").and_then(Value::as_str) {
                return Err(format!("{}: {}", model, e));
            }
            progress(&v);
            last = v;
        }
    }
    if last.get("status").and_then(Value::as_str) == Some("success") {
        Ok(())
    } else {
        Err(format!("{}: the pull ended before it finished", model))
    }
}

/// Start a pull in the background for the console; its progress shows in
/// `pull_status()`. A pull already running for the model is left alone.
pub fn start_pull(base: String, model: String) -> Result<(), String> {
    router::check_url(&base)?;
    if !valid_model(&model) {
        return Err(format!("not a model name: {:?}", model));
    }
    {
        let mut p = pulls().lock().unwrap();
        if p.get(&model).and_then(|v| v.get("done")) == Some(&json!(false)) {
            return Ok(());
        }
        p.insert(
            model.clone(),
            json!({ "status": "starting", "done": false, "url": base }),
        );
    }
    tokio::spawn(async move {
        let m = model.clone();
        let r = pull(&base, &model, |v| {
            let mut e = v.clone();
            if let Some(o) = e.as_object_mut() {
                o.insert("done".into(), json!(false));
            }
            pulls().lock().unwrap().insert(m.clone(), e);
        })
        .await;
        let end = match r {
            Ok(()) => json!({ "status": "success", "done": true }),
            Err(e) => json!({ "status": "failed", "error": e, "done": true }),
        };
        pulls().lock().unwrap().insert(model, end);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};

    /// A stand-in Ollama with one model that has tools and a 40k context,
    /// one without tools, a small loaded context, and a pull that streams.
    async fn fake() -> String {
        let app = axum::Router::new()
            .route("/api/version", get(|| async { axum::Json(json!({ "version": "0.15.2" })) }))
            .route("/api/tags", get(|| async {
                axum::Json(json!({ "models": [{ "name": "qwen3:0.6b" }, { "name": "gemma:2b" }, { "name": "tiny:latest" }] }))
            }))
            .route("/api/show", post(|axum::Json(b): axum::Json<Value>| async move {
                let m = b["model"].as_str().unwrap_or("").to_string();
                axum::Json(match m.as_str() {
                    "qwen3:0.6b" => json!({ "capabilities": ["completion", "tools"], "model_info": { "qwen3.context_length": 40960 } }),
                    "tiny" | "tiny:latest" => json!({ "capabilities": ["completion", "tools"], "model_info": { "llama.context_length": 8192 } }),
                    _ => json!({ "capabilities": ["completion"], "model_info": { "gemma.context_length": 8192 } }),
                })
            }))
            .route("/api/ps", get(|| async { axum::Json(json!({ "models": [{ "name": "qwen3:0.6b", "context_length": 4096 }] })) }))
            .route("/api/pull", post(|axum::Json(b): axum::Json<Value>| async move {
                if b["model"] == "nope" {
                    return "{\"error\":\"pull model manifest: file does not exist\"}\n".to_string();
                }
                "{\"status\":\"pulling manifest\"}\n{\"status\":\"pulling abc\",\"total\":100,\"completed\":50}\n{\"status\":\"success\"}\n".to_string()
            }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn checks_say_what_will_get_in_the_way() {
        let url = fake().await;
        let c = check(&url, "qwen3:0.6b").await;
        assert_eq!(c["ok"], true, "{}", c);
        assert_eq!(c["tools"], true);
        assert_eq!(c["max_context"], 40960);
        assert_eq!(c["context"], 4096); // what it is loaded with
        assert_eq!(check(&url, "tiny").await["context"], 8192); // its own maximum
                                                                // Loaded with a 4k context: the warning names the fix.
        let w = c["warnings"].to_string();
        assert!(w.contains("OLLAMA_CONTEXT_LENGTH=65536"), "{}", w);

        let c = check(&url, "gemma:2b").await;
        let w = c["warnings"].to_string();
        assert!(
            w.contains("does not support tool calling") && w.contains("at most 8192"),
            "{}",
            w
        );
        // `tiny` is `tiny:latest`.
        assert_eq!(check(&url, "tiny").await["present"], true);

        let c = check(&url, "llama3.3").await;
        assert_eq!(c["ok"], false);
        assert!(c["error"]
            .as_str()
            .unwrap()
            .contains("ollama pull llama3.3"));
        let c = check("http://127.0.0.1:9", "x").await;
        assert_eq!(c["reachable"], false);
        assert!(c["error"].as_str().unwrap().contains("ollama serve"));
    }

    #[tokio::test]
    async fn pulls_stream_progress_and_report_errors() {
        let url = fake().await;
        let mut seen = vec![];
        pull(&url, "qwen3:0.6b", |v| {
            seen.push(v["status"].as_str().unwrap_or("").to_string())
        })
        .await
        .unwrap();
        assert_eq!(seen, vec!["pulling manifest", "pulling abc", "success"]);
        let e = pull(&url, "nope", |_| {}).await.unwrap_err();
        assert!(e.contains("does not exist"), "{}", e);
        assert!(pull(&url, "--help", |_| {}).await.is_err());
        start_pull(url.clone(), "qwen3:0.6b".into()).unwrap();
        for _ in 0..50 {
            if pull_status()["qwen3:0.6b"]["done"] == true {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(pull_status()["qwen3:0.6b"]["status"], "success");
    }

    #[test]
    fn names_and_versions() {
        assert!(valid_model("qwen3-coder"));
        assert!(valid_model("gpt-oss:20b"));
        assert!(valid_model("hf.co/unsloth/Qwen3:Q4_K_M"));
        for bad in ["", "-m", "/etc/passwd", "../x", "a b", "a;b"] {
            assert!(!valid_model(bad), "{}", bad);
        }
        assert!(older("0.13.5", (0, 14)));
        assert!(!older("0.14.0", (0, 14)));
        assert!(!older("1.0.0", (0, 14)));
        assert!(!older("dev", (0, 14)));
        assert!(same_model("qwen3", "qwen3:latest"));
        assert!(!same_model("qwen3", "qwen3:0.6b"));
    }

    #[tokio::test]
    async fn claude_env_goes_through_the_router() {
        let mut env = BTreeMap::new();
        env.insert(
            "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
            "small".to_string(),
        );
        claude_env(&mut env, "qwen3:0.6b", "http://gpu:11434/", None).unwrap();
        assert_eq!(env["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], "65536");
        assert!(env["ANTHROPIC_BASE_URL"].starts_with("http://127.0.0.1:"));
        assert_eq!(env["ANTHROPIC_MODEL"], "qwen3:0.6b");
        assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "small");
        assert_eq!(env["ANTHROPIC_API_KEY"], "");
        assert_eq!(env["CLAUDE_CODE_OAUTH_TOKEN"], "");
        assert!(router::stats()
            .iter()
            .any(|s| s["ollama"] == "http://gpu:11434" && s["all_local"] == true));
    }
}
