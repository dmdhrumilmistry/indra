//! An agent on its own branch: a git worktree, so agents working on the
//! same repository never edit the same files.
//!
//! The worktree lives under indra's data folder (`worktrees/<name>`), on a
//! new branch `indra/<name>` made from whatever the repository had checked
//! out. That base is kept in the branch's own git config
//! (`branch.<branch>.indraBase`), so git itself remembers it and nothing else
//! has to. When the agent is done its work is merged into the base, kept as a
//! branch, or discarded. git runs directly, never through a shell, and only on
//! folders under the worktrees directory.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where agents' worktrees live.
pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("worktrees")
}

/// A worktree made for an agent: where it runs, and on which branch.
#[derive(Debug, Clone)]
pub struct Created {
    /// The worktree's own top folder.
    #[cfg_attr(not(test), allow(dead_code))]
    pub dir: PathBuf,
    /// Where the agent runs: the same subfolder of the repository as the
    /// folder it was launched on.
    pub cwd: PathBuf,
    pub branch: String,
}

/// What the Sessions panel shows about an agent's branch.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Info {
    pub branch: String,
    /// The branch it was made from (and merges into).
    pub base: String,
    /// The repository it belongs to.
    pub repo: String,
    /// Commits on its branch that the base does not have.
    pub commits: u64,
}

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

