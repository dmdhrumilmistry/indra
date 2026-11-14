//! The `indra mem` commands: add, link, recall, tree, export and friends.

use crate::mem::{export, frontmatter, graph, note, search, time, tree, vault};

use clap::{Args, Subcommand, ValueEnum};
use graph::{Dir, Graph, Step};
use note::{normalize_rel, slugify, Note, DEFAULT_REL};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use vault::{Lock, Result, Vault};

/// `indra mem`: the memory commands.
#[derive(Args)]
pub struct MemArgs {
    /// Vault directory (overrides $INDRA_VAULT and discovery)
    #[arg(long, global = true, value_name = "DIR")]
    vault: Option<PathBuf>,
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a vault (a folder with a `.indra/` marker)
    Init {
        /// Directory to initialise (default: the resolved vault)
        dir: Option<PathBuf>,
    },
    /// Print the resolved vault path
    Where,
    /// Add a note
    Add {
        title: String,
        /// Body text, or `-` to read stdin
        #[arg(short, long, allow_hyphen_values = true)]
        body: Option<String>,
        /// Note type, e.g. learning, pattern, finding, technique, tool, decision
        #[arg(short = 't', long = "type")]
        kind: Option<String>,
        /// Tag (repeatable)
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
        /// Alternative name the note can be linked by (repeatable)
        #[arg(long = "alias")]
        aliases: Vec<String>,
        /// Untyped link to another note (repeatable)
        #[arg(short, long = "link", value_name = "NOTE")]
        links: Vec<String>,
        /// Typed relation REL=NOTE, e.g. mitigated-by=parameterized-queries (repeatable)
        #[arg(short, long = "rel", value_name = "REL=NOTE")]
        rels: Vec<String>,
        /// Where this came from: a run id, URL, file, ticket...
        #[arg(long)]
        source: Option<String>,
        /// Subfolder for the new note
        #[arg(long)]
        folder: Option<String>,
        /// If a note with this title exists, merge into it instead of failing
        #[arg(short, long)]
        upsert: bool,
    },
    /// Append text to a note (before its Relations section)
    Append {
        note: String,
        /// Text to append, or `-` to read stdin
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true)]
        text: Vec<String>,
    },
    /// Add a typed relation FROM -> TO
    Link {
        from: String,
        to: String,
        /// Relation type (default: related)
        #[arg(short, long, default_value = DEFAULT_REL)]
        rel: String,
    },
    /// Remove relation lines FROM -> TO
    Unlink {
        from: String,
        to: String,
        /// Only this relation type (default: any)
        #[arg(short, long)]
        rel: Option<String>,
    },
    /// Add (or with --remove, remove) frontmatter tags
    Tag {
        note: String,
        #[arg(required = true, num_args = 1..)]
        tags: Vec<String>,
        #[arg(long)]
        remove: bool,
    },
    /// Show a note with its links and backlinks
    Show { note: String },
    /// List notes
    List {
        #[arg(short = 't', long = "type")]
        kind: Option<String>,
        #[arg(long)]
        tag: Option<String>,
    },
    /// Ranked full-text search
    Search {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        #[arg(short = 't', long = "type")]
        kind: Option<String>,
        #[arg(long)]
        tag: Option<String>,
    },
    /// Graph-aware recall for agents: search, then spread through the graph
    Recall {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// Notes to return
        #[arg(short = 'n', long, default_value_t = 8)]
        limit: usize,
        /// Hops to expand from each search hit
        #[arg(short, long, default_value_t = 1)]
        depth: usize,
        /// Total character budget for note text in the output
        #[arg(long, default_value_t = 6000)]
        max_chars: usize,
        #[arg(short = 't', long = "type")]
        kind: Option<String>,
        #[arg(long)]
        tag: Option<String>,
    },
    /// Notes within N hops of a note
    #[command(alias = "neighbours")]
    Neighbors {
        note: String,
        #[arg(short, long, default_value_t = 1)]
        depth: usize,
        /// Follow only this relation type
        #[arg(short, long)]
        rel: Option<String>,
    },
    /// Shortest path between two notes
    Path { from: String, to: String },
    /// Draw the graph as a text tree in the terminal (one note, or the whole vault)
    Tree {
        /// Start here; omit to draw every connected group in the vault
        note: Option<String>,
        /// Hops to expand (default: 2 from a note, unlimited for the whole vault)
        #[arg(short, long)]
        depth: Option<usize>,
        /// Follow only this relation type
        #[arg(short, long)]
        rel: Option<String>,
    },
    /// Tags with note counts
    Tags,
    /// Vault statistics and hub notes
    Stats,
    /// Lint: dangling links, ambiguous names, orphans (exit 1 on errors)
    Check {
        /// Also fail on orphan notes
        #[arg(long)]
        strict: bool,
    },
    /// Export the graph
    Export {
        #[arg(short, long, value_enum, default_value_t = Format::Html)]
        format: Format,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Leave note bodies out of json/html
        #[arg(long)]
        no_body: bool,
    },
    /// Import notes from Obsidian, a Notion export, a GitHub wiki or Markdown
    Import {
        /// Folder to import (unzip a Notion export first)
        source: PathBuf,
        /// obsidian | notion | github-wiki | markdown (default: detected)
        #[arg(long)]
        from: Option<String>,
        /// Put the imported notes in this subfolder of the vault
        #[arg(long)]
        folder: Option<String>,
        /// Tag every imported note (repeatable)
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Replace notes that already exist (default: skip them)
        #[arg(long)]
        overwrite: bool,
        /// Show what would be imported without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete a note (reports the links it leaves dangling)
    Rm { note: String },
    /// Record a finished security review run (its verified.jsonl) as notes:
    /// findings, false positives as learnings, class and file hubs
    RecordRun {
        /// <TARGET>/.security-harness/<run-id>; the vault defaults to its
        /// sibling `memory` folder (or --vault)
        run_dir: PathBuf,
        /// Print the indra mem commands instead of writing
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Json,
    Dot,
    Mermaid,
    Html,
}

fn print_json<T: Serialize>(v: &T) -> Result<()> {
    let s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    println!("{}", s);
    Ok(())
}

fn read_text(parts: &[String]) -> Result<String> {
    if parts.len() == 1 && parts[0] == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| format!("cannot read stdin: {}", e))?;
        return Ok(s.trim_end().to_string());
    }
    Ok(parts.join(" "))
}

