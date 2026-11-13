//! Ranked full-text search: BM25 over title, aliases, tags, type and body,
//! with field boosts, light plural stemming and prefix matching.

use crate::note::Note;
use std::collections::HashMap;

const K1: f64 = 1.2;
const B: f64 = 0.75;
const TITLE_BOOST: u32 = 4;
const TAG_BOOST: u32 = 3;
const ALIAS_BOOST: u32 = 3;

const STOP: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "how", "in", "is", "it", "of",
    "on", "or", "that", "the", "this", "to", "was", "what", "when", "with",
];

pub fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| stem(&t.to_lowercase()))
        .filter(|t| !STOP.contains(&t.as_str()))
        .collect()
}

/// Strip a plural `s`/`es` so `injections` meets `injection`.
fn stem(t: &str) -> String {
    let n = t.chars().count();
    if n > 4 && t.ends_with("ies") {
        return format!("{}y", &t[..t.len() - 3]);
    }
    if n > 4
        && (t.ends_with("sses") || t.ends_with("xes") || t.ends_with("ches") || t.ends_with("shes"))
    {
        return t[..t.len() - 2].to_string();
    }
    if n > 3 && t.ends_with('s') && !t.ends_with("ss") && !t.ends_with("us") && !t.ends_with("is") {
        return t[..t.len() - 1].to_string();
    }
    t.to_string()
}

pub struct Index {
    docs: Vec<HashMap<String, u32>>,
    lens: Vec<f64>,
    avg: f64,
    df: HashMap<String, u32>,
}

impl Index {
    pub fn build(notes: &[Note]) -> Index {
        let mut docs = Vec::with_capacity(notes.len());
        let mut lens = Vec::with_capacity(notes.len());
        let mut df: HashMap<String, u32> = HashMap::new();
        for n in notes {
            let mut tf: HashMap<String, u32> = HashMap::new();
            let mut len = 0u32;
            let mut add = |text: &str, w: u32| {
                for t in tokenize(text) {
                    *tf.entry(t).or_insert(0) += w;
                    len += w;
                }
            };
            add(&n.title, TITLE_BOOST);
            for a in &n.aliases {
                add(a, ALIAS_BOOST);
            }
            for t in &n.tags {
                add(&t.replace(['/', '-', '_'], " "), TAG_BOOST);
            }
            if let Some(k) = &n.kind {
                add(k, 1);
            }
            add(&n.body, 1);
            for t in tf.keys() {
                *df.entry(t.clone()).or_insert(0) += 1;
            }
            docs.push(tf);
            lens.push(len as f64);
        }
        let avg = if lens.is_empty() {
            1.0
        } else {
            (lens.iter().sum::<f64>() / lens.len() as f64).max(1.0)
        };
        Index {
            docs,
            lens,
            avg,
            df,
        }
    }

    /// (note index, score), best first. Only notes passing `keep` are scored.
    pub fn search(
        &self,
        notes: &[Note],
        query: &str,
        keep: impl Fn(&Note) -> bool,
    ) -> Vec<(usize, f64)> {
        let mut terms = tokenize(query);
        terms.sort();
        terms.dedup();
        if terms.is_empty() {
            return Vec::new();
        }
        // Expand each query term to vocabulary terms it prefixes (weight 0.5).
        let n_docs = self.docs.len() as f64;
        // Per query term: (vocabulary term, weight, idf).
        let mut expanded: Vec<Vec<(String, f64, f64)>> = vec![Vec::new(); terms.len()];
        for (qi, q) in terms.iter().enumerate() {
            for (t, &df) in &self.df {
                let w = if t == q {
                    1.0
                } else if q.chars().count() >= 3 && t.starts_with(q.as_str()) {
                    0.5
                } else {
                    continue;
                };
                let idf = ((n_docs - df as f64 + 0.5) / (df as f64 + 0.5) + 1.0).ln();
                expanded[qi].push((t.clone(), w, idf));
            }
        }
        let q_lower = query.trim().to_lowercase();
        let mut out = Vec::new();
        for (i, tf) in self.docs.iter().enumerate() {
            if !keep(&notes[i]) {
                continue;
            }
            let norm = K1 * (1.0 - B + B * self.lens[i] / self.avg);
            let mut score = 0.0;
            let mut matched = 0usize;
            for group in &expanded {
                let mut hit = false;
                for (t, w, idf) in group {
                    if let Some(&f) = tf.get(t) {
                        let f = f as f64;
                        score += w * idf * f * (K1 + 1.0) / (f + norm);
                        hit = true;
                    }
                }
                matched += hit as usize;
            }
            if score <= 0.0 {
                continue;
            }
            // Reward covering more of the query, and a phrase hit in the title.
            score *= 0.5 + 0.5 * matched as f64 / terms.len() as f64;
            if q_lower.len() > 2 && notes[i].title.to_lowercase().contains(&q_lower) {
                score += 2.0;
            }
            out.push((i, score));
        }
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }
}

/// The first body line that mentions a query term, trimmed to `max` chars.
pub fn snippet(body: &str, query: &str, max: usize) -> String {
    let terms = tokenize(query);
    let mut fallback = None;
    for line in body.lines() {
        let t = line.trim();
        let relation = t.contains("[[") && crate::note::inline_field(t).is_some();
        if t.is_empty() || t.starts_with('#') || t == "---" || relation {
            continue;
        }
        if fallback.is_none() {
            fallback = Some(t);
        }
        let toks = tokenize(t);
        if terms
            .iter()
            .any(|q| toks.iter().any(|x| x.starts_with(q.as_str())))
        {
            return truncate(t, max);
        }
    }
    truncate(fallback.unwrap_or(""), max)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn n(id: &str, src: &str) -> Note {
        Note::parse(id, PathBuf::new(), src)
    }

    #[test]
    fn stems() {
        assert_eq!(
            tokenize("Injections, policies and classes is"),
            vec!["injection", "policy", "class"]
        );
        assert_eq!(stem("status"), "status");
        assert_eq!(stem("analysis"), "analysis");
    }

    #[test]
    fn ranks_title_over_body_and_prefix_matches() {
        let notes = vec![
            n(
                "a",
                "# Unrelated\nsome text mentioning deserialization once\n",
            ),
            n("b", "# Deserialization gadget chains\nbody\n"),
            n("c", "# Something else\nnothing here\n"),
        ];
        let idx = Index::build(&notes);
        let r = idx.search(&notes, "deserial", |_| true);
        assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![1, 0]);
        let r = idx.search(&notes, "gadget", |n| n.id != "b");
        assert!(r.is_empty());
        assert!(idx.search(&notes, "the and", |_| true).is_empty());
    }

    #[test]
    fn snippets() {
        let s = snippet("# T\nfirst line\nthe SQLi sink is here\n", "sqli", 100);
        assert_eq!(s, "the SQLi sink is here");
        assert_eq!(truncate("abcdef", 5), "ab...");
    }
}
