//! Finding a harness's agents, skills and prompts on disk, and the graph of
//! which one mentions (spawns, invokes, reads) which.

use crate::deck::config::{expand, Harness};
use crate::mem::export::{GraphData, GraphEdge, GraphNode};
use crate::mem::frontmatter;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Agent,
    Skill,
    Prompt,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::Skill => "skill",
            Kind::Prompt => "prompt",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Def {
    /// `<kind>:<name>`, unique within a harness.
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub tools: Vec<String>,
    /// Path shown to people: relative to the harness root when inside it.
    pub path: String,
    pub size: u64,
    pub modified: u64,
    #[serde(skip)]
    pub abs: PathBuf,
}

// ---- glob ------------------------------------------------------------------

/// Expand a glob relative to `base`. Supports `*` and `?` within a path
/// segment, `**` for any number of directories, `~/` and absolute patterns.
/// Hidden entries match only when the pattern segment names them explicitly.
/// Every path under `base` matching `pattern`: `*` and `?` within a name, `**`
/// for any depth, and `{a,b}` for alternatives (`.security-harness/{run,pr}-*`).
pub fn glob(base: &Path, pattern: &str) -> Vec<PathBuf> {
    let alts = braces(pattern);
    if alts.len() > 1 {
        let mut out: Vec<PathBuf> = alts.iter().flat_map(|p| glob_one(base, p)).collect();
        out.sort();
        out.dedup();
        return out;
    }
    glob_one(base, pattern)
}

/// `a{b,c}d` as `abd` and `acd` (nested and repeated braces too). A pattern
/// without a closed brace is itself.
fn braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let mut depth = 0;
    let mut close = None;
    let mut commas = vec![];
    for (i, c) in pattern[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            ',' if depth == 1 => commas.push(open + i),
            _ => {}
        }
    }
    let Some(close) = close else {
        return vec![pattern.to_string()];
    };
    let (head, tail) = (&pattern[..open], &pattern[close + 1..]);
    let mut cuts = vec![open];
    cuts.extend(commas);
    cuts.push(close);
    let mut out = vec![];
    for w in cuts.windows(2) {
        let alt = &pattern[w[0] + 1..w[1]];
        out.extend(braces(&format!("{}{}{}", head, alt, tail)));
    }
    out
}

fn glob_one(base: &Path, pattern: &str) -> Vec<PathBuf> {
    let pattern = pattern.replace('\\', "/");
    let first_wild = pattern.find(['*', '?']).unwrap_or(pattern.len());
    let fixed_end = pattern[..first_wild].rfind('/').map(|i| i + 1).unwrap_or(0);
    let start = if fixed_end == 0 {
        base.to_path_buf()
    } else {
        expand(base, &pattern[..fixed_end - 1])
    };
    if fixed_end == pattern.len() || first_wild == pattern.len() {
        let p = expand(base, &pattern);
        return if p.exists() { vec![p] } else { Vec::new() };
    }
    let segs: Vec<&str> = pattern[fixed_end..]
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let mut out = Vec::new();
    walk(&start, &segs, 0, &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk(dir: &Path, segs: &[&str], depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 12 {
        return;
    }
    let Some((seg, rest)) = segs.split_first() else {
        out.push(dir.to_path_buf());
        return;
    };
    if *seg == "**" {
        walk(dir, rest, depth + 1, out);
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !name.starts_with('.') && e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    walk(&e.path(), segs, depth + 1, out);
                }
            }
        }
        return;
    }
    if !seg.contains(['*', '?']) {
        let p = dir.join(seg);
        if rest.is_empty() {
            if p.exists() {
                out.push(p);
            }
        } else if p.is_dir() {
            walk(&p, rest, depth + 1, out);
        }
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !seg.starts_with('.') {
            continue;
        }
        if !wild_match(seg, &name) {
            continue;
        }
        let p = e.path();
        if rest.is_empty() {
            out.push(p);
        } else if p.is_dir() {
            walk(&p, rest, depth + 1, out);
        }
    }
}