fn parse_rel(s: &str) -> Result<String> {
    normalize_rel(s).ok_or_else(|| {
        format!(
            "invalid relation '{}': use letters, digits, '-' or '_', starting with a letter",
            s
        )
    })
}

fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags.iter().flat_map(|t| t.split(',')) {
        let t = t.trim().trim_start_matches('#').replace(' ', "-");
        if !t.is_empty() && !out.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
            out.push(t);
        }
    }
    out
}

/// How a relation target is written: the note's id when it exists, else the
/// name as typed (an unresolved link, like Obsidian).
fn link_target(v: &Vault, name: &str) -> String {
    match v.resolve(name) {
        Some(i) => v.notes[i].id.clone(),
        None => {
            let slug = slugify(name);
            if slug.is_empty() {
                name.trim().to_string()
            } else {
                slug
            }
        }
    }
}

/// The `[[...]]` text for a relation. An unresolved target is written as
/// `[[its-slug|As Typed]]`: the slug is the file name `indra mem add` will give
/// that note later, so the link resolves in Obsidian too (which matches file
/// names, not titles), while the label keeps the name readable.
fn rel_line(v: &Vault, rel: &str, name: &str) -> String {
    let target = link_target(v, name);
    let name = name.trim();
    if v.resolve(name).is_none() && target != name {
        format!(
            "- {}:: [[{}|{}]]",
            rel,
            target,
            name.replace(['|', '[', ']'], " ")
        )
    } else {
        format!("- {}:: [[{}]]", rel, target)
    }
}

/// Two names for a note that does not exist yet refer to the same note.
fn same_unresolved(a: &str, b: &str) -> bool {
    let (sa, sb) = (slugify(a), slugify(b));
    a.trim().eq_ignore_ascii_case(b.trim()) || (!sa.is_empty() && sa == sb)
}

fn note_matches(n: &Note, kind: &Option<String>, tag: &Option<String>) -> bool {
    let k_ok = kind
        .as_ref()
        .map(|k| {
            n.kind
                .as_deref()
                .map(|x| x.eq_ignore_ascii_case(k))
                .unwrap_or(false)
        })
        .unwrap_or(true);
    let t_ok = tag
        .as_ref()
        .map(|t| {
            let t = t.trim_start_matches('#');
            n.tags.iter().any(|x| {
                x.eq_ignore_ascii_case(t)
                    || x.to_lowercase()
                        .starts_with(&format!("{}/", t.to_lowercase()))
            })
        })
        .unwrap_or(true);
    k_ok && t_ok
}

// ---- document editing helpers (operate on `\n`-normalised text) ----------

const REL_HEADING: &str = "## Relations";

fn rel_heading_index(lines: &[String]) -> Option<usize> {
    let mut fence = false;
    let mut found = None;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        } else if !fence && t.eq_ignore_ascii_case(REL_HEADING) {
            found = Some(i);
        }
    }
    found
}

fn trim_trailing_blank(lines: &mut Vec<String>) {
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
}

fn join_doc(lines: Vec<String>) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

/// Add relation lines to the Relations section, creating it at the end.
fn add_relation_lines(doc: &str, new: &[String]) -> String {
    if new.is_empty() {
        return doc.to_string();
    }
    let mut lines: Vec<String> = doc.lines().map(String::from).collect();
    match rel_heading_index(&lines) {
        Some(h) => {
            let end = (h + 1..lines.len())
                .find(|&i| lines[i].trim_start().starts_with('#'))
                .unwrap_or(lines.len());
            let at = (h + 1..end)
                .rev()
                .find(|&i| !lines[i].trim().is_empty())
                .map(|i| i + 1)
                .unwrap_or(h + 1);
            for (k, l) in new.iter().enumerate() {
                lines.insert(at + k, l.clone());
            }
        }
        None => {
            trim_trailing_blank(&mut lines);
            lines.push(String::new());
            lines.push(REL_HEADING.to_string());
            lines.extend(new.iter().cloned());
        }
    }
    join_doc(lines)
}

/// Append a paragraph before the Relations section (or at the end).
fn append_text(doc: &str, text: &str) -> String {
    let mut lines: Vec<String> = doc.lines().map(String::from).collect();
    let tail = match rel_heading_index(&lines) {
        Some(h) => lines.split_off(h),
        None => Vec::new(),
    };
    trim_trailing_blank(&mut lines);
    // Keep the frontmatter's closing fence directly above the body.
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend(text.trim_end().lines().map(String::from));
    if !tail.is_empty() {
        lines.push(String::new());
        lines.extend(tail);
    }
    join_doc(lines)
}

fn touch(doc: &str) -> String {
    frontmatter::set_field(doc, "updated", &time::now_iso())
}

fn render_new(
    title: &str,
    kind: Option<&str>,
    tags: &[String],
    aliases: &[String],
    source: Option<&str>,
    body: &str,
    rel_lines: &[String],
) -> String {
    let now = time::now_iso();
    let mut s = String::from("---\n");
    s.push_str(&format!("title: {}\n", frontmatter::quote(title)));
    if let Some(k) = kind {
        s.push_str(&format!("type: {}\n", frontmatter::quote(k)));
    }
    if !tags.is_empty() {
        s.push_str(&format!("tags: {}\n", frontmatter::render_list(tags)));
    }
    if !aliases.is_empty() {
        s.push_str(&format!("aliases: {}\n", frontmatter::render_list(aliases)));
    }
    if let Some(src) = source {
        s.push_str(&format!("source: {}\n", frontmatter::quote(src)));
    }
    s.push_str(&format!(
        "created: {}\nupdated: {}\n---\n\n# {}\n",
        now, now, title
    ));
    let body = body.trim();
    if !body.is_empty() {
        s.push('\n');
        s.push_str(body);
        s.push('\n');
    }
    if !rel_lines.is_empty() {
        s.push('\n');
        s.push_str(REL_HEADING);
        s.push('\n');
        for l in rel_lines {
            s.push_str(l);
            s.push('\n');
        }
    }
    s
}

// ---- output shapes -------------------------------------------------------

