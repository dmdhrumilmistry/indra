//! Marketplaces: browse plugins and skills from Claude Code plugin
//! marketplaces (and plain skill repositories), and install what they hold
//! where agents will find it.
//!
//! A marketplace is a git repository (or a local folder) with a
//! `.claude-plugin/marketplace.json`, the format Claude Code uses; a repository
//! without one is read as a set of skills (every `SKILL.md`). Fetching is
//! explicit (Refresh) and uses the system `git`; nothing from a marketplace is
//! ever executed. Installing copies a plugin's skills, agents and commands
//! into the chosen place, and records every file it wrote, so updating and
//! removing touch nothing else.

use crate::deck::config::Settings;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

/// Marketplaces offered without any configuration.
pub fn builtins() -> Vec<crate::deck::config::MarketplaceCfg> {
    let m = |name: &str, source: &str, description: &str| crate::deck::config::MarketplaceCfg {
        name: name.into(),
        source: source.into(),
        description: description.into(),
        ..Default::default()
    };
    vec![
        m(
            "indra",
            "dmdhrumilmistry/indra",
            "The indra security-review plugin",
        ),
        m(
            "anthropic-skills",
            "anthropics/skills",
            "Anthropic's example and document skills",
        ),
        m(
            "claude-plugins-official",
            "anthropics/claude-plugins-official",
            "The official Claude Code plugin directory",
        ),
    ]
}

/// Configured marketplaces first, then the built-ins they do not replace.
pub fn all(s: &Settings) -> Vec<crate::deck::config::MarketplaceCfg> {
    let mut v = s.marketplaces.clone();
    for b in builtins() {
        if !v.iter().any(|m| m.name == b.name) {
            v.push(b);
        }
    }
    v
}

fn now() -> u64 {
    crate::deck::runs::now()
}

/// Where a source is cloned to: `owner/repo` is GitHub; a URL is git; a
/// path that exists is used in place.
pub enum Source {
    Git(String),
    Local(PathBuf),
}

pub fn source_of(s: &str, base: &Path) -> Result<Source, String> {
    let s = s.trim();
    if s.starts_with("https://")
        || s.starts_with("http://")
        || s.starts_with("git@")
        || s.ends_with(".git")
    {
        if s.starts_with("http://") {
            return Err("use https:// for a marketplace".into());
        }
        return Ok(Source::Git(s.to_string()));
    }
    let local = crate::deck::config::expand(base, s);
    if local.is_dir() {
        return Ok(Source::Local(local));
    }
    let parts: Vec<&str> = s.split('/').collect();
    let ok = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            && p != "."
            && p != ".."
    };
    if parts.len() == 2 && parts.iter().all(|p| ok(p)) {
        return Ok(Source::Git(format!("https://github.com/{}.git", s)));
    }
    Err(format!(
        "'{}' is not owner/repo, a git URL, or an existing folder",
        s
    ))
}

fn safe_name(s: &str) -> String {
    let n: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let n = n.trim_matches(|c| c == '-' || c == '.').to_string();
    if n.is_empty() {
        "x".into()
    } else {
        n.chars().take(80).collect()
    }
}

async fn git(args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let git = crate::deck::config::which("git")
        .ok_or("git is not installed (marketplaces are fetched with it)")?;
    let mut c = tokio::process::Command::new(git);
    c.args([
        "-c",
        "protocol.file.allow=user",
        "-c",
        "core.hooksPath=/dev/null",
    ])
    .args(args)
    .env("GIT_TERMINAL_PROMPT", "0")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(180), c.output())
        .await
        .map_err(|_| "git took longer than 3 minutes".to_string())?
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Fetch `url` at `rev` (a branch, tag or commit; None: the default branch)
/// into `dir`, shallowly. Returns the commit.
pub async fn fetch_git(url: &str, rev: Option<&str>, dir: &Path) -> Result<String, String> {
    if !dir.join(".git").is_dir() {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        git(&["init", "-q"], Some(dir)).await?;
        git(&["remote", "add", "origin", url], Some(dir)).await?;
    } else {
        git(&["remote", "set-url", "origin", url], Some(dir)).await?;
    }
    let want = rev.unwrap_or("HEAD");
    git(
        &["fetch", "-q", "--depth", "1", "--no-tags", "origin", want],
        Some(dir),
    )
    .await?;
    git(&["checkout", "-q", "--force", "FETCH_HEAD"], Some(dir)).await?;
    git(&["clean", "-qfdx"], Some(dir)).await?;
    git(&["rev-parse", "HEAD"], Some(dir)).await
}

/// What indra knows about a marketplace on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FetchState {
    pub fetched: Option<u64>,
    pub commit: Option<String>,
    pub error: Option<String>,
}

pub struct Market {
    root: PathBuf,
    installed_path: PathBuf,
    state: Mutex<std::collections::HashMap<String, FetchState>>,
    installed: Mutex<Vec<Installed>>,
    busy: tokio::sync::Mutex<()>,
}

