//! `indra mem import`: bring an existing knowledge base into a vault.
//!
//! Sources and what changes:
//!   obsidian     notes copied as they are (file names kept, so `[[links]]`
//!                keep working in Obsidian too); `.obsidian/` is skipped
//!   notion       a Notion "Markdown & CSV" export, unzipped: the 32-hex page
//!                ids are dropped from names, and `[text](Page%20<id>.md)`
//!                links become `[[page|text]]`
//!   github-wiki  a cloned GitHub wiki (`git clone <repo>.wiki.git`): page
//!                names lose their dashes, and GitHub's `[[text|Page-Name]]`
//!                (text first) becomes `[[page|text]]`
//!   markdown     any folder of Markdown: relative `.md` links that point at
//!                imported notes become `[[note|text]]`
//!
//! Every note gets a `source:` field (and any `--tag`). Existing notes are
//! never overwritten unless asked. Only `.md` files are read; attachments,
//! Notion's CSV databases, hidden folders and symlinks are left alone.

use crate::mem::frontmatter;
use crate::mem::note::slugify;
use crate::mem::vault::{self, Lock, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Obsidian,
    Notion,
    GithubWiki,
    Markdown,
}

impl Source {
    pub fn parse(s: &str) -> Result<Source> {
        match s {
            "obsidian" => Ok(Source::Obsidian),
            "notion" => Ok(Source::Notion),
            "github-wiki" | "github" => Ok(Source::GithubWiki),
            "markdown" | "md" => Ok(Source::Markdown),
            _ => Err(format!(
                "unknown source '{}': use obsidian, notion, github-wiki or markdown",
                s
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Source::Obsidian => "obsidian",
            Source::Notion => "notion",
            Source::GithubWiki => "github-wiki",
            Source::Markdown => "markdown",
        }
    }
}

pub struct Options {
    pub from: Option<Source>,
    pub folder: Option<String>,
    pub tags: Vec<String>,
    pub overwrite: bool,
    pub dry_run: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub from: Option<Source>,
    pub imported: Vec<String>,
    pub skipped_existing: Vec<String>,
    pub links_converted: usize,
    /// (note id, link target) for links that point at nothing imported.
    pub unresolved: Vec<(String, String)>,
    /// Non-Markdown files left alone (attachments, CSV databases).
    pub ignored_files: usize,
    pub dry_run: bool,
}

struct Item {
    src: PathBuf,
    /// Source path relative to the import root, `/`-separated, decoded.
    rel: String,
    id: String,
    title: String,
}

/// Notion appends ` <32 hex>` to every exported page and folder name.
fn strip_notion_id(name: &str) -> &str {
    let b = name.as_bytes();
    if b.len() > 33
        && b[b.len() - 33] == b' '
        && name[name.len() - 32..]
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    {
        &name[..name.len() - 33]
    } else {
        name
    }
}

fn files(root: &Path) -> (Vec<(PathBuf, String)>, usize) {
    let mut md = Vec::new();
    let mut other = 0;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() && depth < 24 {
                stack.push((p, depth + 1));
            } else if ft.is_file() {
                if name.to_lowercase().ends_with(".md") {
                    let rel = p
                        .strip_prefix(root)
                        .unwrap_or(&p)
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    md.push((p, rel));
                } else {
                    other += 1;
                }
            }
        }
    }
    md.sort_by(|a, b| a.1.cmp(&b.1));
    (md, other)
}

pub fn detect(root: &Path, md: &[(PathBuf, String)]) -> Source {
    if root.join(".obsidian").is_dir() {
        return Source::Obsidian;
    }
    let notion = md
        .iter()
        .filter(|(_, rel)| {
            rel.split('/')
                .any(|c| strip_notion_id(c.trim_end_matches(".md")) != c.trim_end_matches(".md"))
        })
        .count();
    if notion > 0 && notion * 2 >= md.len() {
        return Source::Notion;
    }
    if root.join("_Sidebar.md").is_file()
        || (root.join("Home.md").is_file()
            && md.iter().any(|(_, r)| r.contains('-') && !r.contains(' ')))
    {
        return Source::GithubWiki;
    }
    Source::Markdown
}

