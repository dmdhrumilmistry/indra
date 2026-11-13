//! Parsing one Markdown note: title, type, tags, aliases and outgoing links.
//!
//! Link syntax, all Obsidian-compatible:
//!   `[[target]]`, `[[target|label]]`, `[[target#heading]]`  -> rel `related`
//!   `rel:: [[target]]` (a Dataview inline field, optionally a list item) -> rel `rel`
//!   `[label](other-note.md)`                                -> rel `related`
//!   a frontmatter key whose value holds `[[target]]`        -> rel = that key

use crate::frontmatter::{self, Frontmatter, Value};
use serde::Serialize;
use std::path::PathBuf;

pub const DEFAULT_REL: &str = "related";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Link {
    pub target: String,
    pub rel: String,
    /// 1-based line in the file, 0 when it came from frontmatter.
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct Note {
    /// Vault-relative path without `.md`, `/`-separated. Unique.
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub fm_id: Option<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    pub source: Option<String>,
    pub body: String,
    pub links: Vec<Link>,
}

impl Note {
    pub fn parse(id: &str, path: PathBuf, src: &str) -> Note {
        let (fm_text, body) = frontmatter::split(src);
        let fm = fm_text.map(frontmatter::parse).unwrap_or_default();
        let body_offset = match fm_text {
            Some(f) => f.lines().count() + 2,
            None => 0,
        };

        let basename = id.rsplit('/').next().unwrap_or(id).to_string();
        let title = fm
            .get_str("title")
            .or_else(|| first_heading(body))
            .unwrap_or(basename);

        let mut tags: Vec<String> = fm
            .get_list("tags")
            .into_iter()
            .map(|t| t.trim_start_matches('#').to_string())
            .filter(|t| !t.is_empty())
            .collect();
        for t in inline_tags(body) {
            if !tags.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
                tags.push(t);
            }
        }

        let mut links = frontmatter_links(&fm);
        links.extend(body_links(body, body_offset));

        Note {
            id: id.to_string(),
            path,
            title,
            kind: fm.get_str("type").or_else(|| fm.get_str("kind")),
            tags,
            aliases: fm.get_list("aliases"),
            fm_id: fm.get_str("id"),
            created: fm.get_str("created"),
            updated: fm.get_str("updated"),
            source: fm.get_str("source"),
            body: body.to_string(),
            links,
        }
    }
}

fn first_heading(body: &str) -> Option<String> {
    let mut fence = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if !fence {
            if let Some(h) = t.strip_prefix("# ") {
                let h = h.trim();
                if !h.is_empty() {
                    return Some(h.to_string());
                }
            }
        }
    }
    None
}

fn frontmatter_links(fm: &Frontmatter) -> Vec<Link> {
    let mut out = Vec::new();
    for (key, value) in &fm.entries {
        let values: Vec<&String> = match value {
            Value::Str(s) => vec![s],
            Value::List(l) => l.iter().collect(),
        };
        let rel = normalize_rel(key).unwrap_or_else(|| DEFAULT_REL.to_string());
        for v in values {
            for target in wikilinks(v) {
                out.push(Link {
                    target,
                    rel: rel.clone(),
                    line: 0,
                });
            }
        }
    }
    out
}

fn body_links(body: &str, offset: usize) -> Vec<Link> {
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    for (i, raw) in body.lines().enumerate() {
        let t = raw.trim_start();
        if let Some(open) = fence {
            if t.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if t.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if t.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        let line = strip_inline_code(raw);
        let (rel, scan) = match inline_field(&line) {
            Some((rel, value)) => (rel, value),
            None => (DEFAULT_REL.to_string(), line.as_str()),
        };
        let lineno = offset + i + 1;
        for target in wikilinks(scan) {
            out.push(Link {
                target,
                rel: rel.clone(),
                line: lineno,
            });
        }
        for target in md_links(scan) {
            out.push(Link {
                target,
                rel: rel.clone(),
                line: lineno,
            });
        }
    }
    out
}

/// `- rel:: value` or `rel:: value` -> (normalised rel, value).
pub fn inline_field(line: &str) -> Option<(String, &str)> {
    let mut t = line.trim_start();
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(bullet) {
            t = rest.trim_start();
            break;
        }
    }
    let idx = t.find("::")?;
    let key = &t[..idx];
    if key.is_empty() || key.contains("[[") || key.trim() != key {
        return None;
    }
    let rel = normalize_rel(key)?;
    Some((rel, &t[idx + 2..]))
}

/// Lowercase, spaces to `-`; must then be `[a-z0-9_-]+` and start with a letter.
pub fn normalize_rel(s: &str) -> Option<String> {
    let r: String = s
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .collect();
    let ok = !r.is_empty()
        && r.len() <= 64
        && r.chars()
            .next()
            .map(|c| c.is_ascii_lowercase())
            .unwrap_or(false)
        && r.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    ok.then_some(r)
}

fn strip_inline_code(line: &str) -> String {
    if !line.contains('`') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut in_code = false;
    for c in line.chars() {
        if c == '`' {
            in_code = !in_code;
            continue;
        }
        if !in_code {
            out.push(c);
        }
    }
    out
}