/// One install: what, where, and every path it created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installed {
    pub id: u64,
    pub marketplace: String,
    pub plugin: String,
    pub version: Option<String>,
    pub commit: Option<String>,
    /// claude-user | claude-project | codex-user | harness
    pub dest: String,
    /// The folder the files went under.
    pub base: String,
    pub items: Vec<String>,
    pub paths: Vec<String>,
    pub installed: u64,
    /// Where each kind went, to install again on update.
    #[serde(default)]
    pub skills_dir: Option<String>,
    #[serde(default)]
    pub agents_dir: Option<String>,
    #[serde(default)]
    pub commands_dir: Option<String>,
}

impl Market {
    pub fn open(data_dir: &Path) -> Market {
        let installed_path = data_dir.join("installed.json");
        let installed = std::fs::read_to_string(&installed_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let state = std::fs::read_to_string(data_dir.join("marketplaces").join("state.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Market {
            root: data_dir.join("marketplaces"),
            installed_path,
            state: Mutex::new(state),
            installed: Mutex::new(installed),
            busy: tokio::sync::Mutex::new(()),
        }
    }

    fn save_state(&self) {
        let st = self.state.lock().unwrap();
        let _ = std::fs::create_dir_all(&self.root);
        if let Ok(t) = serde_json::to_string_pretty(&*st) {
            let _ = crate::deck::confedit::write_atomic(&self.root.join("state.json"), &t);
        }
    }

    fn save_installed(&self, v: &[Installed]) {
        if let Ok(t) = serde_json::to_string_pretty(v) {
            let _ = crate::deck::confedit::write_atomic(&self.installed_path, &t);
        }
    }

    pub fn state(&self, name: &str) -> FetchState {
        self.state
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    pub fn installed(&self) -> Vec<Installed> {
        self.installed.lock().unwrap().clone()
    }

    /// The folder a marketplace is read from.
    pub fn dir(
        &self,
        m: &crate::deck::config::MarketplaceCfg,
        base: &Path,
    ) -> Result<PathBuf, String> {
        Ok(match source_of(&m.source, base)? {
            Source::Local(p) => p,
            Source::Git(_) => self.root.join(safe_name(&m.name)),
        })
    }

    /// Fetch (or re-fetch) a marketplace.
    pub async fn refresh(
        &self,
        m: &crate::deck::config::MarketplaceCfg,
        base: &Path,
    ) -> Result<FetchState, String> {
        let _one = self.busy.lock().await;
        let r = match source_of(&m.source, base)? {
            Source::Local(p) => Ok(git(&["rev-parse", "HEAD"], Some(&p)).await.ok()),
            Source::Git(url) => fetch_git(
                &url,
                m.git_ref.as_deref(),
                &self.root.join(safe_name(&m.name)),
            )
            .await
            .map(Some),
        };
        let st = match r {
            Ok(commit) => FetchState {
                fetched: Some(now()),
                commit,
                error: None,
            },
            Err(e) => FetchState {
                error: Some(e),
                ..self.state(&m.name)
            },
        };
        self.state
            .lock()
            .unwrap()
            .insert(m.name.clone(), st.clone());
        self.save_state();
        match &st.error {
            Some(e) => Err(e.clone()),
            None => Ok(st),
        }
    }

    /// Where an external plugin is fetched to.
    fn plugin_dir(&self, market: &str, plugin: &str) -> PathBuf {
        self.root
            .join("_plugins")
            .join(safe_name(market))
            .join(safe_name(plugin))
    }

    /// The folder holding a plugin's files, fetching it when it lives in
    /// another repository.
    pub async fn plugin_root(
        &self,
        mdir: &Path,
        market: &str,
        p: &Value,
    ) -> Result<(PathBuf, Option<String>), String> {
        let src = p.get("source").cloned().unwrap_or(Value::Null);
        let inside = |rel: &str| -> Result<PathBuf, String> {
            let rel = rel.trim_start_matches("./");
            let path = if rel.is_empty() || rel == "." {
                mdir.to_path_buf()
            } else {
                mdir.join(rel)
            };
            let canon = std::fs::canonicalize(&path)
                .map_err(|_| format!("{} is not in the marketplace", rel))?;
            let base = std::fs::canonicalize(mdir).map_err(|e| e.to_string())?;
            if !canon.starts_with(&base) {
                return Err("plugin path leaves the marketplace".into());
            }
            Ok(canon)
        };
        match &src {
            Value::String(rel) => Ok((inside(rel)?, None)),
            Value::Object(o) => {
                let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
                let kind = s("source").unwrap_or_default();
                let url = match kind.as_str() {
                    "github" => s("repo").map(|r| format!("https://github.com/{}.git", r)),
                    "url" | "git" | "git-subdir" => s("url"),
                    "command" | "npm" | "archive" => {
                        return Err(format!(
                        "this plugin comes from a {} source; install it with Claude Code (native)",
                        kind
                    ))
                    }
                    _ => None,
                }
                .ok_or_else(|| format!("unsupported plugin source '{}'", kind))?;
                if let Source::Local(_) = source_of(&url, Path::new("/nonexistent"))? {
                    return Err("plugin sources must be git URLs".into());
                }
                let rev = s("sha").or_else(|| s("ref"));
                let name = p.get("name").and_then(Value::as_str).unwrap_or("plugin");
                let dir = self.plugin_dir(market, name);
                let commit = {
                    let _one = self.busy.lock().await;
                    fetch_git(&url, rev.as_deref(), &dir).await?
                };
                let root = match s("path").filter(|x| !x.is_empty()) {
                    Some(sub) => {
                        let sub = sub.trim_start_matches("./").trim_matches('/');
                        if sub.split('/').any(|c| c == ".." || c.is_empty()) {
                            return Err("bad plugin path".into());
                        }
                        dir.join(sub)
                    }
                    None => dir,
                };
                if !root.is_dir() {
                    return Err(format!("{} has no folder {}", url, root.display()));
                }
                Ok((root, Some(commit)))
            }
            _ => Err("this plugin names no source".into()),
        }
    }
}

/// The plugins in a fetched marketplace: from `marketplace.json`, else one
/// entry per `SKILL.md` found.
pub fn catalog(mdir: &Path) -> Result<Value, String> {
    let mf = mdir.join(".claude-plugin").join("marketplace.json");
    if let Ok(t) = std::fs::read_to_string(&mf) {
        let v: Value = serde_json::from_str(&t).map_err(|e| format!("marketplace.json: {}", e))?;
        let plugins: Vec<Value> = v
            .get("plugins")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.get("name").and_then(Value::as_str).is_some())
            .map(|p| {
                let kind = match p.get("source") {
                    Some(Value::String(_)) => "here".to_string(),
                    Some(Value::Object(o)) => o.get("source").and_then(Value::as_str).unwrap_or("?").to_string(),
                    _ => "?".into(),
                };
                json!({
                    "name": p["name"], "description": p.get("description").cloned().unwrap_or(Value::Null),
                    "category": p.get("category").cloned().unwrap_or(Value::Null),
                    "version": p.get("version").cloned().unwrap_or(Value::Null),
                    "homepage": p.get("homepage").cloned().unwrap_or(Value::Null),
                    "author": p.get("author").and_then(|a| a.get("name")).cloned().unwrap_or(Value::Null),
                    "installable": !matches!(kind.as_str(), "command" | "npm" | "archive"),
                    "source_kind": kind,
                    "skills": p.get("skills").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        return Ok(
            json!({ "name": v.get("name"), "description": v.get("description").or_else(|| v.pointer("/metadata/description")), "plugins": plugins }),
        );
    }
    // A plain repository of skills.
    let mut plugins = Vec::new();
    for s in find_skills(mdir, 6) {
        let rel = s
            .strip_prefix(mdir)
            .unwrap_or(&s)
            .to_string_lossy()
            .replace('\\', "/");
        let (name, desc) = skill_meta(&s.join("SKILL.md"));
        plugins.push(
            json!({ "name": name, "description": desc, "category": "skill", "source_kind": "here",
                             "installable": true, "skills": [format!("./{}", rel)] }),
        );
    }
    if plugins.is_empty() {
        return Err("no .claude-plugin/marketplace.json and no SKILL.md files here".into());
    }
    Ok(json!({ "name": null, "description": null, "plugins": plugins }))
}

/// The plugin entry in a marketplace, as written there (or as `catalog` made it).
pub fn plugin_entry(mdir: &Path, name: &str) -> Result<Value, String> {
    let mf = mdir.join(".claude-plugin").join("marketplace.json");
    if let Ok(t) = std::fs::read_to_string(&mf) {
        let v: Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
        return v
            .get("plugins")
            .and_then(Value::as_array)
            .and_then(|a| {
                a.iter()
                    .find(|p| p.get("name").and_then(Value::as_str) == Some(name))
                    .cloned()
            })
            .ok_or_else(|| format!("no plugin '{}' in this marketplace", name));
    }
    let c = catalog(mdir)?;
    let p = c["plugins"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["name"] == name).cloned())
        .ok_or_else(|| format!("no skill '{}' here", name))?;
    Ok(json!({ "name": name, "source": "./", "skills": p["skills"] }))
}

fn find_skills(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(d: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth == 0 || out.len() > 2000 {
            return;
        }
        if d.join("SKILL.md").is_file() {
            out.push(d.to_path_buf());
            return;
        }
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut ents: Vec<_> = rd.flatten().collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with('.') || n == "node_modules" {
                continue;
            }
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                walk(&e.path(), depth - 1, out);
            }
        }
    }
    walk(dir, depth, &mut out);
    out
}

fn skill_meta(path: &Path) -> (String, String) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let fm = crate::mem::frontmatter::split(&text)
        .0
        .map(crate::mem::frontmatter::parse)
        .unwrap_or_default();
    let fallback = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (
        fm.get_str("name").unwrap_or(fallback),
        fm.get_str("description").unwrap_or_default(),
    )
}

/// One installable thing in a plugin.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    /// skill | agent | command
    pub kind: &'static str,
    pub name: String,
    pub description: String,
    #[serde(skip)]
    pub path: PathBuf,
}

/// What a plugin holds: skills, agents and commands to copy, plus what only
/// Claude Code's own plugin install provides (hooks, MCP and LSP servers).
pub fn contents(root: &Path, entry: &Value) -> (Vec<Item>, Vec<String>) {
    let mut items = Vec::new();
    let listed: Vec<String> = entry
        .get("skills")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let skill_dirs: Vec<PathBuf> = if !listed.is_empty() {
        listed
            .iter()
            .map(|s| root.join(s.trim_start_matches("./")))
            .filter(|p| p.join("SKILL.md").is_file() && p.starts_with(root))
            .collect()
    } else {
        find_skills(&root.join("skills"), 3)
    };
    for d in skill_dirs {
        let (name, description) = skill_meta(&d.join("SKILL.md"));
        items.push(Item {
            kind: "skill",
            name: safe_name(&name),
            description,
            path: d,
        });
    }
    if listed.is_empty() {
        for (kind, sub) in [("agent", "agents"), ("command", "commands")] {
            if let Ok(rd) = std::fs::read_dir(root.join(sub)) {
                let mut files: Vec<PathBuf> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false) && p.is_file())
                    .collect();
                files.sort();
                for f in files {
                    let (_, description) = skill_meta(&f);
                    let name = f
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    items.push(Item {
                        kind,
                        name: safe_name(&name),
                        description,
                        path: f,
                    });
                }
            }
        }
    }
    let mut native = Vec::new();
    if root.join("hooks").join("hooks.json").is_file() {
        native.push("hooks".into());
    }
    if root.join(".mcp.json").is_file() || entry.get("mcpServers").is_some() {
        native.push("MCP servers".into());
    }
    if root.join(".lsp.json").is_file() || entry.get("lspServers").is_some() {
        native.push("LSP servers".into());
    }
    (items, native)
}