pub fn wild_match(pat: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ---- definitions -------------------------------------------------------------

pub fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn display_path(root: &Path, p: &Path) -> String {
    match p.strip_prefix(root) {
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}

fn list_field(fm: &frontmatter::Frontmatter, key: &str) -> Vec<String> {
    match fm.get(key) {
        Some(frontmatter::Value::List(l)) => l.clone(),
        Some(frontmatter::Value::Str(s)) => s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        None => Vec::new(),
    }
}

fn parse_def(h: &Harness, kind: Kind, path: &Path) -> Option<Def> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 2_000_000 {
        return None;
    }
    let src = std::fs::read_to_string(path).ok()?;
    let (fm_text, body) = frontmatter::split(&src);
    let fm = fm_text.map(frontmatter::parse).unwrap_or_default();
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    // A skill is named after its folder: skills/<name>/SKILL.md.
    let fallback = if stem.eq_ignore_ascii_case("skill") {
        path.parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(stem)
    } else if kind == Kind::Prompt {
        path.file_name()?.to_string_lossy().into_owned()
    } else {
        stem
    };
    let name = fm
        .get_str("name")
        .filter(|_| kind != Kind::Prompt)
        .unwrap_or(fallback);
    let json_desc = || -> Option<String> {
        let v: serde_json::Value = serde_json::from_str(&src).ok()?;
        ["description", "title", "$id"]
            .iter()
            .find_map(|k| v.get(k).and_then(|x| x.as_str()).map(String::from))
    };
    let is_json = path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    let description = fm
        .get_str("description")
        .or_else(|| if is_json { json_desc() } else { None })
        .unwrap_or_else(|| {
            body.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("<!--"))
                .map(|l| crate::mem::search::truncate(l, 200))
                .unwrap_or_default()
        });
    let mut tools = list_field(&fm, "tools");
    if tools.is_empty() {
        tools = list_field(&fm, "allowed-tools");
    }
    Some(Def {
        id: format!("{}:{}", kind.as_str(), name),
        kind,
        name,
        description,
        model: fm.get_str("model"),
        tools,
        path: display_path(&h.root, path),
        size: meta.len(),
        modified: mtime(path),
        abs: path.to_path_buf(),
    })
}