/// Every `[[...]]` target in a string, skipping embeds of non-note files.
pub fn wikilinks(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = &after[..end];
        let embed = start > 0 && rest[..start].ends_with('!');
        rest = &after[end + 2..];
        if inner.contains('\n') || inner.contains("[[") {
            continue;
        }
        let target = inner.split('|').next().unwrap_or("");
        let target = target.split(['#', '^']).next().unwrap_or("").trim();
        if target.is_empty() {
            continue;
        }
        let target = target.strip_suffix(".md").unwrap_or(target);
        if embed && has_file_ext(target) {
            continue;
        }
        out.push(target.to_string());
    }
    out
}

/// `[label](path.md)` links to other notes; URLs and non-note files are ignored.
fn md_links(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("](") {
        let after = &rest[i + 2..];
        let Some(end) = after.find(')') else { break };
        let href = after[..end].trim();
        rest = &after[end + 1..];
        let href = href.split('#').next().unwrap_or("");
        if href.contains("://") || href.starts_with("mailto:") || !href.ends_with(".md") {
            continue;
        }
        let href = href.replace("%20", " ");
        let href = href.trim_start_matches("./");
        out.push(href.trim_end_matches(".md").to_string());
    }
    out
}

fn has_file_ext(t: &str) -> bool {
    match t.rsplit_once('.') {
        Some((_, ext)) => {
            !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// Obsidian-style inline `#tags`, outside code. A tag needs one non-digit.
fn inline_tags(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut fence = false;
    for raw in body.lines() {
        let t = raw.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        let line = strip_inline_code(raw);
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let boundary = i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '(';
            if chars[i] == '#' && boundary {
                let mut j = i + 1;
                while j < chars.len()
                    && (chars[j].is_alphanumeric() || matches!(chars[j], '_' | '-' | '/'))
                {
                    j += 1;
                }
                let tag: String = chars[i + 1..j].iter().collect();
                let tag = tag.trim_end_matches(['-', '/']).to_string();
                if !tag.is_empty()
                    && tag.chars().any(|c| !c.is_ascii_digit())
                    && !out.iter().any(|x| x.eq_ignore_ascii_case(&tag))
                {
                    out.push(tag);
                }
                i = j.max(i + 1);
            } else {
                i += 1;
            }
        }
    }
    out
}

/// A filesystem-safe, lowercase ASCII slug of at most 80 chars.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 80 {
        out.truncate(80);
        while out.ends_with('-') {
            out.pop();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(src: &str) -> Note {
        Note::parse("folder/my-note", PathBuf::from("x.md"), src)
    }

    #[test]
    fn wikilink_forms() {
        assert_eq!(
            wikilinks("see [[A]], [[b|label]], [[c#Head]], [[d^blk]], [[e.md]], ![[img.png]], ![[f]] [[ ]]"),
            vec!["A", "b", "c", "d", "e", "f"]
        );
        assert_eq!(wikilinks("[[unclosed"), Vec::<String>::new());
    }

    #[test]
    fn typed_and_untyped_links() {
        let n = note(
            "---\ntitle: T\nrelated: [\"[[fm-target]]\"]\nparent: \"[[up]]\"\n---\n# Heading\nplain [[a]] and `[[code]]`\n- Mitigated By:: [[b]], [[c]]\nfixes:: [[d]]\n```\n[[fenced]]\n```\nsee [x](other%20note.md) and [y](https://x.io/a.md)\n",
        );
        let got: Vec<(String, String)> = n
            .links
            .iter()
            .map(|l| (l.target.clone(), l.rel.clone()))
            .collect();
        let want = [
            ("fm-target", "related"),
            ("up", "parent"),
            ("a", "related"),
            ("b", "mitigated-by"),
            ("c", "mitigated-by"),
            ("d", "fixes"),
            ("other note", "related"),
        ];
        let want: Vec<(String, String)> = want
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        assert_eq!(got, want);
        // `plain [[a]]` is body line 2, after 3 frontmatter lines and their 2 fences.
        assert_eq!(n.links[2].line, 7);
    }

    #[test]
    fn title_and_tags() {
        let n = note("# From Heading\ntext #sqli #django/orm `#nope` #123 a#b (#paren)\n");
        assert_eq!(n.title, "From Heading");
        assert_eq!(n.tags, vec!["sqli", "django/orm", "paren"]);
        let n = note("no heading");
        assert_eq!(n.title, "my-note");
        let n = note("---\ntags: [\"#a\", b]\ntype: pattern\n---\n#b #c\n");
        assert_eq!(n.tags, vec!["a", "b", "c"]);
        assert_eq!(n.kind.as_deref(), Some("pattern"));
    }

    #[test]
    fn slug() {
        assert_eq!(
            slugify("  Django .extra() bypasses ORM!! "),
            "django-extra-bypasses-orm"
        );
        assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
        assert_eq!(slugify("日本"), "");
        assert_eq!(slugify(&"a".repeat(200)).len(), 80);
    }

    #[test]
    fn rel_names() {
        assert_eq!(
            normalize_rel("Mitigated By").as_deref(),
            Some("mitigated-by")
        );
        assert_eq!(normalize_rel("9lives"), None);
        assert_eq!(normalize_rel("a/b"), None);
    }
}
