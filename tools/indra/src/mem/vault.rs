//! A vault is a directory of Markdown notes, marked by a `.indra/` folder.
//! The notes are the only source of truth: there is no database to drift out
//! of sync, and the folder opens as-is in Obsidian or any Markdown editor.

use crate::mem::note::{slugify, Note};
use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub const MARKER: &str = ".indra";

pub type Result<T> = std::result::Result<T, String>;

pub struct Vault {
    pub root: PathBuf,
    pub notes: Vec<Note>,
    /// Lowercased lookup keys -> note index. See `resolve`.
    by_id: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
    /// Keys that matched more than one note by title/alias/basename.
    pub ambiguous: HashMap<String, Vec<usize>>,
}

/// Where the vault lives: `--vault`, then `$INDRA_VAULT`, then the nearest
/// ancestor of the working directory holding a `.indra/` folder, then the
/// per-user default: `memory/` in the indra data directory
/// (`~/.local/share/indra`, `~/Library/Application Support/indra` or
/// `%LOCALAPPDATA%\indra`).
pub fn locate(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    if let Some(p) = std::env::var_os("INDRA_VAULT").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            if dir.join(MARKER).is_dir() {
                return dir;
            }
            if !dir.pop() {
                break;
            }
        }
    }
    default_vault()
}

fn default_vault() -> PathBuf {
    crate::paths::data_dir().join("memory")
}

/// Create the vault directory and its marker. Idempotent.
pub fn init(root: &Path) -> Result<bool> {
    let marker = root.join(MARKER);
    let existed = marker.is_dir();
    fs::create_dir_all(&marker)
        .map_err(|e| format!("cannot create {}: {}", marker.display(), e))?;
    let gi = marker.join(".gitignore");
    if !gi.exists() {
        let _ = fs::write(&gi, "lock\n");
    }
    Ok(!existed)
}

