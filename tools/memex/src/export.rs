//! Graph exports: JSON (for tools), DOT (Graphviz), Mermaid (renders in
//! GitHub Markdown) and a single self-contained interactive HTML file.

use crate::graph::Graph;
use crate::vault::Vault;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
pub struct GraphNode {
    pub id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub tags: Vec<String>,
    /// True for an unresolved link target that has no note yet.
    pub ghost: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

#[derive(Serialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub rel: String,
}

#[derive(Serialize)]
pub struct GraphData {
    pub vault: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

pub fn data(v: &Vault, g: &Graph, with_body: bool) -> GraphData {
    let mut nodes: Vec<GraphNode> = v
        .notes
        .iter()
        .map(|n| GraphNode {
            id: n.id.clone(),
            title: n.title.clone(),
            kind: n.kind.clone(),
            tags: n.tags.clone(),
            ghost: false,
            body: with_body.then(|| n.body.clone()),
            updated: n.updated.clone(),
        })
        .collect();
    let mut edges = Vec::new();
    for (i, outs) in g.out.iter().enumerate() {
        for (j, rel) in outs {
            edges.push(GraphEdge {
                source: v.notes[i].id.clone(),
                target: v.notes[*j].id.clone(),
                rel: rel.clone(),
            });
        }
    }
    let mut ghosts: BTreeMap<String, String> = BTreeMap::new();
    for d in &g.dangling {
        let slug = crate::note::slugify(&d.target);
        let key = format!(
            "?{}",
            if slug.is_empty() {
                d.target.to_lowercase()
            } else {
                slug
            }
        );
        ghosts
            .entry(key.clone())
            .or_insert_with(|| d.target.clone());
        edges.push(GraphEdge {
            source: v.notes[d.from].id.clone(),
            target: key,
            rel: d.rel.clone(),
        });
    }
    for (id, title) in ghosts {
        nodes.push(GraphNode {
            id,
            title,
            kind: None,
            tags: Vec::new(),
            ghost: true,
            body: None,
            updated: None,
        });
    }
    GraphData {
        vault: v.root.display().to_string(),
        nodes,
        edges,
    }
}

fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}

pub fn dot(d: &GraphData) -> String {
    let mut s =
        String::from("digraph memex {\n  rankdir=LR;\n  node [shape=box, style=rounded];\n");
    for n in &d.nodes {
        let style = if n.ghost {
            ", style=\"rounded,dashed\""
        } else {
            ""
        };
        s.push_str(&format!(
            "  \"{}\" [label=\"{}\"{}];\n",
            dot_escape(&n.id),
            dot_escape(&n.title),
            style
        ));
    }
    for e in &d.edges {
        s.push_str(&format!(
            "  \"{}\" -> \"{}\" [label=\"{}\"];\n",
            dot_escape(&e.source),
            dot_escape(&e.target),
            dot_escape(&e.rel)
        ));
    }
    s.push_str("}\n");
    s
}

pub fn mermaid(d: &GraphData) -> String {
    let ids: BTreeMap<&str, usize> = d
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    // Mermaid labels: keep it to characters that never break its parser.
    let label = |s: &str| -> String {
        s.chars()
            .map(|c| if "\"[](){}<>|#;`".contains(c) { ' ' } else { c })
            .collect::<String>()
            .trim()
            .to_string()
    };
    let mut s = String::from("graph LR\n");
    for (i, n) in d.nodes.iter().enumerate() {
        if n.ghost {
            s.push_str(&format!("  n{}([\"{}?\"])\n", i, label(&n.title)));
        } else {
            s.push_str(&format!("  n{}[\"{}\"]\n", i, label(&n.title)));
        }
    }
    for e in &d.edges {
        if let (Some(a), Some(b)) = (ids.get(e.source.as_str()), ids.get(e.target.as_str())) {
            s.push_str(&format!("  n{} -->|{}| n{}\n", a, label(&e.rel), b));
        }
    }
    s
}

const VIEWER: &str = include_str!("viewer.html");

/// A single HTML file with the graph embedded. Works offline, no CDN. The
/// JSON is made safe for a `<script>` element; note text is rendered with
/// `textContent` only, so a hostile note cannot inject markup.
pub fn html(d: &GraphData, title: &str) -> String {
    let json = serde_json::to_string(d).unwrap_or_else(|_| "{}".into());
    let json = json
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    let title = title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let (head, tail) = VIEWER.split_once("__MEMEX_DATA__").unwrap_or((VIEWER, ""));
    format!(
        "{}{}{}",
        head.replace("__MEMEX_TITLE__", &title),
        json,
        tail.replace("__MEMEX_TITLE__", &title)
    )
}