/// Where an install puts each kind of item.
pub struct Dest {
    pub kind: String,
    pub base: PathBuf,
    pub skills: Option<PathBuf>,
    pub agents: Option<PathBuf>,
    pub commands: Option<PathBuf>,
}

/// Copy a skill folder or one file into `to`; returns what was created.
fn copy_item(
    from: &Path,
    to: &Path,
    overwrite: bool,
    budget: &mut u64,
) -> Result<Vec<PathBuf>, String> {
    if to.exists() && !overwrite {
        return Err(format!(
            "{} exists already (tick Replace to overwrite it)",
            to.display()
        ));
    }
    let meta = std::fs::symlink_metadata(from).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        return Ok(vec![]);
    }
    if meta.is_file() {
        *budget = budget
            .checked_sub(meta.len())
            .ok_or("the plugin is larger than 50 MB")?;
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        std::fs::copy(from, to).map_err(|e| format!("{}: {}", to.display(), e))?;
        return Ok(vec![to.to_path_buf()]);
    }
    if to.exists() {
        std::fs::remove_dir_all(to).map_err(|e| e.to_string())?;
    }
    let mut made = vec![to.to_path_buf()];
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let n = e.file_name();
        if n == ".git" {
            continue;
        }
        let mut sub = copy_item(&e.path(), &to.join(&n), true, budget)?;
        made.append(&mut sub);
    }
    made.truncate(1); // the folder stands for everything in it
    Ok(made)
}

