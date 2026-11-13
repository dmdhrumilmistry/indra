//! A small, forgiving YAML-frontmatter reader and editor.
//!
//! It understands the subset that notes actually use: `key: scalar`,
//! `key: [a, b]` inline lists and `key:` followed by `- item` block lists.
//! Anything else (nested maps, multi-line strings) is skipped, never fatal.
//! Edits are line-based so a hand-written frontmatter keeps its other keys,
//! comments and ordering.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    List(Vec<String>),
}

#[derive(Debug, Default, Clone)]
pub struct Frontmatter {
    pub entries: Vec<(String, Value)>,
}

impl Frontmatter {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    pub fn get_str(&self, key: &str) -> Option<String> {
        match self.get(key)? {
            Value::Str(s) if !s.is_empty() => Some(s.clone()),
            Value::List(l) => l.first().cloned(),
            _ => None,
        }
    }

    /// A list field. A scalar is split on commas (and on spaces for `tags`,
    /// which Obsidian also accepts as `tags: a b c`).
    pub fn get_list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Value::List(l)) => l.iter().filter(|s| !s.is_empty()).cloned().collect(),
            Some(Value::Str(s)) => {
                let space_split = key.eq_ignore_ascii_case("tags");
                s.split(|c: char| c == ',' || (space_split && c.is_whitespace()))
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            }
            None => Vec::new(),
        }
    }
}

/// Split a document into `(frontmatter, body)`. The frontmatter must open on
/// the very first line with `---` and close with `---` or `...`.
pub fn split(src: &str) -> (Option<&str>, &str) {
    let s = src.strip_prefix('\u{feff}').unwrap_or(src);
    let first_end = match s.find('\n') {
        Some(i) => i,
        None => return (None, s),
    };
    if s[..first_end].trim_end() != "---" {
        return (None, s);
    }
    let mut pos = first_end + 1;
    while pos <= s.len() {
        let end = s[pos..].find('\n').map(|i| pos + i).unwrap_or(s.len());
        let line = s[pos..end].trim_end();
        if line == "---" || line == "..." {
            let body_start = (end + 1).min(s.len());
            return (Some(&s[first_end + 1..pos]), &s[body_start..]);
        }
        if end == s.len() {
            break;
        }
        pos = end + 1;
    }
    (None, s)
}

pub fn parse(fm: &str) -> Frontmatter {
    let lines: Vec<&str> = fm.lines().collect();
    let mut entries = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || indented || t.starts_with('-') {
            i += 1;
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            i += 1;
            continue;
        };
        let key = unquote(k.trim());
        let v = strip_comment(v.trim());
        if v.is_empty() {
            let mut items = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let raw = lines[j];
                let tt = raw.trim();
                if let Some(rest) = tt.strip_prefix('-') {
                    if rest.is_empty() || rest.starts_with(' ') {
                        let item = unquote(strip_comment(rest.trim()));
                        if !item.is_empty() {
                            items.push(item);
                        }
                        j += 1;
                        continue;
                    }
                }
                if tt.is_empty() || raw.starts_with(' ') || raw.starts_with('\t') {
                    j += 1;
                    continue;
                }
                break;
            }
            entries.push((key, Value::List(items)));
            i = j;
            continue;
        }
        if v.starts_with('[') && v.ends_with(']') {
            entries.push((key, Value::List(split_inline(&v[1..v.len() - 1]))));
        } else {
            entries.push((key, Value::Str(unquote(v))));
        }
        i += 1;
    }
    Frontmatter { entries }
}

fn strip_comment(v: &str) -> &str {
    if v.starts_with('"') || v.starts_with('\'') || v.starts_with('[') {
        return v;
    }
    match v.find(" #") {
        Some(i) => v[..i].trim_end(),
        None => v,
    }
}

fn split_inline(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0i32;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                cur.push(c);
            }
            (Some(_), c) => cur.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                cur.push(c);
            }
            (None, '[') => {
                depth += 1;
                cur.push(c);
            }
            (None, ']') => {
                depth -= 1;
                cur.push(c);
            }
            (None, ',') if depth == 0 => {
                out.push(unquote(cur.trim()));
                cur.clear();
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(unquote(cur.trim()));
    }
    out.retain(|s| !s.is_empty());
    out
}

pub fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(o) => out.push(o),
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        return s[1..s.len() - 1].replace("''", "'");
    }
    s.to_string()
}

