//! End-to-end tests that drive the real binary against throwaway vaults.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Vault {
    dir: PathBuf,
}

impl Vault {
    fn new() -> Vault {
        let dir = std::env::temp_dir().join(format!(
            "indra-mem-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Vault { dir }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_indra"));
        c.arg("mem").arg("--vault").arg(&self.dir).args(args);
        c.env_remove("INDRA_VAULT");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Run, assert success, return stdout.
    fn ok(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(
            o.status.success(),
            "indra mem {:?} failed:\n{}{}",
            args,
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.ok(&a)).unwrap()
    }

    fn read(&self, id: &str) -> String {
        fs::read_to_string(self.dir.join(format!("{}.md", id))).unwrap()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn seed(v: &Vault) {
    v.ok(&[
        "add",
        "SQL injection",
        "-t",
        "vuln-class",
        "--alias",
        "SQLi",
        "-b",
        "Untrusted input reaches a SQL interpreter.",
    ]);
    v.ok(&[
        "add",
        "Parameterized queries",
        "-t",
        "pattern",
        "--tag",
        "defense",
        "-b",
        "Bind input as parameters.",
    ]);
    v.ok(&[
        "add",
        "Django extra() bypasses the ORM",
        "-t",
        "learning",
        "--tag",
        "sqli",
        "--tag",
        "django",
        "--rel",
        "instance-of=SQLi",
        "--rel",
        "mitigated-by=parameterized queries",
        "-b",
        "QuerySet.extra() takes raw SQL fragments.",
    ]);
}

#[test]
fn add_writes_obsidian_compatible_markdown() {
    let v = Vault::new();
    seed(&v);
    let doc = v.read("django-extra-bypasses-the-orm");
    assert!(doc.starts_with(
        "---\ntitle: Django extra() bypasses the ORM\ntype: learning\ntags: [sqli, django]\n"
    ));
    assert!(doc.contains(
        "\n# Django extra() bypasses the ORM\n\nQuerySet.extra() takes raw SQL fragments.\n"
    ));
    // Resolved targets are written by id, which is the file name Obsidian matches.
    assert!(doc.contains("## Relations\n- instance-of:: [[sql-injection]]\n- mitigated-by:: [[parameterized-queries]]\n"));
    assert!(v.path(".indra").is_dir());
    let out = v.run(&["add", "sql injection"]);
    assert!(!out.status.success(), "duplicate title must be refused");
}

#[test]
fn unresolved_links_heal_when_the_note_is_created() {
    let v = Vault::new();
    v.ok(&["add", "A", "--rel", "uses=Future Note"]);
    assert!(v.read("a").contains("- uses:: [[future-note|Future Note]]"));
    assert!(!v.run(&["check"]).status.success());
    v.ok(&["add", "Future Note"]);
    v.ok(&["check"]);
    let n = v.json(&["neighbors", "a"]);
    assert_eq!(n[0]["id"], "future-note");
}

#[test]
fn link_is_idempotent_and_unlink_removes_it() {
    let v = Vault::new();
    seed(&v);
    let r = v.json(&[
        "link",
        "parameterized-queries",
        "SQLi",
        "--rel",
        "Defends Against",
    ]);
    assert_eq!(r["changed"], true);
    let r = v.json(&[
        "link",
        "Parameterized queries",
        "sql injection",
        "--rel",
        "defends-against",
    ]);
    assert_eq!(r["changed"], false);
    assert_eq!(
        v.read("parameterized-queries")
            .matches("defends-against::")
            .count(),
        1
    );
    v.ok(&[
        "unlink",
        "parameterized-queries",
        "SQLi",
        "--rel",
        "defends-against",
    ]);
    assert!(!v.read("parameterized-queries").contains("defends-against"));
    assert!(!v
        .run(&["unlink", "parameterized-queries", "SQLi"])
        .status
        .success());
    assert!(
        !v.run(&["link", "sqli", "sqli"]).status.success(),
        "self links are refused"
    );
}

#[test]
fn upsert_merges_body_tags_and_relations() {
    let v = Vault::new();
    seed(&v);
    v.ok(&[
        "add",
        "SQLi",
        "--upsert",
        "--tag",
        "owasp-a03",
        "--rel",
        "mitigated-by=Parameterized queries",
        "-b",
        "Second paragraph.",
    ]);
    v.ok(&[
        "add",
        "SQLi",
        "--upsert",
        "--rel",
        "mitigated-by=Parameterized queries",
        "-b",
        "Second paragraph.",
    ]);
    // Text that starts with a hyphen (a Markdown list) is a value, not a flag.
    v.ok(&["add", "SQLi", "--upsert", "-b", "- a list item body"]);
    v.ok(&["append", "SQLi", "- appended", "item"]);
    let doc = v.read("sql-injection");
    assert!(doc.contains("tags: [owasp-a03]"));
    assert!(doc.contains(
        "Untrusted input reaches a SQL interpreter.\n\nSecond paragraph.\n\n- a list item body\n\n- appended item\n\n## Relations\n"
    ));
    assert_eq!(doc.matches("mitigated-by::").count(), 1);
}

#[test]
fn append_goes_above_relations_and_preserves_crlf() {
    let v = Vault::new();
    fs::create_dir_all(v.path(".indra")).unwrap();
    let crlf = "---\r\ntitle: Win\r\ncustom: keep\r\n---\r\n# Win\r\n\r\nfirst\r\n\r\n## Relations\r\n- x:: [[y]]\r\n";
    fs::write(v.path("win.md"), crlf).unwrap();
    v.ok(&["append", "win", "second", "line"]);
    v.ok(&["tag", "win", "a,b", "#c"]);
    let doc = fs::read_to_string(v.path("win.md")).unwrap();
    assert!(
        !doc.replace("\r\n", "").contains('\n'),
        "line endings stay CRLF: {:?}",
        doc
    );
    let doc = doc.replace("\r\n", "\n");
    assert!(doc.contains("custom: keep\n"));
    assert!(doc.contains("tags: [a, b, c]\n"));
    assert!(doc.contains("updated: "));
    assert!(doc.contains("first\n\nsecond line\n\n## Relations\n- x:: [[y]]\n"));
}

#[test]
fn reads_hand_written_obsidian_notes() {
    let v = Vault::new();
    fs::create_dir_all(v.path("sub")).unwrap();
    fs::create_dir_all(v.path(".obsidian")).unwrap();
    fs::write(v.path(".obsidian/ignored.md"), "[[nowhere]]").unwrap();
    fs::write(
        v.path("sub/Deep Note.md"),
        "---\naliases:\n  - deep\ntags: x y\n---\nBody #inline-tag\n",
    )
    .unwrap();
    fs::write(
        v.path("Top.md"),
        "Links: [[Deep Note|label]], [[deep#heading]], [md](sub/Deep%20Note.md)\ncauses:: [[sub/Deep Note]]\n```\n[[in-code]]\n```\n",
    )
    .unwrap();
    v.ok(&["check"]);
    let s = v.json(&["show", "deep note"]);
    assert_eq!(s["id"], "sub/Deep Note");
    assert_eq!(s["tags"], serde_json::json!(["x", "y", "inline-tag"]));
    let rels: Vec<String> = s["relations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rel"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(rels, vec!["related", "causes"]);
    assert_eq!(
        v.json(&["list", "--tag", "inline-tag"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn search_recall_path_and_neighbours() {
    let v = Vault::new();
    seed(&v);
    v.ok(&["add", "Unrelated note", "-b", "nothing to see"]);

    let s = v.json(&["search", "raw", "sql"]);
    assert_eq!(s[0]["id"], "django-extra-bypasses-the-orm");

    // Recall pulls in the mitigation one hop away, and says why.
    let r = v.json(&["recall", "django", "-n", "3"]);
    let res = r["results"].as_array().unwrap();
    assert_eq!(res[0]["id"], "django-extra-bypasses-the-orm");
    assert!(res[0]["via"].is_null());
    let ids: Vec<&str> = res.iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"parameterized-queries"), "{:?}", ids);
    assert!(!ids.contains(&"unrelated-note"));
    let pq = res
        .iter()
        .find(|x| x["id"] == "parameterized-queries")
        .unwrap();
    assert!(pq["via"].as_str().unwrap().contains("mitigated-by"));

    let text = v.ok(&["recall", "django"]);
    assert!(text.starts_with("# Memory recall: django\n"));
    assert!(
        text.contains("links: -> instance-of SQL injection; -> mitigated-by Parameterized queries")
    );
    assert!(!text.contains("## Relations"));

    let p = v.json(&["path", "sqli", "parameterized queries"]);
    assert_eq!(p.as_array().unwrap().len(), 3);
    assert_eq!(p[1]["dir"], "in");
    assert_eq!(p[2]["rel"], "mitigated-by");
    assert!(!v.run(&["path", "sqli", "unrelated note"]).status.success());

    let n = v.json(&["neighbors", "sqli", "--depth", "2"]);
    assert_eq!(n.as_array().unwrap().len(), 2);
    let n = v.json(&["neighbors", "sqli", "--depth", "2", "--rel", "instance-of"]);
    assert_eq!(n.as_array().unwrap().len(), 1);

    let empty = v.json(&["recall", "kubernetes"]);
    assert_eq!(empty["results"].as_array().unwrap().len(), 0);
}

#[test]
fn exports_are_well_formed_and_html_is_injection_safe() {
    let v = Vault::new();
    seed(&v);
    v.ok(&[
        "add",
        "</script><img src=x onerror=alert(1)>",
        "-b",
        "__INDRA_DATA__ \u{2028} <b>&amp;</b>",
        "--link",
        "ghost target",
    ]);

    let j = v.json(&["export", "--format", "json"]);
    assert!(j["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["ghost"] == true));

    let html = v.ok(&["export"]);
    assert_eq!(
        html.matches("</script>").count(),
        2,
        "only the two real closing tags"
    );
    assert!(!html.contains("<img"));
    assert!(!html.contains('\u{2028}'));
    assert!(html.contains("\\u003c/script\\u003e"));
    // The viewer renders note text with textContent, never innerHTML.
    assert!(!html.contains("innerHTML"));

    let dot = v.ok(&["export", "-f", "dot"]);
    assert!(dot.starts_with("digraph indra {") && dot.contains("[label=\"mitigated-by\"]"));
    let mm = v.ok(&["export", "-f", "mermaid"]);
    assert!(mm.starts_with("graph LR\n") && mm.contains("-->|instance-of|"));
    assert!(!mm.contains("<img"));

    let out = v.path("graph.html");
    v.ok(&["export", "-o", out.to_str().unwrap()]);
    assert!(fs::read_to_string(out).unwrap().contains("indra-data"));
}

#[test]
fn ids_cannot_escape_the_vault() {
    let v = Vault::new();
    let r = v.json(&["add", "../../../etc/passwd", "--folder", "../../x/./y"]);
    assert_eq!(r["id"], "x/y/etc-passwd");
    assert!(Path::new(r["path"].as_str().unwrap()).starts_with(&v.dir));
    let r = v.json(&["add", "日本語"]);
    assert!(r["id"].as_str().unwrap().starts_with("note-"));
}

#[test]
fn rm_reports_new_dangling_links() {
    let v = Vault::new();
    seed(&v);
    let r = v.json(&["rm", "parameterized queries"]);
    assert_eq!(
        r["now_unresolved_in"],
        serde_json::json!(["django-extra-bypasses-the-orm"])
    );
    assert!(!v.run(&["check"]).status.success());
}

#[test]
fn parallel_writers_do_not_lose_updates() {
    let v = Vault::new();
    v.ok(&["add", "Ledger"]);
    let kids: Vec<_> = (0..12)
        .map(|i| {
            v.cmd(&["append", "ledger", &format!("entry-{}", i)])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut k in kids {
        assert!(k.wait().unwrap().success());
    }
    let doc = v.read("ledger");
    for i in 0..12 {
        assert!(
            doc.contains(&format!("entry-{}\n", i)),
            "lost entry-{}:\n{}",
            i,
            doc
        );
    }
    assert!(!v.path(".indra/lock").exists());
}

#[test]
fn missing_vault_reads_as_empty() {
    let v = Vault::new();
    let mut c = Command::new(env!("CARGO_BIN_EXE_indra"));
    c.arg("mem");
    let nowhere = v.dir.join("does-not-exist");
    let out = c
        .arg("--vault")
        .arg(&nowhere)
        .args(["--json", "recall", "anything"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!nowhere.exists(), "reads must not create the vault");
}

#[test]
fn tree_draws_the_graph_in_the_terminal() {
    let v = Vault::new();
    seed(&v);
    v.ok(&["add", "Loner"]);
    let t = v.ok(&["tree", "sqli"]);
    assert_eq!(
        t,
        "SQL injection  (sql-injection)  [vuln-class]\n\
         `-- <- instance-of  Django extra() bypasses the ORM\n    \
         `-- -> mitigated-by  Parameterized queries\n"
    );
    // Depth limits expansion; a cycle back to a drawn note is a (*) leaf.
    v.ok(&[
        "link",
        "parameterized queries",
        "sqli",
        "--rel",
        "defends-against",
    ]);
    let t = v.ok(&["tree", "sqli", "-d", "1"]);
    assert_eq!(t.lines().count(), 3);
    let t = v.ok(&["tree", "sqli"]);
    assert!(t.contains("(*)"), "{}", t);
    // Whole vault: every linked note appears, unlinked ones are listed apart.
    let all = v.ok(&["tree"]);
    for title in [
        "SQL injection",
        "Parameterized queries",
        "Django extra() bypasses the ORM",
    ] {
        assert!(all.contains(title), "{}", all);
    }
    assert!(
        all.ends_with("Unlinked notes:\n  Loner  (loner)\n"),
        "{}",
        all
    );
    let j = v.json(&["tree", "sqli", "--rel", "instance-of"]);
    assert_eq!(j["trees"][0]["children"][0]["rel"], "instance-of");
    assert_eq!(
        j["trees"][0]["children"][0]["children"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}
