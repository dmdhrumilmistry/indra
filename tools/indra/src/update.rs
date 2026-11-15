//! `indra update`: replace this binary with the newest release (or a chosen
//! one), after checking its SHA-256, the same way the install scripts do.
//!
//! Once a day, a command run in a terminal looks for a new release in the
//! background (a detached `indra update --background`) and the next one says
//! so. With `indra update --auto on` that background check also installs it.
//! `INDRA_NO_UPDATE_CHECK=1` (or running under CI) turns all of it off.

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue};
use clap::Args;
use serde_json::Value;
use std::process::ExitCode;

const REPO: &str = "dmdhrumilmistry/indra";

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Only say whether a newer release exists
    #[arg(long)]
    check: bool,
    /// Install this release (e.g. 0.7.0 or v0.7.0) instead of the newest
    #[arg(long)]
    version: Option<String>,
    /// Reinstall even when this is already the newest
    #[arg(long)]
    force: bool,
    /// Keep indra up to date by itself: on (a daily check installs new
    /// releases, checking their SHA-256) or off (it only tells you)
    #[arg(long, value_name = "on|off", value_parser = ["on", "off"])]
    auto: Option<String>,
    /// The daily check (started by indra itself)
    #[arg(long, hide = true)]
    background: bool,
}

/// What the daily check knows, in `<data dir>/update.json`.
#[derive(serde::Serialize, serde::Deserialize, Default, Debug, Clone, PartialEq)]
pub struct State {
    /// Install new releases by themselves.
    #[serde(default)]
    pub auto: bool,
    /// When the last check started (unix seconds).
    #[serde(default)]
    pub checked: u64,
    /// The newest release it found.
    #[serde(default)]
    pub latest: Option<String>,
    /// When a "new release" line was last printed.
    #[serde(default)]
    pub noticed: u64,
    /// An update the background check installed, not yet reported.
    #[serde(default)]
    pub updated: Option<String>,
    /// Why the last automatic update failed.
    #[serde(default)]
    pub error: Option<String>,
}

const DAY: u64 = 24 * 3600;

fn state_path() -> std::path::PathBuf {
    crate::paths::data_dir().join("update.json")
}