pub fn discover(h: &Harness) -> Vec<Def> {
    let mut out: Vec<Def> = Vec::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    for (kind, globs) in [
        (Kind::Agent, &h.agents),
        (Kind::Skill, &h.skills),
        (Kind::Prompt, &h.prompts),
    ] {
        for g in globs {
            for p in glob(&h.root, g) {
                if !p.is_file() || !seen_paths.insert(p.clone()) {
                    continue;
                }
                if let Some(mut d) = parse_def(h, kind, &p) {
                    let mut n = 2;
                    let base = d.id.clone();
                    while out.iter().any(|x| x.id == d.id) {
                        d.id = format!("{}-{}", base, n);
                        n += 1;
                    }
                    out.push(d);
                }
            }
        }
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
    out
}

pub fn read_body(d: &Def) -> std::io::Result<String> {
    std::fs::read_to_string(&d.abs)
}

// ---- graph ---------------------------------------------------------------

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

/// True when `name` occurs in `text` as a whole word (hyphens count as word
/// characters, so `sh-kb-sqli` does not match inside `sh-kb-sqli-extra`).
pub fn mentions(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(i) = text[from..].find(name) {
        let s = from + i;
        let e = s + name.len();
        let before = text[..s].chars().next_back();
        let after = text[e..].chars().next();
        let ok_before = before.map(|c| !is_word(c)).unwrap_or(true);
        // A trailing '.' or '-' followed by a non-word char still ends the word.
        let ok_after = match after {
            None => true,
            Some(c) if !is_word(c) => true,
            Some('-') => text[e + 1..]
                .chars()
                .next()
                .map(|c| !is_word(c))
                .unwrap_or(true),
            _ => false,
        };
        if ok_before && ok_after {
            return true;
        }
        from = s + name.len().max(1);
    }
    false
}

/// Family references like `sh-kb-*` or `sh-kb-<class>`: the prefixes named.
fn family_prefixes(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let b = text.as_bytes();
    for (i, w) in text.match_indices(['*', '<']) {
        let _ = w;
        if i == 0 || b[i - 1] != b'-' {
            continue;
        }
        let mut s = i - 1;
        while s > 0 && (b[s - 1].is_ascii_alphanumeric() || b[s - 1] == b'-' || b[s - 1] == b'_') {
            s -= 1;
        }
        let prefix = &text[s..i];
        if prefix.len() > 2 && prefix.chars().any(|c| c.is_ascii_alphabetic()) {
            out.insert(prefix.to_string());
        }
    }
    out
}

/// Nodes: every definition plus pipeline stages. Edges: a definition that
/// names another spawns (agent), invokes (skill) or reads (prompt) it; a
/// family reference (`sh-kb-*`) loads every member; stages run their agent
/// and lead to the next stage.
pub fn graph(h: &Harness, defs: &[Def]) -> GraphData {
    let mut nodes: Vec<GraphNode> = defs
        .iter()
        .map(|d| GraphNode {
            id: d.id.clone(),
            title: d.name.clone(),
            kind: Some(d.kind.as_str().to_string()),
            tags: d.model.iter().cloned().collect(),
            ghost: false,
            body: Some(d.description.clone()),
            updated: None,
        })
        .collect();
    let mut edges: BTreeMap<(String, String), String> = BTreeMap::new();
    for a in defs {
        let Ok(text) = read_body(a) else { continue };
        let fams = family_prefixes(&text);
        for b in defs {
            if a.id == b.id {
                continue;
            }
            let rel = match b.kind {
                Kind::Agent => "spawns",
                Kind::Skill => "invokes",
                Kind::Prompt => "reads",
            };
            let direct = mentions(&text, &b.name);
            let family =
                b.kind != Kind::Prompt && fams.iter().any(|f| b.name.starts_with(f.as_str()));
            if direct || family {
                let rel = if direct { rel } else { "loads" };
                edges
                    .entry((a.id.clone(), b.id.clone()))
                    .or_insert_with(|| rel.to_string());
            }
        }
    }
    let by_name: BTreeMap<&str, &Def> = defs.iter().map(|d| (d.name.as_str(), d)).collect();
    let mut prev: Option<String> = None;
    for s in &h.stages {
        let sid = format!("stage:{}", s.name);
        nodes.push(GraphNode {
            id: sid.clone(),
            title: format!("stage: {}", s.name),
            kind: Some("stage".into()),
            tags: Vec::new(),
            ghost: false,
            body: s
                .done_when
                .as_ref()
                .map(|d| format!("Done when the run has {}", d)),
            updated: None,
        });
        if let Some(agent) = s.agent.as_deref().and_then(|a| by_name.get(a)) {
            edges.insert((sid.clone(), agent.id.clone()), "runs".into());
        }
        if let Some(p) = prev {
            edges.insert((p, sid.clone()), "then".into());
        }
        prev = Some(sid);
    }
    GraphData {
        vault: h.root.display().to_string(),
        nodes,
        edges: edges
            .into_iter()
            .map(|((source, target), rel)| GraphEdge {
                source,
                target,
                rel,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wild_match("*.md", "a.md"));
        assert!(wild_match("run-*", "run-20261113-1"));
        assert!(wild_match("a?c", "abc"));
        assert!(!wild_match("*.md", "a.mdx"));
        assert!(wild_match("*", ""));
        assert!(wild_match("*a*b", "xxaYYb"));
        assert_eq!(braces(".s/{run,pr}-*"), vec![".s/run-*", ".s/pr-*"]);
        assert_eq!(braces("{a,b{c,d}}x"), vec!["ax", "bcx", "bdx"]);
        assert_eq!(braces("no{brace"), vec!["no{brace"]);
    }

    #[test]
    fn word_mentions() {
        assert!(mentions("spawn `sh-hunter` per class", "sh-hunter"));
        assert!(mentions("ends with sh-hunter.", "sh-hunter"));
        assert!(!mentions("sh-hunters", "sh-hunter"));
        assert!(!mentions("the sh-kb-sqli-extra kb", "sh-kb-sqli"));
        assert!(mentions("load sh-kb-sqli- next", "sh-kb-sqli"));
        assert!(!mentions("xsh-recon", "sh-recon"));
    }

    #[test]
    fn families() {
        let f = family_prefixes("load `sh-kb-<class>` or any sh-kb-* skill; not a-* or x <y");
        assert!(f.contains("sh-kb-"), "{:?}", f);
        assert_eq!(f.len(), 1, "short prefixes like a- are ignored: {:?}", f);
    }
}
