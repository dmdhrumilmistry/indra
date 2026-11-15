//! Control flow for workflows: conditions (`when`, `until`) and loop items
//! (`foreach`).
//!
//! A condition is a small expression over what earlier steps did and what the
//! person running the workflow chose:
//!
//! ```text
//! scan.failed
//! test.exit_code != 0 and input.fix == "true"
//! not (review.output contains "no findings") or input.always
//! self.output contains "ALL PASS"          (in `until`: this step's last run)
//! ```
//!
//! References: `<step>.succeeded | failed | skipped | status | output |
//! exit_code`, `input.<name>`, and `self.<same fields>` inside `until`.
//! Operators: `==`, `!=`, `contains`, `<`, `<=`, `>`, `>=` (numbers), `and`,
//! `or`, `not`, parentheses. Literals: "text" or 'text', numbers, true, false.
//! A bare reference is true when it is true, or a non-empty string that is
//! not "false" or "0".

use std::collections::HashMap;

pub type Result<T> = std::result::Result<T, String>;

/// What a step reference can ask.
pub const FIELDS: [&str; 6] = [
    "succeeded",
    "failed",
    "skipped",
    "status",
    "output",
    "exit_code",
];

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(String),
    Num(f64),
    /// A dotted name: `scan.failed`, `input.x`, `true`.
    Ref(String),
    Op(&'static str),
    LParen,
    RParen,
}