pub fn load() -> State {
    std::fs::read(state_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(st: &State) {
    let p = state_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension(format!("json.{}", std::process::id()));
    if std::fs::write(&tmp, serde_json::to_vec_pretty(st).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// No checks at all: `INDRA_NO_UPDATE_CHECK` set (not "0"), or CI.
pub fn checks_off() -> bool {
    let set = |k: &str| {
        std::env::var(k)
            .map(|v| !v.is_empty() && v != "0" && v != "false")
            .unwrap_or(false)
    };
    set("INDRA_NO_UPDATE_CHECK") || set("CI") || asset().is_none()
}

fn newer_than_here(tag: &str) -> bool {
    match (parse(tag), parse(env!("CARGO_PKG_VERSION"))) {
        (Some(t), Some(h)) => t > h,
        _ => false,
    }
}

/// What to print before a command runs, and the state to keep. Pure, so
/// tests can drive it with any clock.
pub fn notice(st: &mut State, at: u64) -> Option<String> {
    if let Some(to) = st.updated.take() {
        st.noticed = at;
        return Some(format!(
            "indra updated itself to {} (restart a running `indra serve` to use it; `indra update --auto off` stops this)",
            to
        ));
    }
    if at.saturating_sub(st.noticed) < DAY {
        return None;
    }
    if st.auto {
        if let Some(e) = st.error.clone() {
            st.noticed = at;
            return Some(format!(
                "indra could not update itself ({}); run `indra update`",
                e
            ));
        }
        return None;
    }
    let latest = st.latest.clone().filter(|t| newer_than_here(t))?;
    st.noticed = at;
    Some(format!(
        "indra {} is available (this is {}): run `indra update`, or `indra update --auto on` to stay up to date by itself",
        latest.trim_start_matches("indra-v").trim_start_matches('v'),
        env!("CARGO_PKG_VERSION")
    ))
}

/// Before a command runs in a terminal: say what the last check found, and
/// start today's check in the background if it is due.
pub fn on_start() {
    use std::io::IsTerminal;
    if checks_off() || !std::io::stderr().is_terminal() {
        return;
    }
    let mut st = load();
    let before = st.clone();
    if let Some(line) = notice(&mut st, now()) {
        eprintln!("indra: {}", line);
    }
    if st != before {
        save(&st);
    }
    maybe_check();
}

/// Start the background check when the last one is a day old.
pub fn maybe_check() {
    if checks_off() {
        return;
    }
    let mut st = load();
    let at = now();
    if at.saturating_sub(st.checked) < DAY {
        return;
    }
    st.checked = at;
    save(&st);
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["update", "--background"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0008); // no window, detached
    }
    if let Ok(mut child) = cmd.spawn() {
        // Reap it, so a long-running console leaves no zombie.
        std::thread::spawn(move || child.wait());
    }
}

/// For the console: this version, the newest known, and the setting.
pub fn summary() -> Value {
    let st = load();
    let latest = st.latest.clone().filter(|t| newer_than_here(t));
    serde_json::json!({
        "current": env!("CARGO_PKG_VERSION"),
        "available": latest.map(|t| t.trim_start_matches("indra-v").trim_start_matches('v').to_string()),
        "auto": st.auto,
        "error": if st.auto { st.error.clone() } else { None },
        "checks": !checks_off(),
    })
}

/// The release asset built for this machine (see .github/workflows/indra.yml).
pub fn asset() -> Option<&'static str> {
    let a = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "indra-x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "indra-aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "indra-x86_64-apple-darwin",
        ("macos", "aarch64") => "indra-aarch64-apple-darwin",
        ("windows", "x86_64") => "indra-x86_64-pc-windows-msvc.exe",
        _ => return None,
    };
    Some(a)
}

/// `0.2.10` as (0, 2, 10); anything after a `-` is ignored.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim_start_matches("indra-v").trim_start_matches('v');
    let mut it = v
        .split('-')
        .next()?
        .split('.')
        .map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

/// GET a URL, following redirects (release downloads redirect to a CDN).
async fn get(url: &str) -> Result<Bytes, String> {
    let mut url = url.to_string();
    for _ in 0..6 {
        let uri: axum::http::Uri = url.parse().map_err(|_| format!("bad URL {}", url))?;
        let base = format!(
            "{}://{}",
            uri.scheme_str().unwrap_or("https"),
            uri.authority().map(|a| a.as_str()).unwrap_or("")
        );
        let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
        let mut h = HeaderMap::new();
        h.insert("user-agent", HeaderValue::from_static("indra-update"));
        h.insert("accept", HeaderValue::from_static("*/*"));
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            crate::deck::router::forward(&base, "GET", path, &h, Bytes::new(), true),
        )
        .await
        .map_err(|_| format!("{}: timed out", url))??;
        let st = resp.status();
        if st.is_redirection() {
            let next = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or("a redirect without a location")?
                .to_string();
            url = if next.starts_with('/') {
                format!("{}{}", base, next)
            } else {
                next
            };
            continue;
        }
        let body = axum::body::to_bytes(resp.into_body(), 1 << 30)
            .await
            .map_err(|e| e.to_string())?;
        if !st.is_success() {
            return Err(format!("{} answered {}", url, st));
        }
        return Ok(body);
    }
    Err("too many redirects".into())
}

/// A release tag of indra: `v0.7.0`, or the older `indra-v0.6.0`.
fn is_release_tag(t: &str) -> bool {
    let rest = t.strip_prefix("indra-v").or_else(|| t.strip_prefix('v'));
    rest.is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
}

/// The published releases' tags (not drafts or pre-releases).
async fn release_tags(api: &str) -> Result<Vec<String>, String> {
    let body = get(&format!("{}?per_page=50", api)).await?;
    let list: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["draft"] != true && r["prerelease"] != true)
        .filter_map(|r| r["tag_name"].as_str())
        .filter(|t| is_release_tag(t))
        .map(String::from)
        .collect())
}

/// The newest indra release's tag, by version.
async fn newest(api: &str) -> Result<String, String> {
    release_tags(api)
        .await?
        .into_iter()
        .filter(|t| parse(t).is_some())
        .max_by_key(|t| parse(t))
        .ok_or_else(|| "no indra release found".to_string())
}

/// The tag of a given version: `v0.7.0`, or `indra-v0.5.0` for older ones.
async fn tag_of(api: &str, want: &str) -> Result<String, String> {
    let w = parse(want).ok_or_else(|| format!("{} is not a version", want))?;
    release_tags(api)
        .await?
        .into_iter()
        .find(|t| parse(t) == Some(w))
        .ok_or_else(|| format!("there is no release {}", want))
}

pub fn run(a: UpdateArgs) -> Result<ExitCode, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(go(a))
}