impl Vault {
    /// Load every `*.md` under `root`. Hidden directories (`.obsidian`,
    /// `.git`, `.indra`, `.trash`) and symlinks are skipped. A missing vault
    /// loads as empty so read commands work before the first write.
    pub fn open(root: &Path) -> Result<Vault> {
        let mut files = Vec::new();
        if root.is_dir() {
            walk(root, root, &mut files, 0)?;
        }
        files.sort();
        let mut notes = Vec::with_capacity(files.len());
        for (id, path) in files {
            let bytes =
                fs::read(&path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
            let src = String::from_utf8_lossy(&bytes);
            notes.push(Note::parse(&id, path, &src));
        }
        let mut v = Vault {
            root: root.to_path_buf(),
            notes,
            by_id: HashMap::new(),
            by_name: HashMap::new(),
            ambiguous: HashMap::new(),
        };
        v.reindex();
        Ok(v)
    }

    fn reindex(&mut self) {
        self.by_id.clear();
        self.by_name.clear();
        self.ambiguous.clear();
        for (i, n) in self.notes.iter().enumerate() {
            self.by_id.insert(n.id.to_lowercase(), i);
        }
        // Lower priority names. First writer wins; collisions are recorded.
        let mut names: Vec<(String, usize)> = Vec::new();
        for (i, n) in self.notes.iter().enumerate() {
            let base = n.id.rsplit('/').next().unwrap_or(&n.id);
            if base != n.id {
                names.push((base.to_lowercase(), i));
            }
        }
        for (i, n) in self.notes.iter().enumerate() {
            names.push((n.title.to_lowercase(), i));
            for a in &n.aliases {
                names.push((a.to_lowercase(), i));
            }
            if let Some(f) = &n.fm_id {
                names.push((f.to_lowercase(), i));
            }
        }
        for (k, i) in names {
            if k.is_empty() || self.by_id.contains_key(&k) {
                continue;
            }
            match self.by_name.get(&k) {
                Some(&j) if j != i => {
                    let e = self.ambiguous.entry(k).or_insert_with(|| vec![j]);
                    if !e.contains(&i) {
                        e.push(i);
                    }
                }
                Some(_) => {}
                None => {
                    self.by_name.insert(k, i);
                }
            }
        }
    }

    /// Resolve a link target or user-typed name: exact id, then basename,
    /// title, alias or frontmatter `id` (case-insensitive), then its slug.
    pub fn resolve(&self, name: &str) -> Option<usize> {
        let name = name.trim().trim_end_matches(".md").replace('\\', "/");
        let k = name.to_lowercase();
        if let Some(&i) = self.by_id.get(&k).or_else(|| self.by_name.get(&k)) {
            return Some(i);
        }
        let s = slugify(&name);
        if s.is_empty() {
            return None;
        }
        self.by_id.get(&s).or_else(|| self.by_name.get(&s)).copied()
    }

    pub fn require(&self, name: &str) -> Result<usize> {
        self.resolve(name).ok_or_else(|| {
            let hint = self
                .notes
                .iter()
                .filter(|n| n.title.to_lowercase().contains(&name.to_lowercase()))
                .take(5)
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>();
            if hint.is_empty() {
                format!("no note named '{}' in {}", name, self.root.display())
            } else {
                format!(
                    "no note named '{}'. Did you mean: {}",
                    name,
                    hint.join(", ")
                )
            }
        })
    }

    pub fn path_for_id(&self, id: &str) -> PathBuf {
        let mut p = self.root.clone();
        for part in id.split('/') {
            p.push(part);
        }
        p.set_extension("md");
        p
    }

    /// Pick an unused id for a new note title, inside optional `folder`.
    pub fn fresh_id(&self, title: &str, folder: Option<&str>) -> String {
        let mut base = slugify(title);
        if base.is_empty() {
            base = format!(
                "note-{}",
                crate::mem::time::now_iso()
                    .replace([':', '-'], "")
                    .to_lowercase()
            );
        }
        let prefix = folder
            .map(|f| {
                f.split(['/', '\\'])
                    .map(slugify)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .filter(|f| !f.is_empty());
        let make = |s: &str| match &prefix {
            Some(p) => format!("{}/{}", p, s),
            None => s.to_string(),
        };
        let mut id = make(&base);
        let mut n = 2;
        while self.by_id.contains_key(&id) || self.path_for_id(&id).exists() {
            id = make(&format!("{}-{}", base, n));
            n += 1;
        }
        id
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>, depth: usize) -> Result<()> {
    if depth > 32 {
        return Ok(());
    }
    let rd = fs::read_dir(dir).map_err(|e| format!("cannot list {}: {}", dir.display(), e))?;
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            walk(root, &path, out, depth + 1)?;
        } else if ft.is_file() && name.to_lowercase().ends_with(".md") {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let mut id = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            id.truncate(id.len() - 3);
            out.push((id, path));
        }
    }
    Ok(())
}

/// A cross-process advisory lock (`.indra/lock`) so parallel agents writing
/// to one vault do not lose each other's edits. Stale locks (> 30 s) are
/// broken. Released on drop.
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn acquire(root: &Path) -> Result<Lock> {
        init(root)?;
        let path = root.join(MARKER).join("lock");
        let start = Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(Lock { path });
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok())
                        .map(|age| age > Duration::from_secs(30))
                        .unwrap_or(false);
                    if stale {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() > Duration::from_secs(10) {
                        return Err(format!(
                            "vault is locked by another writer ({}); retry, or delete it if no indra is running",
                            path.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return Err(format!("cannot lock {}: {}", path.display(), e)),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Write via a temp file and rename, so a crash never leaves half a note.
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
    }
    let tmp = path.with_extension(format!("md.tmp{}", std::process::id()));
    fs::write(&tmp, content).map_err(|e| format!("cannot write {}: {}", tmp.display(), e))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot write {}: {}", path.display(), e)
    })
}

/// Read a note for editing: content with `\n` endings, and whether it used CRLF.
pub fn read_for_edit(path: &Path) -> Result<(String, bool)> {
    let raw =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let crlf = raw.contains("\r\n");
    Ok((if crlf { raw.replace("\r\n", "\n") } else { raw }, crlf))
}

pub fn finish_edit(path: &Path, content: &str, crlf: bool) -> Result<()> {
    if crlf {
        write_atomic(path, &content.replace('\n', "\r\n"))
    } else {
        write_atomic(path, content)
    }
}