#[derive(Serialize)]
pub struct NoteRef {
    pub id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

fn note_ref(v: &Vault, i: usize) -> NoteRef {
    let n = &v.notes[i];
    NoteRef {
        id: n.id.clone(),
        title: n.title.clone(),
        kind: n.kind.clone(),
    }
}

#[derive(Serialize)]
pub struct Rel {
    pub rel: String,
    pub dir: &'static str,
    pub note: NoteRef,
}

fn rels_of(v: &Vault, g: &Graph, i: usize) -> Vec<Rel> {
    g.neighbours(i)
        .map(|(j, r, d)| Rel {
            rel: r.to_string(),
            dir: if d == Dir::Out { "out" } else { "in" },
            note: note_ref(v, j),
        })
        .collect()
}

fn describe_step(v: &Vault, s: &Step) -> String {
    match s.dir {
        Dir::Out => format!("{} -[{}]->", v.notes[s.from].title, s.rel),
        Dir::In => format!("<-[{}]- {}", s.rel, v.notes[s.from].title),
    }
}

/// Body without the leading `# Title` line (it repeats the title).
fn body_excerpt(n: &Note, max: usize) -> String {
    let mut lines = n
        .body
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .peekable();
    if let Some(first) = lines.peek() {
        if first.trim().trim_start_matches('#').trim() == n.title
            && first.trim_start().starts_with("# ")
        {
            lines.next();
        }
    }
    // The Relations section is shown separately, with resolved titles.
    let mut text: Vec<&str> = Vec::new();
    let mut in_rel = false;
    for l in lines {
        let t = l.trim();
        if t.starts_with('#') {
            in_rel = t.eq_ignore_ascii_case(REL_HEADING);
        }
        if !in_rel {
            text.push(l);
        }
    }
    search::truncate(text.join("\n").trim(), max)
}

/// `-> mitigated-by Parameterized queries; <- instance-of Second-order SQLi`
fn links_summary(v: &Vault, g: &Graph, i: usize, max: usize) -> String {
    let all: Vec<String> = g
        .neighbours(i)
        .map(|(j, r, d)| format!("{} {} {}", d.arrow(), r, v.notes[j].title))
        .collect();
    let more = all.len().saturating_sub(max);
    let mut s = all.into_iter().take(max).collect::<Vec<_>>().join("; ");
    if more > 0 {
        s.push_str(&format!("; +{} more", more));
    }
    s
}

/// A note to write: what `indra mem add` takes.
#[derive(Default)]
pub struct NewNote {
    pub title: String,
    pub body: String,
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    /// Untyped links (`related`).
    pub links: Vec<String>,
    /// Typed relations (REL, NOTE).
    pub rels: Vec<(String, String)>,
    pub source: Option<String>,
    pub folder: Option<String>,
    /// Merge into an existing note with this title instead of failing.
    pub upsert: bool,
}

#[derive(Serialize)]
pub struct Written {
    pub id: String,
    pub path: String,
    pub created: bool,
    /// Link targets that are not notes (yet).
    pub unresolved: Vec<String>,
}

/// Create a note (or with `upsert`, merge into the one with that title).
pub fn add_note(root: &std::path::Path, n: &NewNote) -> Result<Written> {
    let NewNote {
        title,
        body,
        kind,
        tags,
        aliases,
        links,
        rels,
        source,
        folder,
        upsert,
    } = n;
    let upsert = *upsert;
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("title must not be empty".into());
    }
    let tags = clean_tags(tags);
    let mut wanted: Vec<(String, String)> = links
        .iter()
        .map(|l| (DEFAULT_REL.to_string(), l.trim().to_string()))
        .collect();
    for (rel, target) in rels {
        wanted.push((parse_rel(rel)?, target.trim().to_string()));
    }
    if wanted.iter().any(|(_, t)| t.is_empty()) {
        return Err("link target must not be empty".into());
    }

    let _lock = Lock::acquire(root)?;
    let v = Vault::open(root)?;
    let mut lines = Vec::new();
    let mut unresolved = Vec::new();
    for (rel, target) in &wanted {
        if v.resolve(target).is_none() {
            unresolved.push(target.clone());
        }
        let l = rel_line(&v, rel, target);
        if !lines.contains(&l) {
            lines.push(l);
        }
    }

    let existing = v.resolve(&title);
    let (id, path, created) = match existing {
        Some(i) if upsert => {
            let n = &v.notes[i];
            let (mut doc, crlf) = vault::read_for_edit(&n.path)?;
            // Idempotent: the same text upserted twice is stored once.
            if !body.trim().is_empty() && !doc.contains(body.trim()) {
                doc = append_text(&doc, body);
            }
            let have: HashSet<(String, Option<usize>)> = n
                .links
                .iter()
                .map(|l| (l.rel.clone(), v.resolve(&l.target)))
                .collect();
            let new_lines: Vec<String> = wanted
                .iter()
                .filter(|(rel, t)| {
                    v.resolve(t)
                        .map(|j| !have.contains(&(rel.clone(), Some(j))))
                        .unwrap_or(true)
                })
                .map(|(rel, t)| rel_line(&v, rel, t))
                .filter(|l| !doc.contains(l.as_str()))
                .collect();
            doc = add_relation_lines(&doc, &new_lines);
            if !tags.is_empty() {
                let fm = frontmatter::split(&doc)
                    .0
                    .map(frontmatter::parse)
                    .unwrap_or_default();
                let mut all = fm.get_list("tags");
                all.extend(tags.iter().cloned());
                doc = frontmatter::set_field(
                    &doc,
                    "tags",
                    &frontmatter::render_list(&clean_tags(&all)),
                );
            }
            if let Some(k) = &kind {
                if n.kind.is_none() {
                    doc = frontmatter::set_field(&doc, "type", &frontmatter::quote(k));
                }
            }
            doc = touch(&doc);
            vault::finish_edit(&n.path, &doc, crlf)?;
            (n.id.clone(), n.path.clone(), false)
        }
        Some(i) => {
            return Err(format!(
            "a note named '{}' already exists ({}). Use --upsert to merge, or `indra mem append`",
            title, v.notes[i].id
        ))
        }
        None => {
            let id = v.fresh_id(&title, folder.as_deref());
            let path = v.path_for_id(&id);
            let doc = render_new(
                &title,
                kind.as_deref(),
                &tags,
                aliases,
                source.as_deref(),
                body,
                &lines,
            );
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            // create_new: never clobber a file that appeared meanwhile.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("cannot create {}: {}", path.display(), e))?;
            std::io::Write::write_all(&mut f, doc.as_bytes()).map_err(|e| e.to_string())?;
            (id, path, true)
        }
    };
    Ok(Written {
        id,
        path: path.display().to_string(),
        created,
        unresolved,
    })
}