fn api() -> String {
    std::env::var("INDRA_RELEASES_URL")
        .unwrap_or_else(|_| format!("https://api.github.com/repos/{}/releases", REPO))
}

/// The daily check: note the newest release, and install it when automatic
/// updates are on.
async fn background() -> Result<ExitCode, String> {
    let found = newest(&api()).await;
    let mut st = load();
    st.checked = now();
    let tag = match found {
        Ok(t) => t,
        Err(e) => {
            save(&st);
            return Err(e);
        }
    };
    st.latest = Some(tag.clone());
    if st.auto && newer_than_here(&tag) {
        match install(&tag).await {
            Ok(_) => {
                st.updated = Some(tag);
                st.error = None;
            }
            Err(e) => st.error = Some(e),
        }
    }
    save(&st);
    Ok(ExitCode::SUCCESS)
}

async fn go(a: UpdateArgs) -> Result<ExitCode, String> {
    if a.background {
        return background().await;
    }
    if let Some(v) = &a.auto {
        let mut st = load();
        st.auto = v == "on";
        st.error = None;
        // Check soon, not a day from the last check.
        st.checked = 0;
        save(&st);
        println!(
            "{}",
            if st.auto {
                "Automatic updates are on: once a day, when you use indra, it looks for a new release in the background and installs it (checking its SHA-256). A running `indra serve` uses it after a restart. Turn off: indra update --auto off"
            } else {
                "Automatic updates are off: indra tells you when a new release is out, and `indra update` installs it."
            }
        );
        if checks_off() {
            println!("(Checks are off here: INDRA_NO_UPDATE_CHECK or CI is set.)");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let api = api();
    asset().ok_or("there is no prebuilt indra for this machine; build it from source")?;
    let tag = match &a.version {
        Some(v) => tag_of(&api, v).await?,
        None => newest(&api).await?,
    };
    let here = env!("CARGO_PKG_VERSION");
    let newer = match (parse(&tag), parse(here)) {
        (Some(t), Some(h)) => t > h,
        _ => true,
    };
    if a.version.is_none() {
        let mut st = load();
        st.latest = Some(tag.clone());
        st.checked = now();
        save(&st);
    }
    if a.check {
        println!(
            "{}",
            if newer {
                format!(
                    "indra {} is installed; {} is available: run `indra update`",
                    here, tag
                )
            } else {
                format!("indra {} is the newest ({})", here, tag)
            }
        );
        println!(
            "Automatic updates: {}",
            if load().auto {
                "on (indra update --auto off to stop)"
            } else {
                "off (indra update --auto on to stay up to date by itself)"
            }
        );
        return Ok(ExitCode::SUCCESS);
    }
    if !newer && !a.force && a.version.is_none() {
        println!("indra {} is already the newest ({}).", here, tag);
        return Ok(ExitCode::SUCCESS);
    }
    let exe = install(&tag).await?;
    println!("Updated indra {} -> {} ({})", here, tag, exe.display());
    Ok(ExitCode::SUCCESS)
}

/// Download `tag` for this machine, check its SHA-256 and put it in place of
/// this binary. Shell completions set up earlier are refreshed from it.
async fn install(tag: &str) -> Result<std::path::PathBuf, String> {
    let downloads = std::env::var("INDRA_DOWNLOAD_URL")
        .unwrap_or_else(|_| format!("https://github.com/{}/releases/download", REPO));
    let asset =
        asset().ok_or("there is no prebuilt indra for this machine; build it from source")?;
    eprintln!("indra: downloading {} {} ...", tag, asset);
    let bin = get(&format!("{}/{}/{}", downloads, tag, asset)).await?;
    let sum = get(&format!("{}/{}/{}.sha256", downloads, tag, asset)).await?;
    let want = String::from_utf8_lossy(&sum)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    let got = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&bin)
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    };
    if want.len() != 64 || want != got {
        return Err(format!(
            "checksum mismatch (expected {}, got {}); nothing was changed",
            want, got
        ));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let dir = exe.parent().ok_or("cannot tell where indra is installed")?;
    let fresh = dir.join(format!(".indra-update-{}", std::process::id()));
    std::fs::write(&fresh, &bin).map_err(|e| {
        // Leave no half-written copy behind (a full disk, say).
        let _ = std::fs::remove_file(&fresh);
        format!(
            "cannot write to {} ({}); reinstall with the install script, or run this with permission to write there",
            dir.display(),
            e
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755)).map_err(|e| {
            let _ = std::fs::remove_file(&fresh);
            e.to_string()
        })?;
    }
    // A running program can be renamed (Windows) or replaced (Unix) in place.
    #[cfg(windows)]
    {
        let old = exe.with_extension("old.exe");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old).map_err(|e| e.to_string())?;
    }
    if let Err(e) = std::fs::rename(&fresh, &exe) {
        let _ = std::fs::remove_file(&fresh);
        return Err(format!("cannot replace {}: {}", exe.display(), e));
    }
    // The new binary knows its own commands: let it rewrite the completions.
    let _ = std::process::Command::new(&exe)
        .args(["completions", "--refresh"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_assets() {
        assert_eq!(parse("indra-v0.2.10"), Some((0, 2, 10)));
        assert_eq!(parse("0.3"), Some((0, 3, 0)));
        assert_eq!(parse("v1.0.0-rc1"), Some((1, 0, 0)));
        assert!(parse("indra-v0.10.0") > parse("0.9.9"));
        assert_eq!(parse("nope"), None);
        assert!(asset().is_some(), "every CI platform has a release asset");
    }

    #[test]
    fn the_daily_notice() {
        let t0 = 10 * DAY;
        // Nothing known yet: quiet.
        let mut st = State::default();
        assert_eq!(notice(&mut st, t0), None);
        // An older or the same release: quiet.
        st.latest = Some(format!("indra-v{}", env!("CARGO_PKG_VERSION")));
        assert_eq!(notice(&mut st, t0), None);
        // A newer one: said once a day.
        st.latest = Some("indra-v99.0.0".into());
        let line = notice(&mut st, t0).unwrap();
        assert!(line.contains("99.0.0 is available") && line.contains("--auto on"));
        assert_eq!(notice(&mut st, t0 + 3600), None);
        assert!(notice(&mut st, t0 + DAY + 1).is_some());
        // With automatic updates the background check installs it: no nagging...
        let mut st = State {
            auto: true,
            latest: Some("indra-v99.0.0".into()),
            ..Default::default()
        };
        assert_eq!(notice(&mut st, t0), None);
        // ...but the next command says it happened, once.
        st.updated = Some("indra-v99.0.0".into());
        assert!(notice(&mut st, t0)
            .unwrap()
            .contains("updated itself to indra-v99.0.0"));
        assert_eq!(st.updated, None);
        // And says why it could not.
        st.error = Some("cannot write to /usr/local/bin".into());
        assert_eq!(notice(&mut st, t0 + 60), None, "not twice in a day");
        assert!(notice(&mut st, t0 + 2 * DAY)
            .unwrap()
            .contains("could not update itself"));
    }

    #[tokio::test]
    async fn finds_the_newest_indra_release_and_follows_redirects() {
        let app = axum::Router::new()
            .route(
                "/releases",
                axum::routing::get(|| async {
                    axum::Json(serde_json::json!([
                        { "tag_name": "v0.9.0", "draft": true },
                        { "tag_name": "v0.2.0" },
                        { "tag_name": "v0.7.0" },
                        { "tag_name": "indra-v0.4.0", "draft": true },
                        { "tag_name": "v0.2.0" },
                        { "tag_name": "indra-v0.3.0-rc1", "prerelease": true },
                        { "tag_name": "indra-v0.3.0" },
                        { "tag_name": "indra-v0.2.0" }
                    ]))
                }),
            )
            .route(
                "/moved",
                axum::routing::get(|| async { axum::response::Redirect::temporary("/file") }),
            )
            .route("/file", axum::routing::get(|| async { "binary" }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        assert_eq!(
            newest(&format!("{}/releases", url)).await.unwrap(),
            "v0.7.0"
        );
        assert_eq!(
            tag_of(&format!("{}/releases", url), "0.3.0").await.unwrap(),
            "indra-v0.3.0"
        );
        assert_eq!(
            tag_of(&format!("{}/releases", url), "v0.7.0")
                .await
                .unwrap(),
            "v0.7.0"
        );
        assert!(tag_of(&format!("{}/releases", url), "0.8.0").await.is_err());
        assert_eq!(
            newest(&format!("{}/releases", url)).await.unwrap(),
            "v0.7.0"
        );
        assert_eq!(
            &get(&format!("{}/moved", url)).await.unwrap()[..],
            b"binary"
        );
        assert!(get(&format!("{}/missing", url)).await.is_err());
    }
}
