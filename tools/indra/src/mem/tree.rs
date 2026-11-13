//! A terminal view of the graph: a breadth-first spanning tree drawn with
//! ASCII connectors, so it renders the same in any console on any OS.
//!
//! ```text
//! SQL injection  (sql-injection)  [vuln-class]
//! |-- <- instance-of  Django extra() is a raw SQL sink
//! |   `-- -> mitigated-by  Parameterized queries
//! `-- <- refines  False positive: sanitize_sql helper
//! ```
//!
//! Each note is expanded once, at its shortest distance from the root. A link
//! back to a note already drawn elsewhere is shown as a leaf marked `(*)`, so
//! every link of an expanded note is visible without the drawing looping.

use crate::mem::graph::{Dir, Graph};
use crate::mem::vault::Vault;
use std::collections::{HashMap, VecDeque};

/// The step that discovered a note: (parent, rel, direction from parent).
type Parent = (usize, String, Dir);

pub struct Tree {
    root: usize,
    depth: usize,
    rel: Option<String>,
    dist: HashMap<usize, usize>,
    parent: HashMap<usize, Parent>,
}

struct Item {
    node: usize,
    rel: String,
    dir: Dir,
    /// Drawn elsewhere (or at the depth limit): shown, not expanded.
    leaf_ref: bool,
}

impl Tree {
    pub fn build(g: &Graph, root: usize, depth: usize, rel: Option<&str>) -> Tree {
        let mut dist = HashMap::from([(root, 0usize)]);
        let mut parent = HashMap::new();
        let mut q = VecDeque::from([root]);
        while let Some(u) = q.pop_front() {
            let d = dist[&u];
            if d >= depth {
                continue;
            }
            for (w, r, dir) in sorted_neighbours(g, u, rel) {
                if dist.contains_key(&w) {
                    continue;
                }
                dist.insert(w, d + 1);
                parent.insert(w, (u, r, dir));
                q.push_back(w);
            }
        }
        Tree {
            root,
            depth,
            rel: rel.map(String::from),
            dist,
            parent,
        }
    }

    pub fn nodes(&self) -> impl Iterator<Item = usize> + '_ {
        self.dist.keys().copied()
    }

    /// What to draw under `u`: its tree children (expanded) and its other
    /// links (as `(*)` references). The edge back to its own parent is skipped.
    fn items(&self, g: &Graph, u: usize) -> Vec<Item> {
        if self.dist[&u] >= self.depth {
            return Vec::new();
        }
        let up = self.parent.get(&u);
        let mut out = Vec::new();
        for (w, r, dir) in sorted_neighbours(g, u, self.rel.as_deref()) {
            if let Some((p, pr, pdir)) = up {
                if *p == w && *pr == r && *pdir != dir {
                    continue;
                }
            }
            let child = self
                .parent
                .get(&w)
                .map(|(p, pr, pdir)| *p == u && *pr == r && *pdir == dir)
                .unwrap_or(false);
            out.push(Item {
                node: w,
                rel: r,
                dir,
                leaf_ref: !child,
            });
        }
        out
    }

    pub fn render(&self, v: &Vault, g: &Graph) -> String {
        let n = &v.notes[self.root];
        let mut s = format!("{}  ({})", n.title, n.id);
        if let Some(k) = &n.kind {
            s.push_str(&format!("  [{}]", k));
        }
        s.push('\n');
        self.render_children(v, g, self.root, "", &mut s);
        s
    }

    fn render_children(&self, v: &Vault, g: &Graph, u: usize, prefix: &str, s: &mut String) {
        let items = self.items(g, u);
        let last = items.len().saturating_sub(1);
        for (k, it) in items.iter().enumerate() {
            let (branch, cont) = if k == last {
                ("`-- ", "    ")
            } else {
                ("|-- ", "|   ")
            };
            let note = &v.notes[it.node];
            s.push_str(&format!(
                "{}{}{} {}  {}",
                prefix,
                branch,
                it.dir.arrow(),
                it.rel,
                note.title
            ));
            if it.leaf_ref {
                s.push_str("  (*)");
            }
            s.push('\n');
            if !it.leaf_ref {
                self.render_children(v, g, it.node, &format!("{}{}", prefix, cont), s);
            }
        }
    }

    pub fn to_json(&self, v: &Vault, g: &Graph) -> serde_json::Value {
        let mut root = self.node_json(v, g, self.root);
        root["rel"] = serde_json::Value::Null;
        root
    }

    fn node_json(&self, v: &Vault, g: &Graph, u: usize) -> serde_json::Value {
        let n = &v.notes[u];
        let children: Vec<serde_json::Value> = self
            .items(g, u)
            .into_iter()
            .map(|it| {
                let mut c = if it.leaf_ref {
                    let m = &v.notes[it.node];
                    serde_json::json!({ "id": m.id, "title": m.title, "type": m.kind, "ref": true, "children": [] })
                } else {
                    self.node_json(v, g, it.node)
                };
                c["rel"] = serde_json::json!(it.rel);
                c["dir"] = serde_json::json!(if it.dir == Dir::Out { "out" } else { "in" });
                c
            })
            .collect();
        serde_json::json!({ "id": n.id, "title": n.title, "type": n.kind, "ref": false, "children": children })
    }
}

/// Outgoing links first, then by relation, then by title: a stable drawing.
fn sorted_neighbours(g: &Graph, u: usize, rel: Option<&str>) -> Vec<(usize, String, Dir)> {
    let mut nb: Vec<(usize, String, Dir)> = g
        .neighbours(u)
        .filter(|(_, r, _)| rel.map(|x| x == *r).unwrap_or(true))
        .map(|(w, r, d)| (w, r.to_string(), d))
        .collect();
    nb.sort_by(|a, b| {
        (a.2 == Dir::In)
            .cmp(&(b.2 == Dir::In))
            .then(a.1.cmp(&b.1))
            .then(a.0.cmp(&b.0))
    });
    nb
}