fn clean_folder(f: &str) -> Option<String> {
    let parts: Vec<String> = f
        .split(['/', '\\'])
        .map(slugify)
        .filter(|s| !s.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn plan(md: &[(PathBuf, String)], source: Source, folder: Option<&str>) -> Vec<Item> {
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for (src, rel) in md {
        let stem_path = rel
            .strip_suffix(".md")
            .or_else(|| rel.strip_suffix(".MD"))
            .unwrap_or(rel);
        let (id, title) = match source {
            // Keep names: Obsidian resolves [[links]] by file name.
            Source::Obsidian | Source::Markdown => {
                let base = stem_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(stem_path)
                    .to_string();
                (stem_path.to_string(), base)
            }
            Source::Notion => {
                let parts: Vec<&str> = stem_path.split('/').map(strip_notion_id).collect();
                let title = parts.last().copied().unwrap_or("note").to_string();
                let id = parts
                    .iter()
                    .map(|p| slugify(p))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("/");
                (id, title)
            }
            Source::GithubWiki => {
                let title = stem_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(stem_path)
                    .replace('-', " ");
                (slugify(&title), title)
            }
        };
        let id = if id.is_empty() {
            "note".to_string()
        } else {
            id
        };
        let id = match folder {
            Some(f) => format!("{}/{}", f, id),
            None => id,
        };
        let n = used.entry(id.to_lowercase()).or_insert(0);
        *n += 1;
        let id = if *n == 1 { id } else { format!("{}-{}", id, n) };
        out.push(Item {
            src: src.clone(),
            rel: rel.clone(),
            id,
            title,
        });
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push(hex(b[i + 1]) * 16 + hex(b[i + 2]));
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Join a relative link onto the directory of `from`, resolving `.` and `..`.
fn join_rel(from: &str, href: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Rewrite `[text](href)` links (not images) where `f` returns a replacement.
fn rewrite_md_links(line: &str, mut f: impl FnMut(&str, &str) -> Option<String>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let is_image = open > 0 && rest[..open].ends_with('!');
        let after = &rest[open + 1..];
        let Some(close) = after.find("](") else { break };
        let text = &after[..close];
        let tail = &after[close + 2..];
        let Some(end) = tail.find(')') else { break };
        let href = &tail[..end];
        if text.contains('[') || is_image {
            out.push_str(&rest[..open + 1]);
            rest = after;
            continue;
        }
        match f(text, href.trim()) {
            Some(rep) => {
                out.push_str(&rest[..open]);
                out.push_str(&rep);
            }
            None => out.push_str(&rest[..open + 1 + close + 2 + end + 1]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

fn wikilink(id: &str, text: &str, title: &str) -> String {
    let text = text.replace(['|', '[', ']'], " ");
    if text.trim().is_empty() || text.trim().eq_ignore_ascii_case(title) || text.trim() == id {
        format!("[[{}]]", id)
    } else {
        format!("[[{}|{}]]", id, text.trim())
    }
}

struct Ctx<'a> {
    by_rel: HashMap<String, &'a Item>,
    by_name: HashMap<String, &'a Item>,
}

fn convert(item: &Item, body: &str, source: Source, cx: &Ctx, report: &mut Report) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut fence = false;
    for line in body.split('\n') {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            out.push(line.to_string());
            continue;
        }
        if fence || source == Source::Obsidian {
            out.push(line.to_string());
            continue;
        }
        let mut line = line.to_string();
        // Markdown links to other imported pages.
        line = rewrite_md_links(&line, |text, href| {
            if href.contains("://") || href.starts_with("mailto:") || href.starts_with('#') {
                return None;
            }
            let path = percent_decode(href.split('#').next().unwrap_or(href));
            let is_md = path.to_lowercase().ends_with(".md");
            if !is_md && source != Source::GithubWiki {
                return None;
            }
            let target = if source == Source::GithubWiki && !is_md {
                cx.by_name
                    .get(&path.replace('-', " ").to_lowercase())
                    .copied()
            } else {
                join_rel(&item.rel, &path).and_then(|p| cx.by_rel.get(&p).copied())
            };
            match target {
                Some(t) => {
                    report.links_converted += 1;
                    Some(wikilink(&t.id, text, &t.title))
                }
                None => {
                    if is_md {
                        report.unresolved.push((item.id.clone(), path));
                    }
                    None
                }
            }
        });
        // GitHub wiki: [[text|Page-Name]] and [[Page Name]].
        if source == Source::GithubWiki && line.contains("[[") {
            let mut rebuilt = String::new();
            let mut rest = line.as_str();
            while let Some(s) = rest.find("[[") {
                let after = &rest[s + 2..];
                let Some(e) = after.find("]]") else { break };
                let inner = &after[..e];
                let (text, page) = match inner.split_once('|') {
                    Some((t, p)) => (t, p),
                    None => (inner, inner),
                };
                rebuilt.push_str(&rest[..s]);
                match cx
                    .by_name
                    .get(&page.trim().replace('-', " ").to_lowercase())
                {
                    Some(t) => {
                        report.links_converted += 1;
                        rebuilt.push_str(&wikilink(&t.id, text, &t.title));
                    }
                    None => {
                        report
                            .unresolved
                            .push((item.id.clone(), page.trim().to_string()));
                        rebuilt.push_str(&rest[s..s + 2 + e + 2]);
                    }
                }
                rest = &after[e + 2..];
            }
            rebuilt.push_str(rest);
            line = rebuilt;
        }
        out.push(line);
    }
    out.join("\n")
}

pub fn import(vault_root: &Path, src: &Path, opts: &Options) -> Result<Report> {
    if !src.is_dir() {
        return Err(format!(
            "{} is not a folder (unzip a Notion export first; clone a GitHub wiki with git clone <repo>.wiki.git)",
            src.display()
        ));
    }
    let (md, ignored) = files(src);
    let source = opts.from.unwrap_or_else(|| detect(src, &md));
    let folder = opts.folder.as_deref().and_then(clean_folder);
    let items = plan(&md, source, folder.as_deref());
    let cx = Ctx {
        by_rel: items.iter().map(|i| (i.rel.clone(), i)).collect(),
        by_name: items.iter().map(|i| (i.title.to_lowercase(), i)).collect(),
    };
    let mut report = Report {
        from: Some(source),
        ignored_files: ignored,
        dry_run: opts.dry_run,
        ..Default::default()
    };

    let _lock = if opts.dry_run {
        None
    } else {
        Some(Lock::acquire(vault_root)?)
    };
    let v = vault::Vault::open(vault_root)?;
    let tags: Vec<String> = opts
        .tags
        .iter()
        .map(|t| t.trim_start_matches('#').to_string())
        .filter(|t| !t.is_empty())
        .collect();
    for it in &items {
        let dest = v.path_for_id(&it.id);
        if dest.exists() && !opts.overwrite {
            report.skipped_existing.push(it.id.clone());
            continue;
        }
        let raw = std::fs::read(&it.src)
            .map_err(|e| format!("cannot read {}: {}", it.src.display(), e))?;
        let text = String::from_utf8_lossy(&raw).replace("\r\n", "\n");
        let (fm, body) = frontmatter::split(&text);
        let fm_len = text.len() - body.len();
        let mut doc = format!(
            "{}{}",
            &text[..fm_len],
            convert(it, body, source, &cx, &mut report)
        );
        let parsed = fm.map(frontmatter::parse).unwrap_or_default();
        let has_h1 = body.lines().any(|l| l.trim_start().starts_with("# "));
        if parsed.get_str("title").is_none() && !has_h1 && source != Source::Obsidian {
            doc = frontmatter::set_field(&doc, "title", &frontmatter::quote(&it.title));
        }
        if parsed.get_str("source").is_none() {
            doc = frontmatter::set_field(
                &doc,
                "source",
                &frontmatter::quote(&format!("{}:{}", source.name(), it.rel)),
            );
        }
        if !tags.is_empty() {
            let mut all = parsed.get_list("tags");
            for t in &tags {
                if !all.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                    all.push(t.clone());
                }
            }
            doc = frontmatter::set_field(&doc, "tags", &frontmatter::render_list(&all));
        }
        if !opts.dry_run {
            vault::write_atomic(&dest, &doc)?;
        }
        report.imported.push(it.id.clone());
    }
    if !opts.dry_run {
        vault::init(vault_root)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("indra-import-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(root: &Path, rel: &str, s: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    fn opts() -> Options {
        Options {
            from: None,
            folder: None,
            tags: vec![],
            overwrite: false,
            dry_run: false,
        }
    }

    const ID: &str = "0123456789abcdef0123456789abcdef";
    const ID2: &str = "fedcba9876543210fedcba9876543210";

    #[test]
    fn helpers() {
        assert_eq!(
            strip_notion_id(&format!("Security Notes {}", ID)),
            "Security Notes"
        );
        assert_eq!(strip_notion_id("Plain"), "Plain");
        assert_eq!(percent_decode("A%20B%2Fc"), "A B/c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(
            percent_decode("caf%C3%A9 %E2%9C%93 na\u{ef}ve%2"),
            "caf\u{e9} \u{2713} na\u{ef}ve%2"
        );
        assert_eq!(join_rel("a/b.md", "../c/d.md").as_deref(), Some("c/d.md"));
        assert_eq!(join_rel("b.md", "../../x.md"), None);
        let r = rewrite_md_links("see [a](x.md), ![img](i.png) and [b](https://e)", |t, h| {
            (h == "x.md").then(|| format!("<{}>", t))
        });
        assert_eq!(r, "see <a>, ![img](i.png) and [b](https://e)");
    }

    #[test]
    fn notion_export() {
        let src = tmp("notion-src");
        let vault = tmp("notion-vault");
        write(&src, &format!("Security Notes {}.md", ID), &format!(
            "# Security Notes\n\nSee [Django extra](Security%20Notes%20{id}/Django%20extra%20{id2}.md) and [the docs](https://docs.djangoproject.com).\n\n```\n[not](a.md)\n```\n",
            id = ID, id2 = ID2
        ));
        write(&src, &format!("Security Notes {}/Django extra {}.md", ID, ID2), &format!(
            "# Django extra\n\nRaw SQL sink. Back to [Security Notes](../Security%20Notes%20{}.md). Missing: [gone](Gone%20{}.md)\n",
            ID, ID2
        ));
        write(&src, "Tasks.csv", "a,b\n");
        let r = import(
            &vault,
            &src,
            &Options {
                tags: vec!["imported".into()],
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(r.from, Some(Source::Notion));
        assert_eq!(
            r.imported,
            vec!["security-notes", "security-notes/django-extra"]
        );
        assert_eq!(r.links_converted, 2);
        assert_eq!(r.ignored_files, 1);
        assert_eq!(r.unresolved.len(), 1);
        let top = std::fs::read_to_string(vault.join("security-notes.md")).unwrap();
        assert!(top.contains("See [[security-notes/django-extra]] and [the docs](https://docs.djangoproject.com)."), "{}", top);
        assert!(top.contains("```\n[not](a.md)\n```"), "code is left alone");
        assert!(
            top.contains("tags: [imported]") && top.contains("source: notion:Security Notes"),
            "{}",
            top
        );
        let child = std::fs::read_to_string(vault.join("security-notes/django-extra.md")).unwrap();
        assert!(child.contains("Back to [[security-notes]]"), "{}", child);
        // The vault resolves the converted links.
        let v = crate::mem::vault::Vault::open(&vault).unwrap();
        let g = crate::mem::graph::Graph::build(&v);
        assert_eq!(g.edge_count(), 2);
        // Importing again skips what exists.
        let again = import(&vault, &src, &opts()).unwrap();
        assert_eq!(again.imported.len(), 0);
        assert_eq!(again.skipped_existing.len(), 2);
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(vault);
    }

    #[test]
    fn github_wiki() {
        let src = tmp("wiki-src");
        let vault = tmp("wiki-vault");
        write(&src, "Home.md", "Start with [[Getting Started]] or [[the guide|Getting-Started]]; see [threat model](Threat-Model). Also [[Nowhere]].\n");
        write(&src, "Getting-Started.md", "Install it.\n");
        write(&src, "Threat-Model.md", "Assets and actors.\n");
        write(&src, "_Sidebar.md", "[[Home]]\n");
        let r = import(&vault, &src, &opts()).unwrap();
        assert_eq!(r.from, Some(Source::GithubWiki));
        let home = std::fs::read_to_string(vault.join("home.md")).unwrap();
        assert!(home.contains("Start with [[getting-started]] or [[getting-started|the guide]]; see [[threat-model]]. Also [[Nowhere]]."), "{}", home);
        assert!(home.contains("title: Home"), "{}", home);
        assert_eq!(
            r.unresolved,
            vec![("home".to_string(), "Nowhere".to_string())]
        );
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(vault);
    }

    #[test]
    fn obsidian_and_markdown_keep_names() {
        let src = tmp("obs-src");
        let vault = tmp("obs-vault");
        std::fs::create_dir_all(src.join(".obsidian")).unwrap();
        write(&src, ".obsidian/app.json", "{}");
        write(
            &src,
            "My Note.md",
            "---\ntags: [x]\n---\nLinks to [[Other Note]] and [md](Other%20Note.md).\n",
        );
        write(&src, "Other Note.md", "Hi.\n");
        let r = import(
            &vault,
            &src,
            &Options {
                folder: Some("Team KB".into()),
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(r.from, Some(Source::Obsidian));
        assert_eq!(r.imported, vec!["team-kb/My Note", "team-kb/Other Note"]);
        let n = std::fs::read_to_string(vault.join("team-kb/My Note.md")).unwrap();
        assert!(
            n.contains("Links to [[Other Note]] and [md](Other%20Note.md)."),
            "Obsidian content is untouched: {}",
            n
        );
        assert!(
            n.contains("tags: [x]") && n.contains("source: obsidian:My Note.md"),
            "{}",
            n
        );

        let md = tmp("md-src");
        write(&md, "a.md", "# A\nSee [B](sub/b.md).\n");
        write(&md, "sub/b.md", "# B\n");
        let r = import(
            &vault,
            &md,
            &Options {
                dry_run: true,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(r.from, Some(Source::Markdown));
        assert!(r.dry_run && r.imported == vec!["a", "sub/b"] && r.links_converted == 1);
        assert!(!vault.join("a.md").exists(), "a dry run writes nothing");
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(md);
        let _ = std::fs::remove_dir_all(vault);
    }
}
