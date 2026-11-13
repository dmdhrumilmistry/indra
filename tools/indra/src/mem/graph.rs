//! The note graph: nodes are notes, edges are resolved links with a relation
//! type. Links whose target does not exist are kept as `dangling` (Obsidian
//! calls them unresolved links) and shown as ghost nodes in exports.

use crate::mem::vault::Vault;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Out,
    In,
}

impl Dir {
    pub fn arrow(self) -> &'static str {
        match self {
            Dir::Out => "->",
            Dir::In => "<-",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Dangling {
    pub from: usize,
    pub target: String,
    pub rel: String,
    pub line: usize,
}

pub struct Graph {
    /// out[i] = (target, rel)
    pub out: Vec<Vec<(usize, String)>>,
    /// inc[i] = (source, rel)
    pub inc: Vec<Vec<(usize, String)>>,
    pub dangling: Vec<Dangling>,
}

impl Graph {
    pub fn build(v: &Vault) -> Graph {
        let n = v.notes.len();
        let mut out: Vec<Vec<(usize, String)>> = vec![Vec::new(); n];
        let mut inc: Vec<Vec<(usize, String)>> = vec![Vec::new(); n];
        let mut seen: HashSet<(usize, usize, String)> = HashSet::new();
        let mut dangling = Vec::new();
        for (i, note) in v.notes.iter().enumerate() {
            for l in &note.links {
                match v.resolve(&l.target) {
                    Some(j) if j == i => {}
                    Some(j) => {
                        if seen.insert((i, j, l.rel.clone())) {
                            out[i].push((j, l.rel.clone()));
                            inc[j].push((i, l.rel.clone()));
                        }
                    }
                    None => dangling.push(Dangling {
                        from: i,
                        target: l.target.clone(),
                        rel: l.rel.clone(),
                        line: l.line,
                    }),
                }
            }
        }
        Graph { out, inc, dangling }
    }

    pub fn edge_count(&self) -> usize {
        self.out.iter().map(|e| e.len()).sum()
    }

    pub fn degree(&self, i: usize) -> usize {
        self.out[i].len() + self.inc[i].len()
    }

    /// Both directions, as (neighbour, rel, direction-from-i).
    pub fn neighbours(&self, i: usize) -> impl Iterator<Item = (usize, &str, Dir)> {
        self.out[i]
            .iter()
            .map(|(j, r)| (*j, r.as_str(), Dir::Out))
            .chain(self.inc[i].iter().map(|(j, r)| (*j, r.as_str(), Dir::In)))
    }

    /// Breadth-first neighbourhood up to `depth` hops, ignoring direction.
    /// Returns (node, hops, parent step) in visit order, start excluded.
    pub fn bfs(&self, start: usize, depth: usize, rel: Option<&str>) -> Vec<(usize, usize, Step)> {
        let mut seen = HashSet::from([start]);
        let mut q = VecDeque::from([(start, 0usize)]);
        let mut out = Vec::new();
        while let Some((u, d)) = q.pop_front() {
            if d >= depth {
                continue;
            }
            for (w, r, dir) in self.neighbours(u) {
                if rel.map(|x| x != r).unwrap_or(false) || !seen.insert(w) {
                    continue;
                }
                out.push((
                    w,
                    d + 1,
                    Step {
                        from: u,
                        to: w,
                        rel: r.to_string(),
                        dir,
                    },
                ));
                q.push_back((w, d + 1));
            }
        }
        out
    }

    /// Shortest undirected path; each step names the relation used.
    pub fn path(&self, a: usize, b: usize) -> Option<Vec<Step>> {
        if a == b {
            return Some(Vec::new());
        }
        let mut prev: HashMap<usize, Step> = HashMap::new();
        let mut seen = HashSet::from([a]);
        let mut q = VecDeque::from([a]);
        while let Some(u) = q.pop_front() {
            for (w, r, dir) in self.neighbours(u) {
                if !seen.insert(w) {
                    continue;
                }
                prev.insert(
                    w,
                    Step {
                        from: u,
                        to: w,
                        rel: r.to_string(),
                        dir,
                    },
                );
                if w == b {
                    let mut steps = Vec::new();
                    let mut cur = b;
                    while cur != a {
                        let s = prev[&cur].clone();
                        cur = s.from;
                        steps.push(s);
                    }
                    steps.reverse();
                    return Some(steps);
                }
                q.push_back(w);
            }
        }
        None
    }

    /// Spreading activation: every seed passes `decay^hops` of its score to
    /// its neighbourhood. A node keeps its best score and the step that gave
    /// it, so every recalled note can explain why it was recalled.
    pub fn activate(
        &self,
        seeds: &[(usize, f64)],
        depth: usize,
        decay: f64,
    ) -> HashMap<usize, (f64, Option<Step>)> {
        let mut best: HashMap<usize, (f64, Option<Step>)> = HashMap::new();
        let mut frontier: Vec<(usize, f64)> = Vec::new();
        for &(i, s) in seeds {
            let e = best.entry(i).or_insert((0.0, None));
            if s > e.0 {
                *e = (s, None);
            }
            frontier.push((i, s));
        }
        for _ in 0..depth {
            let mut next: HashMap<usize, (f64, Step)> = HashMap::new();
            for &(u, s) in &frontier {
                // Damp hubs a little so one index note does not drag in everything.
                let fan = 1.0 + (self.degree(u) as f64).ln_1p() * 0.25;
                let pass = s * decay / fan;
                for (w, r, dir) in self.neighbours(u) {
                    if pass <= best.get(&w).map(|b| b.0).unwrap_or(0.0) {
                        continue;
                    }
                    let step = Step {
                        from: u,
                        to: w,
                        rel: r.to_string(),
                        dir,
                    };
                    match next.get(&w) {
                        Some((cur, _)) if *cur >= pass => {}
                        _ => {
                            next.insert(w, (pass, step));
                        }
                    }
                }
            }
            frontier.clear();
            for (w, (s, step)) in next {
                best.insert(w, (s, Some(step)));
                frontier.push((w, s));
            }
            frontier.sort_by_key(|x| x.0);
            if frontier.is_empty() {
                break;
            }
        }
        best
    }
}

#[derive(Debug, Clone)]
pub struct Step {
    pub from: usize,
    pub to: usize,
    pub rel: String,
    pub dir: Dir,
}