impl Market {
    /// Copy the chosen items into `dest`, and remember what was written.
    #[allow(clippy::too_many_arguments)]
    pub fn install(
        &self,
        market: &str,
        plugin: &str,
        version: Option<String>,
        commit: Option<String>,
        items: &[Item],
        dest: &Dest,
        overwrite: bool,
    ) -> Result<Installed, String> {
        // Files this console installed before may be replaced; others only when asked.
        let mine: Vec<String> = self
            .installed()
            .iter()
            .filter(|i| {
                i.marketplace == market
                    && i.plugin == plugin
                    && i.dest == dest.kind
                    && i.base == dest.base.display().to_string()
            })
            .flat_map(|i| i.paths.clone())
            .collect();
        let mut budget: u64 = 50 << 20;
        let mut paths = Vec::new();
        let mut names = Vec::new();
        for it in items {
            let to = match it.kind {
                "skill" => dest.skills.as_ref().map(|d| d.join(&it.name)),
                "agent" => dest
                    .agents
                    .as_ref()
                    .map(|d| d.join(format!("{}.md", it.name))),
                _ => dest
                    .commands
                    .as_ref()
                    .map(|d| d.join(format!("{}.md", it.name))),
            };
            let Some(to) = to else { continue };
            let replace = overwrite || mine.contains(&to.display().to_string());
            for p in copy_item(&it.path, &to, replace, &mut budget)? {
                paths.push(p.display().to_string());
            }
            names.push(format!("{} {}", it.kind, it.name));
        }
        if names.is_empty() {
            let kinds: Vec<&str> = items.iter().map(|i| i.kind).collect();
            return Err(format!(
                "nothing to install: {} has no folder for {} (a harness needs patterns like skills/*/SKILL.md, agents/*.md)",
                dest.base.display(),
                if kinds.is_empty() { "these items".to_string() } else { kinds.join(" or ") }
            ));
        }
        let mut all = self.installed.lock().unwrap();
        let same = |i: &Installed| {
            i.marketplace == market
                && i.plugin == plugin
                && i.dest == dest.kind
                && i.base == dest.base.display().to_string()
        };
        // Installing into the same place again adds to (and keeps) that install.
        let (id, mut items_all, mut paths_all) = match all.iter().position(same) {
            Some(k) => {
                let old = all.remove(k);
                (old.id, old.items, old.paths)
            }
            None => (
                all.iter().map(|i| i.id).max().unwrap_or(0) + 1,
                vec![],
                vec![],
            ),
        };
        for n in names {
            if !items_all.contains(&n) {
                items_all.push(n);
            }
        }
        for p in paths {
            if !paths_all.contains(&p) {
                paths_all.push(p);
            }
        }
        let rec = Installed {
            id,
            marketplace: market.into(),
            plugin: plugin.into(),
            version,
            commit,
            dest: dest.kind.clone(),
            base: dest.base.display().to_string(),
            items: items_all,
            paths: paths_all,
            installed: now(),
            skills_dir: dest.skills.as_ref().map(|p| p.display().to_string()),
            agents_dir: dest.agents.as_ref().map(|p| p.display().to_string()),
            commands_dir: dest.commands.as_ref().map(|p| p.display().to_string()),
        };
        all.push(rec.clone());
        self.save_installed(&all);
        Ok(rec)
    }

