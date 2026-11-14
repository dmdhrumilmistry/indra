//! `indra dev site`: build the project website into a folder ready for GitHub
//! Pages.
//!
//! The landing page is hand-written (`site/`). The documentation pages are
//! rendered from the Markdown already in the repository (README.md,
//! tools/indra/README.md, docs/), listed in `site/pages.toml`, so the docs have
//! one source of truth and the website never sends readers to GitHub to read
//! them. The renderer covers what the docs use: headings, paragraphs, lists,
//! tables, fenced code, block quotes, rules, raw HTML blocks, links, images,
//! emphasis and inline code. Links between documents become links between
//! pages; links to anything else in the repository go to GitHub. A link to a
//! heading that does not exist, or a broken link in the built site, fails the
//! build.

use clap::Args;
use fancy_regex::Regex;

type Captures<'t> = fancy_regex::Captures<'t, str>;
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

const GH: &str = "https://github.com/dmdhrumilmistry/indra";
const SITE_URL: &str = "https://dmdhrumilmistry.github.io/indra/";

#[derive(Args)]
pub struct SiteArgs {
    /// Where to write the site (default: _site in the repository)
    #[arg(long)]
    out: Option<PathBuf>,
    /// The repository (default: found from this folder up)
    #[arg(long)]
    repo: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Pages {
    page: Vec<Page>,
    #[serde(default)]
    alias: Vec<Alias>,
}

#[derive(Deserialize, Clone)]
struct Page {
    group: String,
    slug: String,
    title: String,
    description: String,
    source: Vec<Source>,
}

#[derive(Deserialize, Clone)]
struct Source {
    file: String,
    /// H2 titles to take ("" is the text before the first H2); none: the whole file.
    sections: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Alias {
    file: String,
    heading: String,
    page: String,
    id: String,
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid pattern")
}

macro_rules! pat {
    ($name:ident, $p:expr) => {
        fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| re($p))
        }
    };
}

pat!(re_fence, r"^\s*(```+|~~~+)");
pat!(
    re_html_block,
    r"(?i)^\s*<(p|div|table|img|details|h[1-6]|picture|figure|ul|ol|br|hr)\b"
);
pat!(re_list_item, r"^(\s*)([-*+]|\d+[.)])\s+");
pat!(re_heading, r"^(#{1,6})\s+(.*)$");
pat!(re_heading_start, r"^#{1,6}\s");
pat!(re_hr, r"^(-{3,}|\*{3,}|_{3,})$");
pat!(re_table_sep, r"^\s*\|?\s*:?-{2,}");
pat!(re_quote, r"^\s*>\s?");
pat!(re_code, r"(`+)(.+?)\1");
pat!(re_autolink, r"<((?:https?://|mailto:)[^>\s]+)>");
pat!(
    re_inline_tag,
    r"(?i)&lt;(/?(?:b|i|em|strong|sub|sup|br|kbd|code|s|u))(\s*/?)&gt;"
);
pat!(
    re_img,
    r"!\[([^\]]*)\]\(\s*([^)\s]+)(?:\s+&quot;[^)]*&quot;)?\s*\)"
);
pat!(re_link, r"\[((?:[^\[\]]|\[[^\]]*\])+)\]\(\s*([^)\s]+)\s*\)");
pat!(re_bare_url, r#"(?<![\w"=/])(https?://[^\s<)]+[^\s<).,;:])"#);
pat!(re_strong, r"(?s)\*\*(?=\S)(.+?)(?<=\S)\*\*");
pat!(re_em, r"(?<![\w*])\*(?=\S)([^*]+?)(?<=\S)\*(?![\w*])");
pat!(re_em2, r"(?<![\w_])_(?=\S)([^_]+?)(?<=\S)_(?![\w_])");
pat!(re_hold, "\x00(\\d+)\x00");
pat!(re_raw_attr, r#"\b(href|src)="([^"]*)""#);
pat!(re_tag, r"<[^>]+>");
pat!(re_md_link, r"!?\[([^\]]*)\]\([^)]*\)");
pat!(re_not_word, r"[^\w\- ]");
pat!(re_trailing_hashes, r"\s+#+\s*$");
pat!(re_a_tags, r"<a [^>]*>|</a>");
pat!(re_check_ref, r##"\b(?:href|src)="([^"#:]+)(?:#([^"]*))?""##);
pat!(re_check_frag, r##"\bhref="#([^"]+)""##);

fn caps<'t>(r: &Regex, s: &'t str) -> Option<Captures<'t>> {
    r.captures(s).ok().flatten()
}

fn is_match(r: &Regex, s: &str) -> bool {
    r.is_match(s).unwrap_or(false)
}

fn sub(r: &Regex, s: &str, f: impl Fn(&Captures) -> String) -> String {
    r.replace_all(s, |c: &Captures| f(c)).into_owned()
}

fn g<'a>(c: &'a Captures, i: usize) -> &'a str {
    c.get(i).map(|m| m.as_str()).unwrap_or("")
}