async fn git(dir: &Path, args: &[&str]) -> Result<Out, String> {
    let out = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("git {} took too long", args.first().unwrap_or(&"")))?
    .map_err(|e| format!("cannot run git: {}", e))?;
    Ok(Out {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// git's output, or its error message.
async fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = git(dir, args).await?;
    if o.ok {
        Ok(o.stdout)
    } else {
        Err(first_lines(&o.stderr, 4))
    }
}

fn first_lines(s: &str, n: usize) -> String {
    s.lines().take(n).collect::<Vec<_>>().join(" ")
}

/// A branch-safe name from the prompt: a few lowercase words and a suffix
/// that keeps names unique.
pub fn branch_name(prompt: &str, salt: u64) -> String {
    let mut slug = String::new();
    for c in prompt.chars() {
        if slug.len() >= 32 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "agent" } else { slug };
    format!("indra/{}-{:06x}", slug, salt & 0xff_ffff)
}

/// Make a worktree for an agent launched on `target`, a folder in a git
/// repository.
pub async fn create(data_dir: &Path, target: &Path, prompt: &str) -> Result<Created, String> {
    let top = run(target, &["rev-parse", "--show-toplevel"])
        .await
        .map_err(|_| {
            format!(
                "{} is not in a git repository, so the agent cannot get its own branch",
                target.display()
            )
        })?;
    let top = PathBuf::from(top);
    if run(&top, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .await
        .is_err()
    {
        return Err("the repository has no commits yet, so there is nothing to branch from".into());
    }
    let base = match run(&top, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await {
        Ok(b) if !b.is_empty() => b,
        _ => run(&top, &["rev-parse", "HEAD"]).await?,
    };
    let salt = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ u64::from(std::process::id());
    let branch = branch_name(prompt, salt);
    let name = branch.trim_start_matches("indra/").to_string();
    let wroot = root(data_dir);
    std::fs::create_dir_all(&wroot)
        .map_err(|e| format!("cannot create {}: {}", wroot.display(), e))?;
    let dir = wroot.join(&name);
    let dir_s = dir.to_string_lossy().to_string();
    run(&top, &["worktree", "add", "-b", &branch, &dir_s, "HEAD"]).await?;
    let key = format!("branch.{}.indraBase", branch);
    if let Err(e) = run(&top, &["config", &key, &base]).await {
        let _ = remove(&top, &dir, &branch, true).await;
        return Err(e);
    }
    // The same subfolder the agent was launched on, inside the worktree.
    let rel = target
        .canonicalize()
        .ok()
        .and_then(|t| {
            top.canonicalize()
                .ok()
                .and_then(|r| t.strip_prefix(&r).ok().map(Path::to_path_buf))
        })
        .unwrap_or_default();
    let cwd = if rel.as_os_str().is_empty() {
        dir.clone()
    } else {
        dir.join(rel)
    };
    Ok(Created { dir, cwd, branch })
}

/// The worktree (its top folder) that `cwd` is in, when it is one of the
/// agents' worktrees.
async fn worktree_of(data_dir: &Path, cwd: &Path) -> Option<PathBuf> {
    let wroot = root(data_dir).canonicalize().ok()?;
    let c = cwd.canonicalize().ok()?;
    if !c.starts_with(&wroot) {
        return None;
    }
    let top = PathBuf::from(run(&c, &["rev-parse", "--show-toplevel"]).await.ok()?);
    let top = top.canonicalize().ok()?;
    (top.parent() == Some(wroot.as_path())).then_some(top)
}

/// The repository a worktree belongs to: the first (main) worktree git lists.
async fn repo_of(wt: &Path) -> Option<PathBuf> {
    let list = run(wt, &["worktree", "list", "--porcelain"]).await.ok()?;
    list.lines()
        .find_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
}

/// Branch, base and repository of the worktree `cwd` is in, if it is one.
pub async fn info(data_dir: &Path, cwd: &Path) -> Option<Info> {
    let wt = worktree_of(data_dir, cwd).await?;
    let repo = repo_of(&wt).await?;
    let branch = run(&wt, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await
        .ok()?;
    let base = run(&repo, &["config", &format!("branch.{}.indraBase", branch)])
        .await
        .unwrap_or_default();
    let commits = if base.is_empty() {
        0
    } else {
        run(&wt, &["rev-list", "--count", &format!("{}..HEAD", base)])
            .await
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    Some(Info {
        branch,
        base,
        repo: repo.display().to_string(),
        commits,
    })
}

/// Who commits: the repository's own identity when git has one, else a
/// neutral one (as `-c` options before the command).
async fn identity(dir: &Path) -> Vec<&'static str> {
    let named = run(dir, &["config", "user.email"])
        .await
        .map(|e| !e.is_empty())
        .unwrap_or(false);
    if named {
        vec![]
    } else {
        vec!["-c", "user.name=indra", "-c", "user.email=indra@localhost"]
    }
}

/// Commit whatever the agent left uncommitted, so the branch holds it all.
async fn commit_all(wt: &Path, message: &str) -> Result<bool, String> {
    run(wt, &["add", "-A"]).await?;
    if git(wt, &["diff", "--cached", "--quiet"]).await?.ok {
        return Ok(false);
    }
    let mut args = identity(wt).await;
    args.extend(["commit", "--no-verify", "-q", "-m", message]);
    run(wt, &args).await?;
    Ok(true)
}

async fn remove(repo: &Path, wt: &Path, branch: &str, delete_branch: bool) -> Result<(), String> {
    let wt_s = wt.to_string_lossy().to_string();
    run(repo, &["worktree", "remove", "--force", &wt_s]).await?;
    if delete_branch {
        run(repo, &["branch", "-D", branch]).await?;
        let _ = run(
            repo,
            &["config", "--remove-section", &format!("branch.{}", branch)],
        )
        .await;
    }
    Ok(())
}

/// Finish an agent's branch: `merge` it into its base (then remove it),
/// `keep` the branch (remove only the worktree), or `discard` both.
/// `message` names the commit of anything left uncommitted.
pub async fn finish(
    data_dir: &Path,
    cwd: &Path,
    action: &str,
    message: &str,
) -> Result<String, String> {
    let wt = worktree_of(data_dir, cwd)
        .await
        .ok_or("this agent does not work on its own branch (or it was already finished)")?;
    let repo = repo_of(&wt)
        .await
        .ok_or("cannot find the worktree's repository")?;
    let i = info(data_dir, cwd)
        .await
        .ok_or("cannot read the worktree's branch")?;
    match action {
        "discard" => {
            remove(&repo, &wt, &i.branch, true).await?;
            Ok(format!("Discarded {} and its changes", i.branch))
        }
        "keep" => {
            commit_all(&wt, message).await?;
            remove(&repo, &wt, &i.branch, false).await?;
            Ok(format!("Kept the branch {} in {}", i.branch, i.repo))
        }
        "merge" => {
            if i.base.is_empty() {
                return Err(
                    "its base branch is not known; keep the branch and merge it yourself".into(),
                );
            }
            let current = run(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
                .await
                .unwrap_or_default();
            if current != i.base {
                return Err(format!(
                    "{} has {} checked out, not {}: check out {} there, or keep the branch and merge it yourself",
                    i.repo,
                    if current.is_empty() { "a detached HEAD".to_string() } else { current },
                    i.base,
                    i.base
                ));
            }
            commit_all(&wt, message).await?;
            let merge_msg = format!("Merge {} (indra)", i.branch);
            let mut args = identity(&repo).await;
            args.extend(["merge", "--no-ff", "--no-edit", "-m", &merge_msg, &i.branch]);
            let m = git(&repo, &args).await?;
            if !m.ok {
                let _ = git(&repo, &["merge", "--abort"]).await;
                let why = if m.stderr.is_empty() {
                    m.stdout
                } else {
                    m.stderr
                };
                return Err(format!(
                    "the merge did not go through ({}). Nothing changed in {}; the branch {} stays for you to merge by hand",
                    first_lines(&why, 3),
                    i.base,
                    i.branch
                ));
            }
            remove(&repo, &wt, &i.branch, true).await?;
            Ok(format!("Merged {} into {}", i.branch, i.base))
        }
        _ => Err("the action is merge, keep or discard".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_are_safe_and_short() {
        assert_eq!(
            branch_name("Fix the flaky auth tests!", 0xabc),
            "indra/fix-the-flaky-auth-tests-000abc"
        );
        assert_eq!(branch_name("  --  ", 1), "indra/agent-000001");
        let long = branch_name(&"x".repeat(200), 0xffff_ffff);
        assert!(long.len() <= "indra/".len() + 32 + 7, "{}", long);
        assert!(!branch_name("../../etc; rm -rf /", 2).contains(".."));
    }

    fn sh(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {:?}", args);
    }

    fn repo(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("indra-wt-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let data = base.join("data");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        // The machine's line-ending setting (core.autocrlf on Windows
        // runners) would rewrite the files these tests compare byte for byte.
        sh(&repo, &["config", "core.autocrlf", "false"]);
        std::fs::write(repo.join("sub/a.txt"), "one\n").unwrap();
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-q", "-m", "first"]);
        (repo, data)
    }

    /// The agent works on its own branch, in the same subfolder; its work
    /// merges back into the branch it came from, and the worktree goes.
    #[tokio::test]
    async fn an_agents_branch_is_made_then_merged() {
        let (repo, data) = repo("merge");
        let c = create(&data, &repo.join("sub"), "add a line")
            .await
            .unwrap();
        assert!(c.branch.starts_with("indra/add-a-line-"));
        assert!(c.cwd.ends_with("sub") && c.cwd.starts_with(root(&data)));
        std::fs::write(c.cwd.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(c.cwd.join("b.txt"), "new\n").unwrap();
        let i = info(&data, &c.cwd).await.unwrap();
        assert_eq!((i.base.as_str(), i.commits), ("main", 0));
        // The repository itself is untouched until the merge.
        assert_eq!(
            std::fs::read_to_string(repo.join("sub/a.txt")).unwrap(),
            "one\n"
        );
        let said = finish(&data, &c.cwd, "merge", "indra: add a line")
            .await
            .unwrap();
        assert!(said.contains("Merged"), "{}", said);
        assert_eq!(
            std::fs::read_to_string(repo.join("sub/a.txt")).unwrap(),
            "one\ntwo\n"
        );
        assert!(repo.join("sub/b.txt").exists());
        assert!(!c.dir.exists(), "the worktree is removed");
        assert!(info(&data, &c.cwd).await.is_none());
    }

    #[tokio::test]
    async fn a_branch_can_be_kept_or_discarded_and_others_are_refused() {
        let (repo, data) = repo("keep");
        let c = create(&data, &repo, "keep me").await.unwrap();
        std::fs::write(c.cwd.join("sub/k.txt"), "k\n").unwrap();
        finish(&data, &c.cwd, "keep", "indra: keep me")
            .await
            .unwrap();
        assert!(!c.dir.exists());
        let branches = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["branch", "--list", &c.branch])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&branches.stdout).contains(&c.branch),
            "the branch stays"
        );
        assert!(!repo.join("sub/k.txt").exists(), "kept, not merged");

        let d = create(&data, &repo, "throw away").await.unwrap();
        std::fs::write(d.cwd.join("junk.txt"), "x\n").unwrap();
        finish(&data, &d.cwd, "discard", "x").await.unwrap();
        assert!(!d.dir.exists() && !repo.join("junk.txt").exists());

        // Only agents' worktrees: the repository itself is not one.
        assert!(info(&data, &repo).await.is_none());
        assert!(finish(&data, &repo, "discard", "x").await.is_err());
    }

    /// A merge that conflicts changes nothing and keeps the branch.
    #[tokio::test]
    async fn a_conflicting_merge_is_undone() {
        let (repo, data) = repo("conflict");
        let c = create(&data, &repo, "edit a").await.unwrap();
        std::fs::write(c.cwd.join("sub/a.txt"), "agent\n").unwrap();
        std::fs::write(repo.join("sub/a.txt"), "human\n").unwrap();
        sh(&repo, &["commit", "-qam", "human edit"]);
        let err = finish(&data, &c.cwd, "merge", "indra: edit a")
            .await
            .unwrap_err();
        assert!(err.contains("did not go through"), "{}", err);
        assert_eq!(
            std::fs::read_to_string(repo.join("sub/a.txt")).unwrap(),
            "human\n"
        );
        assert!(c.dir.exists(), "the worktree stays to be merged by hand");
        assert!(
            !repo.join(".git/MERGE_HEAD").exists(),
            "no merge left half done"
        );
    }

    #[tokio::test]
    async fn a_folder_outside_git_cannot_get_a_branch() {
        let base = std::env::temp_dir().join(format!("indra-wt-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let err = create(&base.join(".."), &base, "x").await;
        assert!(err.is_err());
    }
}