    /// Remove what an install wrote (and only that).
    pub fn uninstall(&self, id: u64) -> Result<Installed, String> {
        let mut all = self.installed.lock().unwrap();
        let i = all
            .iter()
            .position(|x| x.id == id)
            .ok_or("no such install")?;
        let rec = all.remove(i);
        for p in &rec.paths {
            let p = Path::new(p);
            if !p.starts_with(&rec.base) {
                continue;
            }
            let _ = if p.is_dir() {
                std::fs::remove_dir_all(p)
            } else {
                std::fs::remove_file(p)
            };
        }
        self.save_installed(&all);
        Ok(rec)
    }
}

// ---- API ----------------------------------------------------------------------------

use crate::deck::web::{bad, forbidden, internal, not_found, ApiResult, App};
use axum::extract::{Path as UrlPath, Query, State};
use axum::Json;
use std::sync::Arc;

/// Relative marketplace folders resolve against the config file's folder.
fn base_dir(s: &Settings) -> PathBuf {
    s.config_path
        .as_ref()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

fn find(s: &Settings, name: &str) -> ApiResult<crate::deck::config::MarketplaceCfg> {
    all(s)
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| not_found(format!("no marketplace named '{}'", name)))
}

fn writable(app: &App) -> ApiResult<()> {
    if app.settings().read_only {
        return Err(forbidden("indra is running read-only"));
    }
    Ok(())
}

pub async fn list(State(app): State<Arc<App>>) -> Json<Value> {
    let s = app.settings();
    let base = base_dir(&s);
    let builtin: Vec<String> = builtins().into_iter().map(|b| b.name).collect();
    let out: Vec<Value> = all(&s)
        .into_iter()
        .map(|m| {
            let dir = app.market.dir(&m, &base).ok();
            let count = dir.as_deref().and_then(|d| catalog(d).ok()).map(|c| c["plugins"].as_array().map(|a| a.len()).unwrap_or(0));
            let configured = s.marketplaces.iter().any(|x| x.name == m.name);
            json!({ "name": m.name, "source": m.source, "description": m.description, "ref": m.git_ref,
                    "builtin": !configured && builtin.contains(&m.name), "state": app.market.state(&m.name),
                    "plugins": count })
        })
        .collect();
    Json(json!({ "marketplaces": out, "installed": app.market.installed() }))
}

#[derive(Deserialize)]
pub struct NameQ {
    m: String,
}