/// How much `recall` returns.
pub struct RecallOpts {
    pub limit: usize,
    pub depth: usize,
    pub max_chars: usize,
    pub kind: Option<String>,
    pub tag: Option<String>,
}

impl Default for RecallOpts {
    fn default() -> Self {
        RecallOpts {
            limit: 8,
            depth: 1,
            max_chars: 6000,
            kind: None,
            tag: None,
        }
    }
}

#[derive(Serialize)]
pub struct Recalled {
    pub id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub score: f64,
    /// How the graph reached it from a search hit (None: a hit itself).
    pub via: Option<String>,
    pub text: String,
    pub relations: Vec<Rel>,
    #[serde(skip)]
    pub links: String,
}

#[derive(Serialize)]
pub struct Recall {
    pub query: String,
    pub vault: String,
    pub results: Vec<Recalled>,
}

/// Graph-aware recall: rank by search, then spread through the graph, within
/// a character budget. What `indra mem recall` prints and agents are given.
pub fn recall(root: &std::path::Path, q: &str, o: &RecallOpts) -> Result<Recall> {
    let limit = o.limit.max(1);
    let v = Vault::open(root)?;
    let g = Graph::build(&v);
    let idx = search::Index::build(&v.notes);
    let hits = idx.search(&v.notes, q, |n| note_matches(n, &o.kind, &o.tag));
    let top = hits.first().map(|h| h.1).unwrap_or(1.0).max(1e-9);
    let seeds: Vec<(usize, f64)> = hits
        .iter()
        .take(limit * 2)
        .map(|&(i, s)| (i, s / top))
        .collect();
    let act = g.activate(&seeds, o.depth, 0.5);
    let mut ranked: Vec<(usize, f64, Option<Step>)> =
        act.into_iter().map(|(i, (s, st))| (i, s, st)).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(limit);
    let per = (o.max_chars / limit).max(240);
    let mut budget = o.max_chars;
    let results = ranked
        .iter()
        .map(|(i, s, st)| {
            let n = &v.notes[*i];
            let text = if budget > 0 {
                body_excerpt(n, per.min(budget))
            } else {
                String::new()
            };
            budget = budget.saturating_sub(text.chars().count());
            Recalled {
                id: n.id.clone(),
                title: n.title.clone(),
                kind: n.kind.clone(),
                tags: n.tags.clone(),
                score: (s * 1000.0).round() / 1000.0,
                via: st.as_ref().map(|s| describe_step(&v, s)),
                text,
                relations: rels_of(&v, &g, *i),
                links: links_summary(&v, &g, *i, 8),
            }
        })
        .collect();
    Ok(Recall {
        query: q.to_string(),
        vault: v.root.display().to_string(),
        results,
    })
}

/// A recall as Markdown, for a person or an agent's prompt.
pub fn render_recall(r: &Recall) -> String {
    use std::fmt::Write;
    let mut o = String::new();
    if r.results.is_empty() {
        let _ = writeln!(
            o,
            "# Memory recall: {}\n\nNothing relevant in {}.",
            r.query, r.vault
        );
        return o;
    }
    let _ = writeln!(o, "# Memory recall: {}\n", r.query);
    for n in &r.results {
        let _ = writeln!(o, "{}", recalled_block(n));
    }
    o
}

/// One recalled note as Markdown (heading, metadata, text, links).
pub fn recalled_block(n: &Recalled) -> String {
    let mut head = format!("## {}\n_id: {}", n.title, n.id);
    if let Some(k) = &n.kind {
        head.push_str(&format!(" | type: {}", k));
    }
    if !n.tags.is_empty() {
        head.push_str(&format!(" | tags: {}", n.tags.join(", ")));
    }
    head.push_str(&format!(" | score: {:.2}", n.score));
    match &n.via {
        Some(v) => head.push_str(&format!(" | via: {}", v)),
        None => head.push_str(" | via: search"),
    }
    head.push('_');
    let mut o = format!("{}\n", head);
    if !n.text.is_empty() {
        o.push_str(&format!("\n{}\n", n.text));
    }
    if !n.links.is_empty() {
        o.push_str(&format!("\nlinks: {}\n", n.links));
    }
    o
}

/// Append text to a note (before its Relations section).
pub fn append_to(root: &std::path::Path, note: &str, text: &str) -> Result<String> {
    if text.trim().is_empty() {
        return Err("nothing to append".into());
    }
    let _lock = Lock::acquire(root)?;
    let v = Vault::open(root)?;
    let n = &v.notes[v.require(note)?];
    let (doc, crlf) = vault::read_for_edit(&n.path)?;
    vault::finish_edit(&n.path, &touch(&append_text(&doc, text)), crlf)?;
    Ok(n.id.clone())
}

/// Add a typed relation FROM -> TO. False when it was already there.
pub fn link_notes(root: &std::path::Path, from: &str, to: &str, rel: &str) -> Result<bool> {
    let rel = parse_rel(rel)?;
    let _lock = Lock::acquire(root)?;
    let v = Vault::open(root)?;
    let i = v.require(from)?;
    let n = &v.notes[i];
    let target = v.resolve(to);
    if target == Some(i) {
        return Err("a note cannot link to itself".into());
    }
    let exists = n.links.iter().any(|l| {
        l.rel == rel
            && match target {
                Some(j) => v.resolve(&l.target) == Some(j),
                None => same_unresolved(&l.target, to),
            }
    });
    if exists {
        return Ok(false);
    }
    let (doc, crlf) = vault::read_for_edit(&n.path)?;
    let doc = add_relation_lines(&doc, &[rel_line(&v, &rel, to)]);
    vault::finish_edit(&n.path, &touch(&doc), crlf)?;
    Ok(true)
}