/// Quote a scalar only when YAML would otherwise misread it.
pub fn quote(s: &str) -> String {
    let needs = s.is_empty()
        || s != s.trim()
        || s.contains(": ")
        || s.ends_with(':')
        || s.contains(" #")
        || s.contains(['"', '\'', '[', ']', '{', '}', ',', '\n', '\t', '\\'])
        || s.starts_with(['-', '?', '!', '&', '*', '|', '>', '%', '@', '`', '#'])
        || matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "null" | "~" | "on" | "off"
        );
    if !needs {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn render_list(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| quote(s)).collect();
    format!("[{}]", parts.join(", "))
}

/// Set (or replace) one top-level key in a document's frontmatter, creating
/// the frontmatter block if there is none. `rendered` is the YAML value text.
/// The document must use `\n` line endings (callers normalise first).
pub fn set_field(doc: &str, key: &str, rendered: &str) -> String {
    let new_line = format!("{}: {}", key, rendered);
    let (fm, body) = split(doc);
    let Some(fm) = fm else {
        return format!("---\n{}\n---\n{}", new_line, doc);
    };
    let lines: Vec<&str> = fm.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 1);
    let mut replaced = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let is_key = !line.starts_with(' ')
            && !line.starts_with('\t')
            && line
                .split_once(':')
                .map(|(k, _)| unquote(k.trim()).eq_ignore_ascii_case(key))
                .unwrap_or(false);
        if is_key && !replaced {
            out.push(new_line.clone());
            replaced = true;
            i += 1;
            // Drop a block list or indented continuation that belonged to it.
            while i < lines.len() {
                let l = lines[i];
                let t = l.trim_start();
                if l.starts_with(' ') || l.starts_with('\t') || t.starts_with("- ") || t == "-" {
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    if !replaced {
        out.push(new_line);
    }
    format!("---\n{}\n---\n{}", out.join("\n"), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_parses() {
        let doc = "---\ntitle: \"Hello: world\"\ntags: [a, \"b c\", d]\naliases:\n  - one\n  - 'two'\nnested:\n  k: v\ntype: learning # trailing\n---\n# Body\n";
        let (fm, body) = split(doc);
        assert_eq!(body, "# Body\n");
        let fm = parse(fm.unwrap());
        assert_eq!(fm.get_str("title").unwrap(), "Hello: world");
        assert_eq!(fm.get_list("tags"), vec!["a", "b c", "d"]);
        assert_eq!(fm.get_list("aliases"), vec!["one", "two"]);
        assert_eq!(fm.get_str("type").unwrap(), "learning");
        assert_eq!(fm.get_list("nested"), Vec::<String>::new());
    }

    #[test]
    fn no_frontmatter_or_unterminated() {
        assert_eq!(split("# Hi\n"), (None, "# Hi\n"));
        assert_eq!(split("---\ntitle: x\n").0, None);
        let (fm, body) = split("---\r\ntitle: x\r\n---\r\nbody");
        assert!(fm.unwrap().contains("title"));
        assert_eq!(body, "body");
    }

    #[test]
    fn scalar_tags_split_on_space_and_comma() {
        let fm = parse("tags: sqli, django orm\n");
        assert_eq!(fm.get_list("tags"), vec!["sqli", "django", "orm"]);
    }

    #[test]
    fn quote_roundtrip() {
        for s in [
            "plain",
            "a: b",
            "yes",
            "-lead",
            "has \"q\"",
            "",
            "c#",
            "x # y",
        ] {
            let q = quote(s);
            let fm = parse(&format!("k: {}\n", q));
            assert_eq!(fm.get("k"), Some(&Value::Str(s.to_string())), "{}", q);
        }
        let list = vec!["a, b".to_string(), "c".to_string()];
        let fm = parse(&format!("k: {}\n", render_list(&list)));
        assert_eq!(fm.get_list("k"), list);
    }

    #[test]
    fn set_field_replaces_block_list_and_keeps_others() {
        let doc = "---\ntitle: T\ntags:\n  - a\n  - b\n# keep me\nupdated: old\n---\nbody\n";
        let out = set_field(doc, "tags", "[x]");
        assert_eq!(
            out,
            "---\ntitle: T\ntags: [x]\n# keep me\nupdated: old\n---\nbody\n"
        );
        let out = set_field(&out, "updated", "new");
        assert!(out.contains("updated: new") && !out.contains("old"));
        let out = set_field("just body\n", "title", "T");
        assert_eq!(out, "---\ntitle: T\n---\njust body\n");
    }
}