/// Leading whitespace characters.
fn lead(s: &str) -> usize {
    s.chars().take_while(|c| c.is_whitespace()).count()
}

/// `s` without its first `n` characters.
fn skip(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

fn take(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn fence_of(line: &str) -> Option<String> {
    caps(re_fence(), line).map(|c| g(&c, 1).to_string())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn attr(s: &str) -> String {
    esc(s).replace('"', "&quot;").replace('\'', "&#x27;")
}

/// The entities this renderer (and the docs) produce.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find(';').filter(|e| *e <= 10);
        let ent = end.map(|e| &tail[1..e]);
        let rep = match ent {
            Some("amp") => Some("&".to_string()),
            Some("lt") => Some("<".to_string()),
            Some("gt") => Some(">".to_string()),
            Some("quot") => Some("\"".to_string()),
            Some("apos") => Some("'".to_string()),
            Some(e) if e.starts_with("#x") || e.starts_with("#X") => {
                u32::from_str_radix(&e[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
            }
            Some(e) if e.starts_with('#') => e[1..]
                .parse()
                .ok()
                .and_then(char::from_u32)
                .map(String::from),
            _ => None,
        };
        match (rep, end) {
            (Some(r), Some(e)) => {
                out.push_str(&r);
                rest = &tail[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// GitHub's heading anchors.
fn slugify(text: &str) -> String {
    let t = re_tag().replace_all(text, "").into_owned();
    let t = sub(re_md_link(), &t, |c| g(c, 1).to_string());
    let t = t.trim().to_lowercase();
    let t = re_not_word().replace_all(&t, "").into_owned();
    t.replace(' ', "-")
}

fn heading_text(raw: &str) -> String {
    re_trailing_hashes().replace_all(raw, "").trim().to_string()
}

/// Lexical normpath of a relative, `/`-separated path.
fn normpath(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if parts.last().map(|l| *l != "..").unwrap_or(false) {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            x => parts.push(x),
        }
    }
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

fn dirname(p: &str) -> &str {
    p.rfind('/').map(|i| &p[..i]).unwrap_or("")
}

// ---- sources ------------------------------------------------------------------------

/// [(title or "", lines)] split at H2 headings outside code fences.
fn split_h2(lines: &[String]) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let (mut cur, mut title, mut fence): (Vec<String>, String, Option<String>) =
        (vec![], String::new(), None);
    for ln in lines {
        let f = fence_of(ln);
        if let Some(fe) = &fence {
            if let Some(f) = &f {
                if f.starts_with(&fe[..1]) && f.len() >= fe.len() && ln.trim() == f {
                    fence = None;
                }
            }
        } else if f.is_some() {
            fence = f;
        } else if let Some(t) = ln.strip_prefix("## ") {
            out.push((std::mem::take(&mut title), std::mem::take(&mut cur)));
            title = t.trim().to_string();
            continue;
        }
        cur.push(ln.clone());
    }
    out.push((title, cur));
    out
}

/// Skip a README's decorative header: HTML blocks and an H1 before the text.
fn drop_leading_html(lines: &[String]) -> Vec<String> {
    let mut i = 0;
    while i < lines.len() {
        let s = lines[i].trim();
        if s.is_empty() || s.starts_with('<') || s.starts_with("# ") {
            if s.starts_with('<') {
                while i < lines.len() && !lines[i].trim().is_empty() {
                    i += 1;
                }
                continue;
            }
            i += 1;
            continue;
        }
        break;
    }
    lines[i..].to_vec()
}

type Chunk = (String, Vec<String>, Option<String>);

fn page_markdown(repo: &Path, sources: &[Source]) -> Result<Vec<Chunk>, String> {
    let mut chunks = Vec::new();
    for s in sources {
        let text = std::fs::read_to_string(repo.join(&s.file))
            .map_err(|e| format!("{}: {}", s.file, e))?;
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        match &s.sections {
            None => {
                if lines.first().map(|l| l.starts_with("# ")).unwrap_or(false) {
                    lines.remove(0);
                }
                chunks.push((s.file.clone(), lines, None));
            }
            Some(sections) => {
                let parts: HashMap<String, Vec<String>> = split_h2(&lines).into_iter().collect();
                for name in sections {
                    let body = parts
                        .get(name)
                        .ok_or_else(|| format!("{} has no section {:?}", s.file, name))?;
                    if name.is_empty() {
                        chunks.push((s.file.clone(), drop_leading_html(body), None));
                    } else {
                        chunks.push((s.file.clone(), body.clone(), Some(name.clone())));
                    }
                }
            }
        }
    }
    Ok(chunks)
}

// ---- links --------------------------------------------------------------------------

struct Links {
    repo: PathBuf,
    anchors: HashMap<(String, String), (String, String)>,
    first_page: HashMap<String, String>,
    aliases: HashMap<(String, String), (String, String)>,
    errors: RefCell<Vec<String>>,
}

impl Links {
    fn register(&mut self, src: &str, hid: &str, slug: &str, page_id: &str) {
        self.anchors
            .entry((src.into(), hid.into()))
            .or_insert((slug.into(), page_id.into()));
        self.first_page.entry(src.into()).or_insert(slug.into());
    }

    /// Rewrite a link found in `src`, rendered on page `here`.
    fn resolve(&self, href: &str, src: &str, here: &str) -> String {
        if href.is_empty()
            || ["http://", "https://", "mailto:", "data:"]
                .iter()
                .any(|p| href.starts_with(p))
        {
            return href.to_string();
        }
        if let Some(frag) = href.strip_prefix('#') {
            return self.page_link(src, frag, here, href);
        }
        let (path, frag) = href.split_once('#').unwrap_or((href, ""));
        let dir = dirname(src);
        let target = normpath(&if dir.is_empty() {
            path.to_string()
        } else {
            format!("{}/{}", dir, path)
        });
        if target.starts_with("../") {
            return href.to_string();
        }
        if self.first_page.contains_key(&target) {
            return self.page_link(&target, frag, here, href);
        }
        if target == "assets" || target.starts_with("assets/") {
            return format!("../{}", target);
        }
        let kind = if self.repo.join(&target).is_dir() {
            "tree"
        } else {
            "blob"
        };
        format!(
            "{}/{}/main/{}{}",
            GH,
            kind,
            target,
            if frag.is_empty() {
                String::new()
            } else {
                format!("#{}", frag)
            }
        )
    }

    fn page_link(&self, src: &str, frag: &str, here: &str, original: &str) -> String {
        if frag.is_empty() {
            let slug = &self.first_page[src];
            return if slug != here {
                format!("{}.html", slug)
            } else {
                "#".into()
            };
        }
        let key = (src.to_string(), frag.to_string());
        let Some((slug, pid)) = self.anchors.get(&key).or_else(|| self.aliases.get(&key)) else {
            self.errors.borrow_mut().push(format!(
                "{}: link '{}' on page {} points to a heading that does not exist",
                src, original, here
            ));
            return original.to_string();
        };
        let page = if slug == here {
            String::new()
        } else {
            format!("{}.html", slug)
        };
        let out = page
            + &if pid.is_empty() {
                String::new()
            } else {
                format!("#{}", pid)
            };
        if out.is_empty() {
            "#".into()
        } else {
            out
        }
    }
}

// ---- markdown -----------------------------------------------------------------------

struct Renderer<'a> {
    links: &'a Links,
    src: String,
    here: String,
    ids: HashMap<String, usize>,
    toc: Vec<(usize, String, String)>,
}

impl Renderer<'_> {
    fn inline(&self, text: &str) -> String {
        let codes: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let hold = |s: String| {
            let mut c = codes.borrow_mut();
            c.push(s);
            format!("\x00{}\x00", c.len() - 1)
        };
        let text = sub(re_code(), text, |c| {
            let inner = g(c, 2);
            let inner = if inner.trim().is_empty() {
                inner
            } else {
                inner.trim()
            };
            hold(format!("<code>{}</code>", esc(inner)))
        });
        let text = sub(re_autolink(), &text, |c| {
            hold(format!(
                "<a href=\"{}\" rel=\"noopener\">{}</a>",
                attr(g(c, 1)),
                esc(g(c, 1))
            ))
        });
        let text = esc(&text);
        let text = sub(re_inline_tag(), &text, |c| {
            format!("<{}{}>", g(c, 1), g(c, 2))
        });
        let text = sub(re_img(), &text, |c| {
            let src = self
                .links
                .resolve(&unescape(g(c, 2)), &self.src, &self.here);
            format!(
                "<img src=\"{}\" alt=\"{}\" loading=\"lazy\">",
                attr(&src),
                attr(&unescape(g(c, 1)))
            )
        });
        let text = sub(re_link(), &text, |c| {
            let href = self
                .links
                .resolve(&unescape(g(c, 2)), &self.src, &self.here);
            let ext = if href.starts_with("http") {
                " rel=\"noopener\""
            } else {
                ""
            };
            format!("<a href=\"{}\"{}>{}</a>", attr(&href), ext, g(c, 1))
        });
        let text = sub(re_bare_url(), &text, |c| {
            format!("<a href=\"{0}\" rel=\"noopener\">{0}</a>", g(c, 1))
        });
        let text = sub(re_strong(), &text, |c| {
            format!("<strong>{}</strong>", g(c, 1))
        });
        let text = sub(re_em(), &text, |c| format!("<em>{}</em>", g(c, 1)));
        let text = sub(re_em2(), &text, |c| format!("<em>{}</em>", g(c, 1)));
        let codes = codes.borrow();
        sub(re_hold(), &text, |c| {
            codes[g(c, 1).parse::<usize>().unwrap_or(0)].clone()
        })
    }

    fn raw_html(&self, block: &str) -> String {
        sub(re_raw_attr(), block, |c| {
            format!(
                "{}=\"{}\"",
                g(c, 1),
                attr(
                    &self
                        .links
                        .resolve(&unescape(g(c, 2)), &self.src, &self.here)
                )
            )
        })
    }

    fn heading(&mut self, level: usize, raw: &str) -> String {
        let text = heading_text(raw);
        let base = slugify(&text);
        let n = *self.ids.get(&base).unwrap_or(&0);
        self.ids.insert(base.clone(), n + 1);
        let hid = if n == 0 {
            base
        } else {
            format!("{}-{}", base, n)
        };
        let html = self.inline(&text);
        if level == 2 || level == 3 {
            self.toc.push((level, hid.clone(), html.clone()));
        }
        format!("<h{0} id=\"{1}\">{2}</h{0}>", level, hid, html)
    }

    fn is_table_start(lines: &[String], i: usize) -> bool {
        lines[i].trim().starts_with('|')
            && i + 1 < lines.len()
            && is_match(re_table_sep(), &lines[i + 1])
    }

    fn starts_block(lines: &[String], i: usize) -> bool {
        let ln = &lines[i];
        let s = ln.trim();
        fence_of(ln).is_some()
            || is_match(re_heading_start(), s)
            || s.starts_with('>')
            || is_match(re_html_block(), ln)
            || is_match(re_list_item(), ln)
            || Self::is_table_start(lines, i)
    }

    fn blocks(&mut self, lines: &[String]) -> String {
        let mut out: Vec<String> = Vec::new();
        let n = lines.len();
        let mut i = 0;
        while i < n {
            let ln = &lines[i];
            let s = ln.trim();
            if s.is_empty() {
                i += 1;
                continue;
            }
            if let Some(f) = fence_of(ln) {
                let after = skip(s, f.chars().count()).trim();
                let lang = after.split_whitespace().next().unwrap_or("").to_string();
                let indent = lead(ln);
                let fch = f.chars().next().unwrap();
                let closes = |l: &str| {
                    let st = l.trim();
                    let prefix: String = std::iter::repeat_n(fch, f.chars().count()).collect();
                    st.starts_with(&prefix)
                        && st.chars().all(|c| c == st.chars().next().unwrap())
                        && st.chars().count() >= f.chars().count()
                };
                let mut body: Vec<String> = Vec::new();
                i += 1;
                while i < n && !closes(&lines[i]) {
                    let l = &lines[i];
                    body.push(if take(l, indent).trim().is_empty() {
                        skip(l, indent).to_string()
                    } else {
                        l.clone()
                    });
                    i += 1;
                }
                i += 1;
                let cls = if lang.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"language-{}\"", attr(&lang))
                };
                out.push(format!(
                    "<pre><code{}>{}</code></pre>",
                    cls,
                    esc(&body.join("\n"))
                ));
                continue;
            }
            if let Some(c) = caps(re_heading(), s) {
                let (lvl, raw) = (g(&c, 1).len(), g(&c, 2).to_string());
                out.push(self.heading(lvl, &raw));
                i += 1;
                continue;
            }
            if is_match(re_hr(), s) {
                out.push("<hr>".into());
                i += 1;
                continue;
            }
            if is_match(re_html_block(), ln) {
                let mut block = Vec::new();
                while i < n && !lines[i].trim().is_empty() {
                    block.push(lines[i].clone());
                    i += 1;
                }
                out.push(self.raw_html(&block.join("\n")));
                continue;
            }
            if Self::is_table_start(lines, i) {
                let mut rows = Vec::new();
                while i < n && lines[i].trim().starts_with('|') {
                    rows.push(lines[i].trim().to_string());
                    i += 1;
                }
                out.push(self.table(&rows));
                continue;
            }
            if s.starts_with('>') {
                let mut quote = Vec::new();
                while i < n && lines[i].trim().starts_with('>') {
                    quote.push(re_quote().replace(&lines[i], "").into_owned());
                    i += 1;
                }
                let inner = self.blocks(&quote);
                out.push(format!("<blockquote>{}</blockquote>", inner));
                continue;
            }
            if is_match(re_list_item(), ln) {
                let (html, next) = self.list(lines, i);
                out.push(html);
                i = next;
                continue;
            }
            let mut para = Vec::new();
            while i < n && !lines[i].trim().is_empty() && !Self::starts_block(lines, i) {
                para.push(lines[i].clone());
                i += 1;
            }
            let text: Vec<String> = para
                .iter()
                .map(|p| {
                    if p.ends_with("  ") {
                        format!("{}<br>", p.trim_end())
                    } else {
                        p.trim_end().to_string()
                    }
                })
                .collect();
            out.push(format!(
                "<p>{}</p>",
                self.inline(&text.join("\n")).replace("&lt;br&gt;", "<br>")
            ));
        }
        out.join("\n")
    }

    fn list(&mut self, lines: &[String], mut i: usize) -> (String, usize) {
        let li = |l: &str| {
            caps(re_list_item(), l).map(|c| {
                (
                    g(&c, 1).chars().count(),
                    g(&c, 2).to_string(),
                    g(&c, 0).chars().count(),
                )
            })
        };
        let (indent, marker, _) = li(&lines[i]).unwrap();
        let ordered = marker.starts_with(|c: char| c.is_ascii_digit());
        let first: usize = if ordered {
            marker[..marker.len() - 1].parse().unwrap_or(1)
        } else {
            1
        };
        let n = lines.len();
        let mut items: Vec<Vec<String>> = Vec::new();
        while i < n {
            let Some((ind, mk, content_indent)) = li(&lines[i]) else {
                break;
            };
            if ind != indent || mk.starts_with(|c: char| c.is_ascii_digit()) != ordered {
                break;
            }
            let mut body = vec![skip(&lines[i], content_indent).to_string()];
            i += 1;
            while i < n {
                let ln = &lines[i];
                if ln.trim().is_empty() {
                    // A blank line ends the item unless the next line is indented into it.
                    if i + 1 < n && !lines[i + 1].trim().is_empty() && lead(&lines[i + 1]) > indent
                    {
                        body.push(String::new());
                        i += 1;
                        continue;
                    }
                    break;
                }
                if let Some((mi, _, _)) = li(ln) {
                    if mi <= indent {
                        break;
                    }
                }
                let ld = lead(ln);
                if ld > indent {
                    body.push(skip(ln, ld.min(content_indent)).to_string());
                } else if fence_of(ln).is_some()
                    || is_match(re_heading_start(), ln)
                    || is_match(re_html_block(), ln)
                {
                    break;
                } else {
                    body.push(ln.trim().to_string()); // a lazy continuation line
                }
                i += 1;
            }
            items.push(body);
            // Tolerate a single blank line between items.
            if i < n && lines[i].trim().is_empty() && i + 1 < n {
                if let Some((mn, _, _)) = li(&lines[i + 1]) {
                    if mn == indent {
                        i += 1;
                    }
                }
            }
        }
        let tag = if ordered { "ol" } else { "ul" };
        let start = if ordered && first != 1 {
            format!(" start=\"{}\"", first)
        } else {
            String::new()
        };
        let mut parts = Vec::new();
        for body in items {
            let nested = body[1..].iter().any(|b| {
                is_match(re_list_item(), b) || fence_of(b).is_some() || b.trim().is_empty()
            });
            let inner = if nested {
                let mut inner = self.blocks(&body);
                if inner.starts_with("<p>") && inner.matches("<p>").count() == 1 {
                    inner = inner[3..].replacen("</p>", "", 1);
                }
                inner
            } else {
                self.inline(&body.join("\n"))
            };
            parts.push(format!("<li>{}</li>", inner));
        }
        (format!("<{0}{1}>{2}</{0}>", tag, start, parts.join("")), i)
    }

    fn table(&self, rows: &[String]) -> String {
        fn cells(row: &str) -> Vec<String> {
            let row: Vec<char> = row.trim().trim_matches('|').chars().collect();
            let (mut out, mut cur, mut code) = (Vec::new(), String::new(), false);
            let mut j = 0;
            while j < row.len() {
                let c = row[j];
                if c == '\\' && j + 1 < row.len() && row[j + 1] == '|' {
                    cur.push('|');
                    j += 2;
                    continue;
                }
                if c == '`' {
                    code = !code;
                }
                if c == '|' && !code {
                    out.push(cur.trim().to_string());
                    cur.clear();
                } else {
                    cur.push(c);
                }
                j += 1;
            }
            out.push(cur.trim().to_string());
            out
        }
        let head = cells(&rows[0]);
        let aligns: Vec<&str> = cells(&rows[1])
            .iter()
            .map(|c| {
                if c.ends_with(':') && !c.starts_with(':') {
                    "right"
                } else if c.starts_with(':') && c.ends_with(':') {
                    "center"
                } else {
                    ""
                }
            })
            .collect();
        let tr = |cs: &[String], tag: &str| {
            let mut res = String::new();
            for (k, c) in cs.iter().enumerate() {
                let al = aligns.get(k).copied().unwrap_or("");
                let st = if al.is_empty() {
                    String::new()
                } else {
                    format!(" style=\"text-align:{}\"", al)
                };
                res.push_str(&format!("<{0}{1}>{2}</{0}>", tag, st, self.inline(c)));
            }
            format!("<tr>{}</tr>", res)
        };
        let body: String = rows[2..].iter().map(|r| tr(&cells(r), "td")).collect();
        format!(
            "<div class=\"table\"><table><thead>{}</thead><tbody>{}</tbody></table></div>",
            tr(&head, "th"),
            body
        )
    }
}

// ---- pages --------------------------------------------------------------------------

fn nav_html(pages: &[Page], active: &str) -> String {
    let mut groups: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for p in pages {
        if groups.last().map(|g| g.0 != p.group).unwrap_or(true) {
            groups.push((p.group.clone(), vec![]));
        }
        groups
            .last_mut()
            .unwrap()
            .1
            .push((p.slug.clone(), p.title.clone()));
    }
    let mut out = Vec::new();
    for (g, items) in groups {
        out.push(format!("<p>{}</p><ul>", esc(&g)));
        for (slug, title) in items {
            let cur = if slug == active {
                " aria-current=\"page\""
            } else {
                ""
            };
            out.push(format!(
                "<li><a href=\"{}.html\"{}>{}</a></li>",
                slug,
                cur,
                esc(&title)
            ));
        }
        out.push("</ul>".into());
    }
    out.join("\n")
}

const TEMPLATE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} - indra docs</title>
<meta name="description" content="{desc}">
<meta property="og:title" content="{title} - indra docs">
<meta property="og:description" content="{desc}">
<meta property="og:image" content="{site}assets/social.png">
<meta property="og:type" content="article">
<link rel="canonical" href="{site}docs/{slug}.html">
<link rel="icon" href="../assets/icon.svg" type="image/svg+xml">
<link rel="stylesheet" href="../style.css">
</head>
<body class="docs-page">
<header class="nav">
  <div class="wrap nav-in">
    <a class="brand" href="../index.html"><img src="../assets/logo.svg" alt="" width="30" height="30"><span>indra</span></a>
    <nav aria-label="Site">
      <a href="../index.html#pipeline">Pipeline</a>
      <a href="../index.html#memory">Memory</a>
      <a href="../index.html#console">Console</a>
      <a href="../index.html#install">Install</a>
      <a class="gh" href="index.html" aria-current="page">Docs</a>
    </nav>
  </div>
</header>

<div class="wrap docs">
  <details class="side" open>
    <summary>Menu <span>{title}</span></summary>
    <nav aria-label="Documentation">
{nav}
    </nav>
  </details>
  <main class="doc" id="top">
    <p class="eyebrow">{group}</p>
    <h1>{title}</h1>
    <p class="sub">{desc}</p>
{body}
    <nav class="pager" aria-label="Previous and next page">{pager}</nav>
  </main>
  <aside class="toc" aria-label="On this page">
{toc}
  </aside>
</div>

<footer class="foot">
  <div class="wrap foot-in">
    <span class="brand"><img src="../assets/logo.svg" alt="" width="22" height="22"><span>indra</span></span>
    <span>MIT licensed &middot; static analysis of code you are authorized to review</span>
    <a href="{gh}" rel="noopener">Source code</a>
  </div>
</footer>
<script src="../main.js"></script>
</body>
</html>
"#;

/// Fill `{name}` placeholders in one pass, so filled-in text is never re-read.
fn fill(t: &str, vars: &HashMap<&str, String>) -> String {
    let mut out = String::with_capacity(t.len() * 2);
    let mut rest = t;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        match tail.find('}').map(|e| (&tail[1..e], e)) {
            Some((k, e)) if vars.contains_key(k) => {
                out.push_str(&vars[k]);
                rest = &tail[e + 1..];
            }
            _ => {
                out.push('{');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn copy_dir(from: &Path, to: &Path, skip_name: Option<&str>) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let name = e.file_name();
        if skip_name.map(|s| name == s).unwrap_or(false) {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(&name), None)?;
        } else {
            std::fs::copy(&p, to.join(&name)).map_err(|e| format!("{}: {}", p.display(), e))?;
        }
    }
    Ok(())
}

fn build(repo: &Path, out: &Path) -> Result<usize, String> {
    let cfg: Pages = toml::from_str(
        &std::fs::read_to_string(repo.join("site").join("pages.toml"))
            .map_err(|e| format!("site/pages.toml: {}", e))?,
    )
    .map_err(|e| format!("site/pages.toml: {}", e))?;
    let pages = cfg.page;
    if out.exists() {
        std::fs::remove_dir_all(out).map_err(|e| e.to_string())?;
    }
    copy_dir(&repo.join("site"), out, Some("pages.toml"))?;
    copy_dir(&repo.join("assets"), &out.join("assets"), None)?;
    std::fs::write(out.join(".nojekyll"), "").map_err(|e| e.to_string())?;
    std::fs::create_dir_all(out.join("docs")).map_err(|e| e.to_string())?;

    let mut links = Links {
        repo: repo.to_path_buf(),
        anchors: HashMap::new(),
        first_page: HashMap::new(),
        aliases: cfg
            .alias
            .iter()
            .map(|a| {
                (
                    (a.file.clone(), a.heading.clone()),
                    (a.page.clone(), a.id.clone()),
                )
            })
            .collect(),
        errors: RefCell::new(vec![]),
    };
    // Pass 1: every heading's id, so links can point across pages.
    for p in &pages {
        for s in &p.source {
            links
                .first_page
                .entry(s.file.clone())
                .or_insert(p.slug.clone());
        }
    }
    let mut chunks_of: Vec<Vec<Chunk>> = Vec::new();
    for p in &pages {
        let chunks = page_markdown(repo, &p.source)?;
        let mut ids: HashMap<String, usize> = HashMap::new();
        let mut take_id = |base: &str| {
            let k = *ids.get(base).unwrap_or(&0);
            ids.insert(base.to_string(), k + 1);
            if k == 0 {
                base.to_string()
            } else {
                format!("{}-{}", base, k)
            }
        };
        for (src, lines, section) in &chunks {
            if let Some(sec) = section {
                // A section that is the page itself (the only one, or named like
                // the page) is not repeated as a heading.
                let hid = slugify(sec);
                let top = chunks.len() == 1 || *sec == p.title;
                let pid = if top { String::new() } else { take_id(&hid) };
                links.register(src, &hid, &p.slug, &pid);
            }
            let mut fence: Option<String> = None;
            for ln in lines {
                let f = fence_of(ln);
                if fence.is_some() {
                    if f.as_deref().map(|f| ln.trim() == f).unwrap_or(false) {
                        fence = None;
                    }
                    continue;
                }
                if f.is_some() {
                    fence = f;
                    continue;
                }
                if let Some(c) = caps(re_heading(), ln.trim()) {
                    let base = slugify(&heading_text(g(&c, 2)));
                    let pid = take_id(&base);
                    links.register(src, &base, &p.slug, &pid);
                }
            }
        }
        chunks_of.push(chunks);
    }

    // Pass 2: render.
    for (idx, p) in pages.iter().enumerate() {
        let chunks = &chunks_of[idx];
        let mut r = Renderer {
            links: &links,
            src: chunks.first().map(|c| c.0.clone()).unwrap_or_default(),
            here: p.slug.clone(),
            ids: HashMap::new(),
            toc: vec![],
        };
        let mut body = Vec::new();
        for (src, lines, section) in chunks {
            r.src = src.clone();
            if let Some(sec) = section {
                if chunks.len() > 1 && *sec != p.title {
                    body.push(r.heading(2, sec));
                }
            }
            body.push(r.blocks(lines));
        }
        let toc = if r.toc.is_empty() {
            String::new()
        } else {
            format!(
                "<p>On this page</p><ul>{}</ul>",
                r.toc
                    .iter()
                    .map(|(lvl, hid, txt)| format!(
                        "<li class=\"l{}\"><a href=\"#{}\">{}</a></li>",
                        lvl,
                        hid,
                        re_a_tags().replace_all(txt, "")
                    ))
                    .collect::<String>()
            )
        };
        let prev = if idx > 0 {
            let q = &pages[idx - 1];
            format!(
                "<a class=\"prev\" href=\"{}.html\"><span>Previous</span>{}</a>",
                q.slug,
                esc(&q.title)
            )
        } else {
            "<span></span>".into()
        };
        let next = pages
            .get(idx + 1)
            .map(|q| {
                format!(
                    "<a class=\"next\" href=\"{}.html\"><span>Next</span>{}</a>",
                    q.slug,
                    esc(&q.title)
                )
            })
            .unwrap_or_default();
        let vars: HashMap<&str, String> = [
            ("title", esc(&p.title)),
            ("desc", attr(&p.description)),
            ("site", SITE_URL.to_string()),
            ("slug", p.slug.clone()),
            ("gh", GH.to_string()),
            ("nav", nav_html(&pages, &p.slug)),
            ("group", esc(&p.group)),
            ("body", body.join("\n")),
            ("pager", prev + &next),
            ("toc", toc),
        ]
        .into_iter()
        .collect();
        std::fs::write(
            out.join("docs").join(format!("{}.html", p.slug)),
            fill(TEMPLATE, &vars),
        )
        .map_err(|e| e.to_string())?;
    }
    let errors = links.errors.borrow();
    if !errors.is_empty() {
        return Err(errors
            .iter()
            .map(|e| format!("build-site: {}", e))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    Ok(pages.len())
}

fn html_files(d: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                html_files(&p, out);
            } else if p.extension().map(|x| x == "html").unwrap_or(false) {
                out.push(p);
            }
        }
    }
}

/// Every relative href and src in the built site must exist, anchors too.
fn check_site_links(out: &Path) -> Vec<String> {
    let mut bad = Vec::new();
    let mut pages = Vec::new();
    html_files(out, &mut pages);
    pages.sort();
    let has_id = |text: &str, id: &str| {
        regex::Regex::new(&format!(r#"\bid="{}""#, regex::escape(id)))
            .map(|r| r.is_match(text))
            .unwrap_or(false)
    };
    for page in &pages {
        let text = std::fs::read_to_string(page).unwrap_or_default();
        let rel = page.strip_prefix(out).unwrap_or(page).display().to_string();
        for c in re_check_ref().captures_iter(text.as_str()).flatten() {
            let target_rel = unescape(g(&c, 1));
            let joined = page.parent().unwrap().join(&target_rel);
            let target = PathBuf::from(normpath(&joined.to_string_lossy().replace('\\', "/")));
            let target = if joined.is_absolute() {
                PathBuf::from("/").join(target)
            } else {
                target
            };
            if !target.exists() {
                bad.push(format!("{} -> {}", rel, g(&c, 1)));
                continue;
            }
            let frag = g(&c, 2);
            if !frag.is_empty() && target.extension().map(|x| x == "html").unwrap_or(false) {
                let t = std::fs::read_to_string(&target).unwrap_or_default();
                if !has_id(&t, frag) {
                    bad.push(format!("{} -> {}#{} (no such anchor)", rel, g(&c, 1), frag));
                }
            }
        }
        for c in re_check_frag().captures_iter(text.as_str()).flatten() {
            if !has_id(&text, g(&c, 1)) {
                bad.push(format!("{} -> #{} (no such anchor)", rel, g(&c, 1)));
            }
        }
    }
    bad
}

pub fn run(a: SiteArgs) -> Result<ExitCode, String> {
    let repo = match a.repo {
        Some(r) => r,
        None => {
            let mut d = std::env::current_dir().ok();
            loop {
                match d {
                    Some(x) if x.join("site").join("pages.toml").is_file() => break x,
                    Some(x) => d = x.parent().map(Path::to_path_buf),
                    None => return Err("no site/pages.toml here or above; run inside the indra repository or pass --repo".into()),
                }
            }
        }
    };
    let out = std::path::absolute(a.out.unwrap_or_else(|| repo.join("_site")))
        .map_err(|e| e.to_string())?;
    let n = match build(&repo, &out) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{}", e);
            return Ok(ExitCode::from(1));
        }
    };
    let bad = check_site_links(&out);
    if !bad.is_empty() {
        eprintln!("build-site: broken links:\n  {}", bad.join("\n  "));
        return Ok(ExitCode::from(1));
    }
    println!("built {}: landing page + {} docs pages", out.display(), n);
    Ok(ExitCode::SUCCESS)
}

/// `indra dev dashes`: find (or with `--fix`, replace) em dashes, en dashes
/// and horizontal bars in the repository's text files. The project writes a
/// plain hyphen instead (see AGENTS.md).
pub fn dashes(repo: Option<PathBuf>, fix: bool) -> Result<ExitCode, String> {
    let root = match repo {
        Some(r) => r,
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    const SKIP: [&str; 5] = [".git", "target", "_site", "node_modules", "vendor"];
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                if !SKIP.contains(&name.as_str()) {
                    walk(&p, out);
                }
            } else {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    let bad = ['\u{2014}', '\u{2013}', '\u{2015}'];
    let mut hits = 0;
    for p in files {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        if !text.contains(bad) {
            continue;
        }
        hits += 1;
        let rel = p.strip_prefix(&root).unwrap_or(&p).display().to_string();
        if fix {
            std::fs::write(&p, text.replace(bad, "-")).map_err(|e| format!("{}: {}", rel, e))?;
            println!("fixed {}", rel);
        } else {
            for (n, line) in text.lines().enumerate().filter(|(_, l)| l.contains(bad)) {
                println!("{}:{}: {}", rel, n + 1, line.trim());
            }
        }
    }
    if hits > 0 && !fix {
        eprintln!(
            "{} file(s) with dashes; `indra dev dashes --fix` replaces them with a hyphen",
            hits
        );
        return Ok(ExitCode::from(1));
    }
    if hits == 0 {
        println!("no dashes");
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_paths_and_entities() {
        assert_eq!(slugify("Memory: `indra mem`"), "memory-indra-mem");
        assert_eq!(
            slugify("Plan and delegate (built)"),
            "plan-and-delegate-built"
        );
        assert_eq!(normpath("docs/guides/../x.md"), "docs/x.md");
        assert_eq!(normpath("../a"), "../a");
        assert_eq!(
            unescape("a &amp; b &lt;c&gt; &#x27;d&#39;"),
            "a & b <c> 'd'"
        );
        let v: HashMap<&str, String> = [("a", "{b}".to_string()), ("b", "x".to_string())]
            .into_iter()
            .collect();
        assert_eq!(fill("{a}-{b}-{c}", &v), "{b}-x-{c}");
    }
}