pub async fn refresh(State(app): State<Arc<App>>, Json(q): Json<NameQ>) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let s = app.settings();
    let m = find(&s, &q.m)?;
    let st = app.market.refresh(&m, &base_dir(&s)).await.map_err(bad)?;
    Ok(Json(json!(st)))
}

pub async fn plugins(
    State(app): State<Arc<App>>,
    Query(q): Query<NameQ>,
) -> ApiResult<Json<Value>> {
    let s = app.settings();
    let m = find(&s, &q.m)?;
    let dir = app.market.dir(&m, &base_dir(&s)).map_err(bad)?;
    if !dir.is_dir() {
        return Err(not_found("not fetched yet: press Refresh"));
    }
    let mut c = catalog(&dir).map_err(bad)?;
    let installed = app.market.installed();
    if let Some(list) = c["plugins"].as_array_mut() {
        for p in list {
            let n = p["name"].as_str().unwrap_or("").to_string();
            let at: Vec<String> = installed
                .iter()
                .filter(|i| i.marketplace == m.name && i.plugin == n)
                .map(|i| i.dest.clone())
                .collect();
            p["installed"] = json!(at);
        }
    }
    Ok(Json(c))
}

#[derive(Deserialize)]
pub struct PluginQ {
    m: String,
    plugin: String,
}

async fn resolve(
    app: &App,
    m: &str,
    plugin: &str,
) -> ApiResult<(Value, PathBuf, Option<String>, String)> {
    let s = app.settings();
    let mk = find(&s, m)?;
    let dir = app.market.dir(&mk, &base_dir(&s)).map_err(bad)?;
    if !dir.is_dir() {
        return Err(not_found("not fetched yet: press Refresh"));
    }
    let entry = plugin_entry(&dir, plugin).map_err(not_found)?;
    let (root, commit) = app
        .market
        .plugin_root(&dir, &mk.name, &entry)
        .await
        .map_err(bad)?;
    let commit = commit.or_else(|| app.market.state(&mk.name).commit);
    Ok((entry, root, commit, mk.name))
}

/// What a plugin holds (fetching it first when it lives elsewhere).
pub async fn inspect(
    State(app): State<Arc<App>>,
    Json(q): Json<PluginQ>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let (entry, root, commit, _) = resolve(&app, &q.m, &q.plugin).await?;
    let (items, native) = contents(&root, &entry);
    Ok(Json(
        json!({ "items": items, "native": native, "commit": commit, "root": root.display().to_string(),
                    "version": entry.get("version"), "homepage": entry.get("homepage") }),
    ))
}

#[derive(Deserialize)]
pub struct InstallReq {
    m: String,
    plugin: String,
    /// claude-user | claude-project | codex-user | codex-project | harness
    dest: String,
    h: Option<String>,
    #[serde(default)]
    target: usize,
    /// "kind name" of the items to install (default: all).
    items: Option<Vec<String>>,
    #[serde(default)]
    overwrite: bool,
}

/// The folder a simple pattern like `agents/*.md` or `skills/*/SKILL.md` lists.
fn pattern_dir(root: &Path, globs: &[String], skill: bool) -> Option<PathBuf> {
    globs.iter().find_map(|g| {
        let rest = if skill {
            g.strip_suffix("/*/SKILL.md")
        } else {
            g.strip_suffix("/*.md")
        }?;
        if rest.contains('*') || rest.contains('?') {
            return None;
        }
        Some(crate::deck::config::expand(root, rest))
    })
}

fn dest_for(app: &App, r: &InstallReq) -> ApiResult<Dest> {
    let home = crate::paths::home().ok_or_else(|| internal("no home folder"))?;
    let h = app.harness(r.h.as_deref())?;
    let target = || -> ApiResult<PathBuf> {
        h.targets
            .get(r.target)
            .cloned()
            .ok_or_else(|| bad("unknown target"))
    };
    let claude = |base: PathBuf, kind: &str| Dest {
        kind: kind.into(),
        skills: Some(base.join("skills")),
        agents: Some(base.join("agents")),
        commands: Some(base.join("commands")),
        base,
    };
    let codex = |base: PathBuf, kind: &str| Dest {
        kind: kind.into(),
        skills: Some(base.join("skills")),
        agents: None,
        commands: None,
        base,
    };
    Ok(match r.dest.as_str() {
        "claude-user" => claude(home.join(".claude"), "claude-user"),
        "claude-project" => claude(target()?.join(".claude"), "claude-project"),
        "codex-user" => codex(home.join(".agents"), "codex-user"),
        "codex-project" => codex(target()?.join(".agents"), "codex-project"),
        "harness" => Dest {
            kind: format!("harness:{}", h.name),
            skills: pattern_dir(&h.root, &h.skills, true),
            agents: pattern_dir(&h.root, &h.agents, false),
            commands: pattern_dir(&h.root, &h.prompts, false),
            base: h.root.clone(),
        },
        _ => {
            return Err(bad(
                "dest must be claude-user, claude-project, codex-user, codex-project or harness",
            ))
        }
    })
}