/// Delete a note; returns its id and the notes whose links to it now dangle.
pub fn remove_note(root: &std::path::Path, note: &str) -> Result<(String, Vec<String>)> {
    let _lock = Lock::acquire(root)?;
    let v = Vault::open(root)?;
    let g = Graph::build(&v);
    let i = v.require(note)?;
    let n = &v.notes[i];
    std::fs::remove_file(&n.path)
        .map_err(|e| format!("cannot delete {}: {}", n.path.display(), e))?;
    let back = g.inc[i]
        .iter()
        .map(|(j, _)| v.notes[*j].id.clone())
        .collect();
    Ok((n.id.clone(), back))
}

/// Notes as the console lists them.
pub fn note_summary(v: &Vault, g: &Graph, i: usize) -> serde_json::Value {
    let n = &v.notes[i];
    serde_json::json!({
        "id": n.id, "title": n.title, "type": n.kind, "tags": n.tags, "updated": n.updated,
        "created": n.created, "source": n.source, "links": g.degree(i),
    })
}

/// One note in full, for the console.
pub fn note_detail(v: &Vault, g: &Graph, i: usize) -> serde_json::Value {
    let n = &v.notes[i];
    serde_json::json!({
        "id": n.id, "title": n.title, "type": n.kind, "tags": n.tags, "aliases": n.aliases,
        "source": n.source, "created": n.created, "updated": n.updated,
        "path": n.path.display().to_string(), "body": n.body, "relations": rels_of(v, g, i),
        "unresolved": g.dangling.iter().filter(|d| d.from == i).map(|d| &d.target).collect::<Vec<_>>(),
    })
}

// ---- commands -----------------------------------------------------------