fn lex(s: &str) -> Result<Vec<Tok>> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch == '(' {
            out.push(Tok::LParen);
            i += 1;
            continue;
        }
        if ch == ')' {
            out.push(Tok::RParen);
            i += 1;
            continue;
        }
        if ch == '"' || ch == '\'' {
            let q = ch;
            let mut v = String::new();
            i += 1;
            loop {
                match c.get(i) {
                    None => return Err("a string is not closed".into()),
                    Some('\\') if c.get(i + 1).is_some() => {
                        v.push(c[i + 1]);
                        i += 2;
                    }
                    Some(x) if *x == q => {
                        i += 1;
                        break;
                    }
                    Some(x) => {
                        v.push(*x);
                        i += 1;
                    }
                }
            }
            out.push(Tok::Str(v));
            continue;
        }
        let two: String = c[i..(i + 2).min(c.len())].iter().collect();
        if let Some(op) = ["==", "!=", "<=", ">="].into_iter().find(|o| *o == two) {
            out.push(Tok::Op(op));
            i += 2;
            continue;
        }
        if ch == '<' || ch == '>' {
            out.push(Tok::Op(if ch == '<' { "<" } else { ">" }));
            i += 1;
            continue;
        }
        if ch == '!' {
            out.push(Tok::Op("not"));
            i += 1;
            continue;
        }
        if ch.is_ascii_digit() || (ch == '-' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            i += 1;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                i += 1;
            }
            let t: String = c[start..i].iter().collect();
            out.push(Tok::Num(
                t.parse().map_err(|_| format!("bad number {}", t))?,
            ));
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || matches!(c[i], '_' | '-' | '.')) {
                i += 1;
            }
            let w: String = c[start..i].iter().collect();
            out.push(match w.as_str() {
                "and" | "AND" => Tok::Op("and"),
                "or" | "OR" => Tok::Op("or"),
                "not" | "NOT" => Tok::Op("not"),
                "contains" => Tok::Op("contains"),
                _ => Tok::Ref(w.trim_end_matches('.').to_string()),
            });
            continue;
        }
        if ch == '&' && c.get(i + 1) == Some(&'&') {
            out.push(Tok::Op("and"));
            i += 2;
            continue;
        }
        if ch == '|' && c.get(i + 1) == Some(&'|') {
            out.push(Tok::Op("or"));
            i += 2;
            continue;
        }
        return Err(format!("unexpected {:?}", ch));
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Str(String),
    Num(f64),
    Bool(bool),
    Ref(String),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Cmp(&'static str, Box<Expr>, Box<Expr>),
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn or(&mut self) -> Result<Expr> {
        let mut l = self.and()?;
        while self.peek() == Some(&Tok::Op("or")) {
            self.i += 1;
            l = Expr::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr> {
        let mut l = self.not()?;
        while self.peek() == Some(&Tok::Op("and")) {
            self.i += 1;
            l = Expr::And(Box::new(l), Box::new(self.not()?));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<Expr> {
        if self.peek() == Some(&Tok::Op("not")) {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr> {
        let l = self.atom()?;
        if let Some(Tok::Op(op)) = self.peek().cloned() {
            if ["==", "!=", "<", "<=", ">", ">=", "contains"].contains(&op) {
                self.i += 1;
                let r = self.atom()?;
                return Ok(Expr::Cmp(op, Box::new(l), Box::new(r)));
            }
        }
        Ok(l)
    }
    fn atom(&mut self) -> Result<Expr> {
        let t = self
            .t
            .get(self.i)
            .cloned()
            .ok_or("the condition ends too soon")?;
        self.i += 1;
        Ok(match t {
            Tok::Str(s) => Expr::Str(s),
            Tok::Num(n) => Expr::Num(n),
            Tok::Ref(r) if r == "true" => Expr::Bool(true),
            Tok::Ref(r) if r == "false" => Expr::Bool(false),
            Tok::Ref(r) => Expr::Ref(r),
            Tok::LParen => {
                let e = self.or()?;
                if self.t.get(self.i) != Some(&Tok::RParen) {
                    return Err("a ( is not closed".into());
                }
                self.i += 1;
                e
            }
            Tok::RParen => return Err("unexpected )".into()),
            Tok::Op(o) => return Err(format!("unexpected '{}'", o)),
        })
    }
}

pub fn parse(s: &str) -> Result<Expr> {
    if s.len() > 2000 {
        return Err("the condition is too long".into());
    }
    let t = lex(s)?;
    if t.is_empty() {
        return Err("the condition is empty".into());
    }
    let mut p = P { t, i: 0 };
    let e = p.or()?;
    if p.i != p.t.len() {
        return Err(format!("unexpected {:?} in the condition", p.t[p.i]));
    }
    Ok(e)
}

/// Every reference in the expression.
pub fn refs(e: &Expr) -> Vec<String> {
    let mut v = Vec::new();
    fn go(e: &Expr, v: &mut Vec<String>) {
        match e {
            Expr::Ref(r) => v.push(r.clone()),
            Expr::Not(a) => go(a, v),
            Expr::And(a, b) | Expr::Or(a, b) | Expr::Cmp(_, a, b) => {
                go(a, v);
                go(b, v);
            }
            _ => {}
        }
    }
    go(e, &mut v);
    v
}

/// Check a condition: it parses, and every reference names an input, `self`
/// (when allowed) or one of `steps`, with a known field.
pub fn check(s: &str, steps: &[&str], inputs: &[&str], allow_self: bool) -> Result<Vec<String>> {
    let e = parse(s).map_err(|e| format!("condition {:?}: {}", s, e))?;
    let mut used = Vec::new();
    for r in refs(&e) {
        let (head, field) = r.split_once('.').unwrap_or((r.as_str(), ""));
        match head {
            "input" => {
                if !inputs.contains(&field) {
                    return Err(format!("condition {:?}: there is no input '{}'", s, field));
                }
            }
            "self" if allow_self => {
                if !FIELDS.contains(&field) {
                    return Err(format!(
                        "condition {:?}: self.{} is not one of {}",
                        s,
                        field,
                        FIELDS.join(", ")
                    ));
                }
            }
            _ => {
                if !steps.contains(&head) {
                    return Err(format!(
                        "condition {:?}: '{}' is not a step of this workflow (write <step>.failed, input.<name>{})",
                        s,
                        head,
                        if allow_self { " or self.<field>" } else { "" }
                    ));
                }
                if !FIELDS.contains(&field) {
                    return Err(format!(
                        "condition {:?}: {}.{} is not one of {}",
                        s,
                        head,
                        field,
                        FIELDS.join(", ")
                    ));
                }
                used.push(head.to_string());
            }
        }
    }
    Ok(used)
}

/// The condition with step names renamed (a sub-workflow's steps get a
/// prefix when it is inlined). Strings and inputs are left alone.
pub fn rename(s: &str, map: &HashMap<String, String>) -> String {
    let Ok(toks) = lex(s) else {
        return s.to_string();
    };
    let mut out = Vec::new();
    for t in toks {
        out.push(match t {
            Tok::Str(v) => format!("{:?}", v),
            Tok::Num(n) => n.to_string(),
            Tok::Ref(r) => match r.split_once('.') {
                Some((h, f)) if map.contains_key(h) => format!("{}.{}", map[h], f),
                _ => r,
            },
            Tok::Op(o) => o.to_string(),
            Tok::LParen => "(".into(),
            Tok::RParen => ")".into(),
        });
    }
    out.join(" ")
}

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Str(String),
    Num(f64),
    Bool(bool),
}

impl Val {
    fn truthy(&self) -> bool {
        match self {
            Val::Bool(b) => *b,
            Val::Num(n) => *n != 0.0,
            Val::Str(s) => {
                let s = s.trim();
                !s.is_empty() && s != "false" && s != "0"
            }
        }
    }
    fn text(&self) -> String {
        match self {
            Val::Str(s) => s.clone(),
            Val::Num(n) => {
                if n.fract() == 0.0 {
                    format!("{}", *n as i64)
                } else {
                    n.to_string()
                }
            }
            Val::Bool(b) => b.to_string(),
        }
    }
    fn num(&self) -> Option<f64> {
        match self {
            Val::Num(n) => Some(*n),
            Val::Str(s) => s.trim().parse().ok(),
            Val::Bool(_) => None,
        }
    }
}

/// Evaluate with `lookup` giving each reference's value (None: unknown,
/// which reads as an empty string).
pub fn eval(e: &Expr, lookup: &dyn Fn(&str) -> Option<Val>) -> bool {
    value(e, lookup).truthy()
}

fn value(e: &Expr, lookup: &dyn Fn(&str) -> Option<Val>) -> Val {
    match e {
        Expr::Str(s) => Val::Str(s.clone()),
        Expr::Num(n) => Val::Num(*n),
        Expr::Bool(b) => Val::Bool(*b),
        Expr::Ref(r) => lookup(r).unwrap_or(Val::Str(String::new())),
        Expr::Not(a) => Val::Bool(!value(a, lookup).truthy()),
        Expr::And(a, b) => Val::Bool(value(a, lookup).truthy() && value(b, lookup).truthy()),
        Expr::Or(a, b) => Val::Bool(value(a, lookup).truthy() || value(b, lookup).truthy()),
        Expr::Cmp(op, a, b) => {
            let (x, y) = (value(a, lookup), value(b, lookup));
            let num = || Some((x.num()?, y.num()?));
            Val::Bool(match *op {
                "contains" => x.text().contains(&y.text()),
                "==" => match (x.num(), y.num()) {
                    (Some(p), Some(q)) => p == q,
                    _ => match (&x, &y) {
                        (Val::Bool(p), _) => *p == y.truthy(),
                        (_, Val::Bool(q)) => x.truthy() == *q,
                        _ => x.text() == y.text(),
                    },
                },
                "!=" => match (x.num(), y.num()) {
                    (Some(p), Some(q)) => p != q,
                    _ => match (&x, &y) {
                        (Val::Bool(p), _) => *p != y.truthy(),
                        (_, Val::Bool(q)) => x.truthy() != *q,
                        _ => x.text() != y.text(),
                    },
                },
                "<" => num().is_some_and(|(p, q)| p < q),
                "<=" => num().is_some_and(|(p, q)| p <= q),
                ">" => num().is_some_and(|(p, q)| p > q),
                ">=" => num().is_some_and(|(p, q)| p >= q),
                _ => false,
            })
        }
    }
}

/// `foreach` values, each split on new lines and commas, trimmed, empty ones
/// dropped: `["a", "b"]`, `"a, b"`, or an input holding a list.
pub fn items(raw: &[String]) -> Vec<String> {
    raw.iter()
        .flat_map(|v| v.split(['\n', ',']))
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// The most items a `foreach` may expand to.
pub const MAX_ITEMS: usize = 50;
/// The most runs an `until` loop may make.
pub const MAX_RUNS: u32 = 20;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(r: &str) -> Option<Val> {
        Some(match r {
            "scan.failed" => Val::Bool(true),
            "scan.succeeded" => Val::Bool(false),
            "scan.exit_code" => Val::Num(2.0),
            "scan.output" => Val::Str("3 findings, 1 CRITICAL".into()),
            "input.deep" => Val::Str("true".into()),
            "input.empty" => Val::Str(String::new()),
            "self.output" => Val::Str("ALL PASS".into()),
            _ => return None,
        })
    }

    #[test]
    fn conditions_parse_and_evaluate() {
        let t = |s: &str| eval(&parse(s).unwrap(), &ctx);
        assert!(t("scan.failed"));
        assert!(!t("scan.succeeded"));
        assert!(t("scan.exit_code != 0"));
        assert!(t("scan.exit_code == 2 and input.deep"));
        assert!(t("scan.exit_code >= 2"));
        assert!(!t("scan.exit_code < 1"));
        assert!(t("scan.output contains 'CRITICAL'"));
        assert!(t("not (scan.output contains \"none\")"));
        assert!(t("input.empty or scan.failed"));
        assert!(!t("input.empty"));
        assert!(t("input.deep == true"));
        assert!(t("input.empty == ''"));
        assert!(t("!scan.succeeded && input.deep"));
        assert!(t("self.output contains 'PASS'"));
        assert!(!t("unknown.output contains 'x'"));
        for bad in [
            "",
            "scan.failed and",
            "(scan.failed",
            "scan.failed )",
            "'open",
            "a = b",
        ] {
            assert!(parse(bad).is_err(), "{}", bad);
        }
    }

    #[test]
    fn conditions_are_checked_and_renamed() {
        let steps = ["scan", "fix-it"];
        assert_eq!(
            check(
                "scan.failed and fix-it.output contains 'x'",
                &steps,
                &["deep"],
                false
            )
            .unwrap(),
            vec!["scan", "fix-it"]
        );
        assert!(check("input.deep", &steps, &["deep"], false).is_ok());
        assert!(check("input.nope", &steps, &["deep"], false).is_err());
        assert!(check("other.failed", &steps, &[], false).is_err());
        assert!(check("scan.colour", &steps, &[], false).is_err());
        assert!(check("self.output", &steps, &[], false).is_err());
        assert!(check("self.output contains 'ok'", &steps, &[], true).is_ok());
        let map: HashMap<String, String> = [("scan".to_string(), "sub__scan".to_string())].into();
        let r = rename("scan.failed and input.scan == 'scan.failed'", &map);
        assert_eq!(r, "sub__scan.failed and input.scan == \"scan.failed\"");
        assert!(eval(&parse(&r).unwrap(), &|x| match x {
            "sub__scan.failed" => Some(Val::Bool(true)),
            "input.scan" => Some(Val::Str("scan.failed".into())),
            _ => None,
        }));
    }

    #[test]
    fn loop_items() {
        assert_eq!(
            items(&["a, b\nc".into(), " ".into(), "d".into()]),
            vec!["a", "b", "c", "d"]
        );
        assert!(items(&[]).is_empty());
    }
}