pub async fn install(
    State(app): State<Arc<App>>,
    Json(r): Json<InstallReq>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let dest = dest_for(&app, &r)?;
    let (entry, root, commit, market) = resolve(&app, &r.m, &r.plugin).await?;
    let (mut items, _) = contents(&root, &entry);
    if let Some(want) = &r.items {
        items.retain(|i| want.contains(&format!("{} {}", i.kind, i.name)));
    }
    let version = entry
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_string);
    let rec = app
        .market
        .install(
            &market,
            &r.plugin,
            version,
            commit,
            &items,
            &dest,
            r.overwrite,
        )
        .map_err(bad)?;
    Ok(Json(json!(rec)))
}

pub async fn uninstall(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    Ok(Json(json!(app.market.uninstall(id).map_err(not_found)?)))
}

/// Fetch the plugin again and install the same items in the same places.
pub async fn update(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<u64>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let rec = app
        .market
        .installed()
        .into_iter()
        .find(|i| i.id == id)
        .ok_or_else(|| not_found("no such install"))?;
    let s = app.settings();
    if let Ok(m) = find(&s, &rec.marketplace) {
        let _ = app.market.refresh(&m, &base_dir(&s)).await;
    }
    let (entry, root, commit, market) = resolve(&app, &rec.marketplace, &rec.plugin).await?;
    let (mut items, _) = contents(&root, &entry);
    items.retain(|i| rec.items.contains(&format!("{} {}", i.kind, i.name)));
    let dest = Dest {
        kind: rec.dest.clone(),
        base: PathBuf::from(&rec.base),
        skills: rec.skills_dir.as_ref().map(PathBuf::from),
        agents: rec.agents_dir.as_ref().map(PathBuf::from),
        commands: rec.commands_dir.as_ref().map(PathBuf::from),
    };
    let version = entry
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_string);
    let next = app
        .market
        .install(&market, &rec.plugin, version, commit, &items, &dest, false)
        .map_err(bad)?;
    Ok(Json(json!(next)))
}

#[derive(Deserialize)]
pub struct NativeReq {
    m: String,
    plugin: String,
    /// user | project
    #[serde(default = "user_scope")]
    scope: String,
    h: Option<String>,
    #[serde(default)]
    target: usize,
}

fn user_scope() -> String {
    "user".into()
}