pub fn run(cli: MemArgs) -> Result<ExitCode> {
    if let Cmd::RecordRun { run_dir, dry_run } = &cli.cmd {
        let run_dir = std::path::absolute(run_dir).map_err(|e| e.to_string())?;
        let vault = match &cli.vault {
            Some(v) => v.clone(),
            None => run_dir
                .parent()
                .map(|p| p.join("memory"))
                .ok_or("the run directory has no parent")?,
        };
        let out = crate::mem::record::record(&run_dir, &vault, *dry_run)
            .map_err(|e| format!("record-run: {}", e))?;
        println!("{}", out);
        return Ok(ExitCode::SUCCESS);
    }
    let root = vault::locate(cli.vault.clone());
    let json = cli.json;
    match cli.cmd {
        Cmd::RecordRun { .. } => unreachable!(),
        Cmd::Init { dir } => {
            let dir = dir.unwrap_or(root);
            let created = vault::init(&dir)?;
            if json {
                print_json(
                    &serde_json::json!({ "vault": dir.display().to_string(), "created": created }),
                )?;
            } else if created {
                println!("initialised vault at {}", dir.display());
            } else {
                println!("vault already exists at {}", dir.display());
            }
        }
        Cmd::Where => {
            if json {
                print_json(
                    &serde_json::json!({ "vault": root.display().to_string(), "exists": root.join(vault::MARKER).is_dir() }),
                )?;
            } else {
                println!("{}", root.display());
            }
        }
        Cmd::Add {
            title,
            body,
            kind,
            tags,
            aliases,
            links,
            rels,
            source,
            folder,
            upsert,
        } => {
            let body = match body {
                Some(b) => read_text(&[b])?,
                None => String::new(),
            };
            let mut relations = Vec::new();
            for r in &rels {
                let (rel, target) = r
                    .split_once('=')
                    .ok_or_else(|| format!("--rel expects REL=NOTE, got '{}'", r))?;
                relations.push((rel.to_string(), target.trim().to_string()));
            }
            let out = add_note(
                &root,
                &NewNote {
                    title,
                    body,
                    kind,
                    tags,
                    aliases,
                    links,
                    rels: relations,
                    source,
                    folder,
                    upsert,
                },
            )?;
            for t in &out.unresolved {
                eprintln!(
                    "indra: note: '{}' does not exist yet; kept as an unresolved link",
                    t
                );
            }
            if json {
                print_json(&out)?;
            } else {
                println!(
                    "{} {} ({})",
                    if out.created { "created" } else { "updated" },
                    out.id,
                    out.path
                );
            }
        }
        Cmd::Append { note, text } => {
            let text = read_text(&text)?;
            if text.trim().is_empty() {
                return Err("nothing to append".into());
            }
            let _lock = Lock::acquire(&root)?;
            let v = Vault::open(&root)?;
            let n = &v.notes[v.require(&note)?];
            let (doc, crlf) = vault::read_for_edit(&n.path)?;
            vault::finish_edit(&n.path, &touch(&append_text(&doc, &text)), crlf)?;
            report_write(json, "updated", &n.id)?;
        }
        Cmd::Link { from, to, rel } => {
            let changed = link_notes(&root, &from, &to, &rel)?;
            let v = Vault::open(&root)?;
            let id = v.notes[v.require(&from)?].id.clone();
            let rel = parse_rel(&rel)?;
            if changed && v.resolve(&to).is_none() {
                eprintln!(
                    "indra: note: '{}' does not exist yet; kept as an unresolved link",
                    to.trim()
                );
            }
            if json {
                print_json(&serde_json::json!({ "id": id, "changed": changed }))?;
            } else if changed {
                println!("linked {} -[{}]-> {}", id, rel, link_target(&v, &to));
            } else {
                println!(
                    "already linked: {} -[{}]-> {}",
                    id,
                    rel,
                    link_target(&v, &to)
                );
            }
        }
        Cmd::Unlink { from, to, rel } => {
            let rel = rel.map(|r| parse_rel(&r)).transpose()?;
            let _lock = Lock::acquire(&root)?;
            let v = Vault::open(&root)?;
            let i = v.require(&from)?;
            let n = &v.notes[i];
            let target = v.resolve(&to);
            let matches_target = |t: &str| match target {
                Some(j) => v.resolve(t) == Some(j),
                None => same_unresolved(t, &to),
            };
            let (doc, crlf) = vault::read_for_edit(&n.path)?;
            let (fm_part, body) = match frontmatter::split(&doc) {
                (Some(_), b) => (&doc[..doc.len() - b.len()], b),
                (None, b) => ("", b),
            };
            let mut removed = 0;
            let mut kept = Vec::new();
            for line in body.lines() {
                if let Some((r, value)) = note::inline_field(line) {
                    let targets = note::wikilinks(value);
                    let rel_ok = rel.as_ref().map(|x| *x == r).unwrap_or(true);
                    if rel_ok && !targets.is_empty() && targets.iter().all(|t| matches_target(t)) {
                        removed += 1;
                        continue;
                    }
                }
                kept.push(line.to_string());
            }
            if removed == 0 {
                return Err(format!(
                    "no relation line from '{}' to '{}' found. Links written in prose must be edited by hand: {}",
                    n.id,
                    to,
                    n.path.display()
                ));
            }
            let doc = format!("{}{}", fm_part, join_doc(kept));
            vault::finish_edit(&n.path, &touch(&doc), crlf)?;
            if json {
                print_json(&serde_json::json!({ "id": n.id, "removed": removed }))?;
            } else {
                println!("removed {} relation line(s) from {}", removed, n.id);
            }
        }
        Cmd::Tag { note, tags, remove } => {
            let tags = clean_tags(&tags);
            let _lock = Lock::acquire(&root)?;
            let v = Vault::open(&root)?;
            let n = &v.notes[v.require(&note)?];
            let (doc, crlf) = vault::read_for_edit(&n.path)?;
            let fm = frontmatter::split(&doc)
                .0
                .map(frontmatter::parse)
                .unwrap_or_default();
            let mut all = clean_tags(&fm.get_list("tags"));
            if remove {
                all.retain(|t| !tags.iter().any(|x| x.eq_ignore_ascii_case(t)));
            } else {
                all.extend(tags);
                all = clean_tags(&all);
            }
            let doc = frontmatter::set_field(&doc, "tags", &frontmatter::render_list(&all));
            vault::finish_edit(&n.path, &touch(&doc), crlf)?;
            if json {
                print_json(&serde_json::json!({ "id": n.id, "tags": all }))?;
            } else {
                println!(
                    "{}: {}",
                    n.id,
                    all.iter()
                        .map(|t| format!("#{}", t))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        }
        Cmd::Show { note } => {
            let v = Vault::open(&root)?;
            let g = Graph::build(&v);
            let i = v.require(&note)?;
            let n = &v.notes[i];
            if json {
                print_json(&serde_json::json!({
                    "id": n.id, "title": n.title, "type": n.kind, "tags": n.tags, "aliases": n.aliases,
                    "source": n.source, "created": n.created, "updated": n.updated,
                    "path": n.path.display().to_string(), "body": n.body, "relations": rels_of(&v, &g, i),
                    "unresolved": g.dangling.iter().filter(|d| d.from == i).map(|d| &d.target).collect::<Vec<_>>(),
                }))?;
            } else {
                println!("{}", n.title);
                let mut meta = vec![format!("id: {}", n.id)];
                if let Some(k) = &n.kind {
                    meta.push(format!("type: {}", k));
                }
                if !n.tags.is_empty() {
                    meta.push(format!(
                        "tags: {}",
                        n.tags
                            .iter()
                            .map(|t| format!("#{}", t))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                }
                if let Some(u) = &n.updated {
                    meta.push(format!("updated: {}", u));
                }
                println!("{}", meta.join("  |  "));
                println!("{}\n", n.path.display());
                println!("{}", n.body.trim_end());
                println!();
                print_rel_block(
                    "Links out",
                    g.out[i].iter().map(|(j, r)| (r.as_str(), *j)),
                    &v,
                    "->",
                );
                print_rel_block(
                    "Backlinks",
                    g.inc[i].iter().map(|(j, r)| (r.as_str(), *j)),
                    &v,
                    "<-",
                );
                let dang: Vec<_> = g.dangling.iter().filter(|d| d.from == i).collect();
                if !dang.is_empty() {
                    println!("Unresolved:");
                    for d in dang {
                        println!("  -> {:<16} {} (no such note)", d.rel, d.target);
                    }
                }
            }
        }
        Cmd::List { kind, tag } => {
            let v = Vault::open(&root)?;
            let hits: Vec<usize> = (0..v.notes.len())
                .filter(|&i| note_matches(&v.notes[i], &kind, &tag))
                .collect();
            if json {
                let out: Vec<_> = hits.iter().map(|&i| {
                    let n = &v.notes[i];
                    serde_json::json!({ "id": n.id, "title": n.title, "type": n.kind, "tags": n.tags, "updated": n.updated })
                }).collect();
                print_json(&out)?;
            } else {
                for i in hits {
                    let n = &v.notes[i];
                    println!(
                        "{:<40} {:<12} {}",
                        n.id,
                        n.kind.as_deref().unwrap_or("-"),
                        n.title
                    );
                }
            }
        }
        Cmd::Search {
            query,
            limit,
            kind,
            tag,
        } => {
            let q = query.join(" ");
            let v = Vault::open(&root)?;
            let idx = search::Index::build(&v.notes);
            let hits = idx.search(&v.notes, &q, |n| note_matches(n, &kind, &tag));
            let hits = &hits[..hits.len().min(limit)];
            if json {
                let out: Vec<_> = hits.iter().map(|&(i, s)| {
                    let n = &v.notes[i];
                    serde_json::json!({ "id": n.id, "title": n.title, "type": n.kind, "tags": n.tags,
                        "score": (s * 1000.0).round() / 1000.0, "snippet": search::snippet(&n.body, &q, 200) })
                }).collect();
                print_json(&out)?;
            } else if hits.is_empty() {
                println!("no matches for '{}'", q);
            } else {
                for &(i, s) in hits {
                    let n = &v.notes[i];
                    println!("{:>6.2}  {}  ({})", s, n.title, n.id);
                    let snip = search::snippet(&n.body, &q, 140);
                    if !snip.is_empty() {
                        println!("        {}", snip);
                    }
                }
            }
        }
        Cmd::Recall {
            query,
            limit,
            depth,
            max_chars,
            kind,
            tag,
        } => {
            let r = recall(
                &root,
                &query.join(" "),
                &RecallOpts {
                    limit,
                    depth,
                    max_chars,
                    kind,
                    tag,
                },
            )?;
            if json {
                print_json(&r)?;
            } else {
                print!("{}", render_recall(&r));
            }
        }
        Cmd::Neighbors { note, depth, rel } => {
            let rel = rel.map(|r| parse_rel(&r)).transpose()?;
            let v = Vault::open(&root)?;
            let g = Graph::build(&v);
            let i = v.require(&note)?;
            let res = g.bfs(i, depth.max(1), rel.as_deref());
            if json {
                let out: Vec<_> = res.iter().map(|(j, d, st)| serde_json::json!({
                    "id": v.notes[*j].id, "title": v.notes[*j].title, "type": v.notes[*j].kind,
                    "hops": d, "via": describe_step(&v, st),
                })).collect();
                print_json(&out)?;
            } else if res.is_empty() {
                println!("{} has no neighbours", v.notes[i].id);
            } else {
                for (j, d, st) in res {
                    println!(
                        "{} {} {:<16} {}  ({})",
                        d,
                        st.dir.arrow(),
                        st.rel,
                        v.notes[j].title,
                        v.notes[j].id
                    );
                }
            }
        }
        Cmd::Tree { note, depth, rel } => {
            let rel = rel.map(|r| parse_rel(&r)).transpose()?;
            let v = Vault::open(&root)?;
            let g = Graph::build(&v);
            let roots: Vec<usize> = match &note {
                Some(n) => vec![v.require(n)?],
                None => {
                    // Biggest hubs first, so each group is drawn from its centre.
                    let mut all: Vec<usize> = (0..v.notes.len()).collect();
                    all.sort_by(|&a, &b| g.degree(b).cmp(&g.degree(a)).then(a.cmp(&b)));
                    all
                }
            };
            let depth = depth.unwrap_or(if note.is_some() { 2 } else { usize::MAX });
            let mut drawn: HashSet<usize> = HashSet::new();
            let mut trees = Vec::new();
            let mut unlinked = Vec::new();
            for r in roots {
                if drawn.contains(&r) {
                    continue;
                }
                if note.is_none() && g.degree(r) == 0 {
                    unlinked.push(r);
                    continue;
                }
                let t = tree::Tree::build(&g, r, depth, rel.as_deref());
                drawn.extend(t.nodes());
                trees.push(t);
            }
            if json {
                let out: Vec<_> = trees.iter().map(|t| t.to_json(&v, &g)).collect();
                print_json(&serde_json::json!({
                    "trees": out,
                    "unlinked": unlinked.iter().map(|&i| &v.notes[i].id).collect::<Vec<_>>(),
                }))?;
            } else {
                if trees.is_empty() && unlinked.is_empty() {
                    println!("(empty vault: {})", v.root.display());
                }
                for (k, t) in trees.iter().enumerate() {
                    if k > 0 {
                        println!();
                    }
                    print!("{}", t.render(&v, &g));
                }
                if !unlinked.is_empty() {
                    if !trees.is_empty() {
                        println!();
                    }
                    println!("Unlinked notes:");
                    for i in unlinked {
                        println!("  {}  ({})", v.notes[i].title, v.notes[i].id);
                    }
                }
            }
        }
        Cmd::Path { from, to } => {
            let v = Vault::open(&root)?;
            let g = Graph::build(&v);
            let (a, b) = (v.require(&from)?, v.require(&to)?);
            match g.path(a, b) {
                Some(steps) => {
                    if json {
                        let mut out = vec![
                            serde_json::json!({ "id": v.notes[a].id, "title": v.notes[a].title }),
                        ];
                        for s in &steps {
                            let next = s.to;
                            out.push(serde_json::json!({ "rel": s.rel, "dir": if s.dir == Dir::Out {"out"} else {"in"},
                                "id": v.notes[next].id, "title": v.notes[next].title }));
                        }
                        print_json(&out)?;
                    } else {
                        let mut line = v.notes[a].title.clone();
                        for s in &steps {
                            let next = s.to;
                            line.push_str(&match s.dir {
                                Dir::Out => format!("  -[{}]->  {}", s.rel, v.notes[next].title),
                                Dir::In => format!("  <-[{}]-  {}", s.rel, v.notes[next].title),
                            });
                        }
                        println!("{}", line);
                        println!(
                            "({} hop{})",
                            steps.len(),
                            if steps.len() == 1 { "" } else { "s" }
                        );
                    }
                }
                None => {
                    eprintln!(
                        "indra: no path between '{}' and '{}'",
                        v.notes[a].id, v.notes[b].id
                    );
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        Cmd::Tags => {
            let v = Vault::open(&root)?;
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for n in &v.notes {
                for t in &n.tags {
                    *counts.entry(t.to_lowercase()).or_insert(0) += 1;
                }
            }
            let mut list: Vec<_> = counts.into_iter().collect();
            list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            if json {
                print_json(
                    &list
                        .iter()
                        .map(|(t, c)| serde_json::json!({ "tag": t, "count": c }))
                        .collect::<Vec<_>>(),
                )?;
            } else {
                for (t, c) in list {
                    println!("{:>5}  #{}", c, t);
                }
            }
        }
        Cmd::Stats => {
            let v = Vault::open(&root)?;
            let g = Graph::build(&v);
            let mut types: BTreeMap<String, usize> = BTreeMap::new();
            let mut rels: BTreeMap<String, usize> = BTreeMap::new();
            for n in &v.notes {
                *types
                    .entry(n.kind.clone().unwrap_or_else(|| "(none)".into()))
                    .or_insert(0) += 1;
            }
            for outs in &g.out {
                for (_, r) in outs {
                    *rels.entry(r.clone()).or_insert(0) += 1;
                }
            }
            let orphans = (0..v.notes.len()).filter(|&i| g.degree(i) == 0).count();
            let mut hubs: Vec<usize> = (0..v.notes.len()).filter(|&i| g.degree(i) > 0).collect();
            hubs.sort_by(|&a, &b| g.degree(b).cmp(&g.degree(a)).then(a.cmp(&b)));
            hubs.truncate(5);
            if json {
                print_json(&serde_json::json!({
                    "vault": v.root.display().to_string(), "notes": v.notes.len(), "links": g.edge_count(),
                    "unresolved_links": g.dangling.len(), "orphans": orphans, "types": types, "relations": rels,
                    "hubs": hubs.iter().map(|&i| serde_json::json!({ "id": v.notes[i].id, "degree": g.degree(i) })).collect::<Vec<_>>(),
                }))?;
            } else {
                println!("vault       {}", v.root.display());
                println!("notes       {}", v.notes.len());
                println!("links       {}", g.edge_count());
                println!("unresolved  {}", g.dangling.len());
                println!("orphans     {}", orphans);
                if !types.is_empty() {
                    println!(
                        "types       {}",
                        types
                            .iter()
                            .map(|(k, c)| format!("{} {}", k, c))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                if !rels.is_empty() {
                    println!(
                        "relations   {}",
                        rels.iter()
                            .map(|(k, c)| format!("{} {}", k, c))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                if !hubs.is_empty() {
                    println!(
                        "hubs        {}",
                        hubs.iter()
                            .map(|&i| format!("{} ({})", v.notes[i].id, g.degree(i)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
        }
        Cmd::Check { strict } => {
            let v = Vault::open(&root)?;
            let g = Graph::build(&v);
            let orphans: Vec<usize> = (0..v.notes.len()).filter(|&i| g.degree(i) == 0).collect();
            let mut ambiguous: Vec<(&String, &Vec<usize>)> = v.ambiguous.iter().collect();
            ambiguous.sort();
            let errors =
                g.dangling.len() + ambiguous.len() + if strict { orphans.len() } else { 0 };
            if json {
                print_json(&serde_json::json!({
                    "ok": errors == 0,
                    "unresolved": g.dangling.iter().map(|d| serde_json::json!({
                        "from": v.notes[d.from].id, "target": d.target, "rel": d.rel, "line": d.line,
                        "path": v.notes[d.from].path.display().to_string() })).collect::<Vec<_>>(),
                    "ambiguous": ambiguous.iter().map(|(k, ids)| serde_json::json!({
                        "name": k, "notes": ids.iter().map(|&i| &v.notes[i].id).collect::<Vec<_>>() })).collect::<Vec<_>>(),
                    "orphans": orphans.iter().map(|&i| &v.notes[i].id).collect::<Vec<_>>(),
                }))?;
            } else {
                for d in &g.dangling {
                    let n = &v.notes[d.from];
                    let at = if d.line > 0 {
                        format!(":{}", d.line)
                    } else {
                        String::new()
                    };
                    println!(
                        "unresolved  {}{}  -[{}]-> {}",
                        n.path.display(),
                        at,
                        d.rel,
                        d.target
                    );
                }
                for (k, ids) in &ambiguous {
                    let names: Vec<&str> = ids.iter().map(|&i| v.notes[i].id.as_str()).collect();
                    println!(
                        "ambiguous   '{}' names {} notes: {}",
                        k,
                        ids.len(),
                        names.join(", ")
                    );
                }
                for &i in &orphans {
                    println!(
                        "{}  {}",
                        if strict { "orphan    " } else { "orphan (ok)" },
                        v.notes[i].id
                    );
                }
                println!(
                    "{}: {} notes, {} links, {} unresolved, {} ambiguous, {} orphans",
                    if errors == 0 { "ok" } else { "problems" },
                    v.notes.len(),
                    g.edge_count(),
                    g.dangling.len(),
                    ambiguous.len(),
                    orphans.len()
                );
            }
            if errors > 0 {
                return Ok(ExitCode::FAILURE);
            }
        }
        Cmd::Export {
            format,
            output,
            no_body,
        } => {
            let v = Vault::open(&root)?;
            let g = Graph::build(&v);
            let with_body = !no_body && matches!(format, Format::Json | Format::Html);
            let d = export::data(&v, &g, with_body);
            let name = v
                .root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "indra".into());
            let text = match format {
                Format::Json => serde_json::to_string_pretty(&d).map_err(|e| e.to_string())? + "\n",
                Format::Dot => export::dot(&d),
                Format::Mermaid => export::mermaid(&d),
                Format::Html => export::html(&d, &format!("indra - {}", name)),
            };
            match output {
                Some(p) => {
                    vault::write_atomic(&p, &text)?;
                    eprintln!(
                        "wrote {} ({} notes, {} links)",
                        p.display(),
                        v.notes.len(),
                        g.edge_count()
                    );
                }
                None => print!("{}", text),
            }
        }
        Cmd::Import {
            source,
            from,
            folder,
            tags,
            overwrite,
            dry_run,
        } => {
            let from = from
                .as_deref()
                .map(crate::mem::import::Source::parse)
                .transpose()?;
            let opts = crate::mem::import::Options {
                from,
                folder,
                tags,
                overwrite,
                dry_run,
            };
            let r = crate::mem::import::import(&root, &source, &opts)?;
            if json {
                print_json(&r)?;
            } else {
                let verb = if r.dry_run {
                    "would import"
                } else {
                    "imported"
                };
                println!(
                    "{} {} note(s) from {} into {}",
                    verb,
                    r.imported.len(),
                    serde_json::to_value(r.from)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default(),
                    root.display()
                );
                println!("  links converted   {}", r.links_converted);
                if !r.skipped_existing.is_empty() {
                    println!(
                        "  skipped existing  {} (use --overwrite to replace)",
                        r.skipped_existing.len()
                    );
                }
                if r.ignored_files > 0 {
                    println!(
                        "  ignored files     {} (attachments, CSV databases)",
                        r.ignored_files
                    );
                }
                if !r.unresolved.is_empty() {
                    println!("  unresolved links  {}", r.unresolved.len());
                    for (note, target) in r.unresolved.iter().take(10) {
                        println!("    {} -> {}", note, target);
                    }
                }
            }
        }
        Cmd::Rm { note } => {
            let (id, back) = remove_note(&root, &note)?;
            if json {
                print_json(&serde_json::json!({ "removed": id, "now_unresolved_in": back }))?;
            } else {
                println!("removed {}", id);
                if !back.is_empty() {
                    println!("links to it are now unresolved in: {}", back.join(", "));
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn print_rel_block<'a>(
    label: &str,
    items: impl Iterator<Item = (&'a str, usize)>,
    v: &Vault,
    arrow: &str,
) {
    let items: Vec<_> = items.collect();
    println!("{}:", label);
    if items.is_empty() {
        println!("  (none)");
    }
    for (r, j) in items {
        println!(
            "  {} {:<16} {}  ({})",
            arrow, r, v.notes[j].title, v.notes[j].id
        );
    }
}

fn report_write(json: bool, what: &str, id: &str) -> Result<()> {
    if json {
        print_json(&serde_json::json!({ "id": id, "changed": true }))
    } else {
        println!("{} {}", what, id);
        Ok(())
    }
}