/// Install with Claude Code itself (`claude plugin marketplace add`, then
/// `claude plugin install`), which also brings hooks and MCP servers.
pub async fn native(
    State(app): State<Arc<App>>,
    Json(r): Json<NativeReq>,
) -> ApiResult<Json<Value>> {
    writable(&app)?;
    let s = app.settings();
    let mk = find(&s, &r.m)?;
    let claude = s
        .runners
        .iter()
        .find(|x| x.name == "claude")
        .and_then(|x| x.program.clone())
        .ok_or_else(|| bad("Claude Code (claude) is not installed on this machine"))?;
    if !matches!(r.scope.as_str(), "user" | "project") {
        return Err(bad("scope must be user or project"));
    }
    let dir = app.market.dir(&mk, &base_dir(&s)).map_err(bad)?;
    let mname = catalog(&dir)
        .ok()
        .and_then(|c| c["name"].as_str().map(str::to_string))
        .ok_or_else(|| bad("Claude Code installs from marketplaces with a marketplace.json; refresh this one first"))?;
    let cwd = if r.scope == "project" {
        app.harness(r.h.as_deref())?
            .targets
            .get(r.target)
            .cloned()
            .ok_or_else(|| bad("unknown target"))?
    } else {
        crate::paths::home().unwrap_or_default()
    };
    let source = match source_of(&mk.source, &base_dir(&s)).map_err(bad)? {
        Source::Local(p) => p.display().to_string(),
        Source::Git(_) => mk.source.clone(),
    };
    let run = |args: Vec<String>| {
        let claude = claude.clone();
        let cwd = cwd.clone();
        async move {
            let out = tokio::time::timeout(
                std::time::Duration::from_secs(300),
                tokio::process::Command::new(claude)
                    .args(&args)
                    .current_dir(cwd)
                    .stdin(Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| "claude took longer than 5 minutes".to_string())?
            .map_err(|e| e.to_string())?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if out.status.success() {
                Ok(text)
            } else {
                Err(format!(
                    "claude {}: {}",
                    args[..2.min(args.len())].join(" "),
                    text.trim()
                ))
            }
        }
    };
    let mut log = run(vec![
        "plugin".into(),
        "marketplace".into(),
        "add".into(),
        source,
        "--scope".into(),
        r.scope.clone(),
    ])
    .await
    .unwrap_or_else(|e| format!("{} (continuing: it may be added already)\n", e));
    log.push_str(
        &run(vec![
            "plugin".into(),
            "install".into(),
            format!("{}@{}", r.plugin, mname),
            "--scope".into(),
            r.scope.clone(),
            "-y".into(),
        ])
        .await
        .map_err(bad)?,
    );
    Ok(Json(
        json!({ "ok": true, "output": crate::mem::search::truncate(log.trim(), 4000) }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_github_git_or_local() {
        let base = std::env::temp_dir();
        assert!(
            matches!(source_of("anthropics/skills", &base), Ok(Source::Git(u)) if u == "https://github.com/anthropics/skills.git")
        );
        assert!(matches!(
            source_of("https://example.com/x.git", &base),
            Ok(Source::Git(_))
        ));
        assert!(matches!(
            source_of(base.to_str().unwrap(), &base),
            Ok(Source::Local(_))
        ));
        assert!(source_of("http://example.com/x.git", &base).is_err());
        assert!(source_of("../../etc", Path::new("/nonexistent")).is_err());
        assert!(source_of("a/b/c", &base).is_err());
    }

    #[test]
    fn a_marketplace_is_read_and_installed_and_removed() {
        let t = std::env::temp_dir().join(format!("indra-market-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let m = t.join("mk");
        let w = |rel: &str, s: &str| {
            let p = m.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        w(
            ".claude-plugin/marketplace.json",
            r#"{"name":"mk","plugins":[
            {"name":"tools","source":"./plugins/tools","description":"Tools"},
            {"name":"docs","source":"./","strict":false,"skills":["./skills/pdf"]},
            {"name":"escape","source":"../.."},
            {"name":"cmd","source":{"source":"command","command":"x"}}]}"#,
        );
        w(
            "plugins/tools/skills/lint/SKILL.md",
            "---\nname: lint\ndescription: Lints\n---\nbody",
        );
        w("plugins/tools/skills/lint/scripts/run.sh", "echo hi");
        w(
            "plugins/tools/agents/fixer.md",
            "---\ndescription: Fixes\n---\nx",
        );
        w("plugins/tools/commands/go.md", "go");
        w("plugins/tools/hooks/hooks.json", "{}");
        w(
            "skills/pdf/SKILL.md",
            "---\nname: pdf\ndescription: PDFs\n---\n",
        );
        w("skills/other/SKILL.md", "---\nname: other\n---\n");
        let c = catalog(&m).unwrap();
        assert_eq!(c["plugins"].as_array().unwrap().len(), 4);
        assert_eq!(c["plugins"][3]["installable"], false);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let market = Market::open(&t.join("data"));
        let entry = plugin_entry(&m, "tools").unwrap();
        let (root, _) = rt.block_on(market.plugin_root(&m, "mk", &entry)).unwrap();
        let (items, native) = contents(&root, &entry);
        let kinds: Vec<&str> = items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, vec!["skill", "agent", "command"]);
        assert_eq!(native, vec!["hooks".to_string()]);
        let docs = plugin_entry(&m, "docs").unwrap();
        let (droot, _) = rt.block_on(market.plugin_root(&m, "mk", &docs)).unwrap();
        assert_eq!(contents(&droot, &docs).0.len(), 1, "only the listed skill");
        assert!(rt
            .block_on(market.plugin_root(&m, "mk", &plugin_entry(&m, "escape").unwrap()))
            .is_err());
        assert!(rt
            .block_on(market.plugin_root(&m, "mk", &plugin_entry(&m, "cmd").unwrap()))
            .is_err());
        // Install into a Claude Code style folder, again (replacing its own files), then remove.
        let home = t.join("home/.claude");
        let dest = Dest {
            kind: "claude-user".into(),
            base: home.clone(),
            skills: Some(home.join("skills")),
            agents: Some(home.join("agents")),
            commands: Some(home.join("commands")),
        };
        let rec = market
            .install("mk", "tools", None, None, &items, &dest, false)
            .unwrap();
        assert!(home.join("skills/lint/scripts/run.sh").is_file());
        assert!(home.join("agents/fixer.md").is_file() && home.join("commands/go.md").is_file());
        assert!(market
            .install("mk", "tools", None, None, &items, &dest, false)
            .is_ok());
        std::fs::write(home.join("agents/mine.md"), "x").unwrap();
        let clash = vec![Item {
            kind: "agent",
            name: "mine".into(),
            description: String::new(),
            path: root.join("agents/fixer.md"),
        }];
        assert!(market
            .install("mk", "other", None, None, &clash, &dest, false)
            .is_err());
        assert_eq!(market.installed().len(), 1);
        market.uninstall(market.installed()[0].id).unwrap();
        assert!(!home.join("skills/lint").exists() && !home.join("agents/fixer.md").exists());
        assert!(
            home.join("agents/mine.md").exists(),
            "files it did not write stay"
        );
        let _ = rec;
        let again = Market::open(&t.join("data"));
        assert!(again.installed().is_empty());
        let _ = std::fs::remove_dir_all(&t);
    }
}
