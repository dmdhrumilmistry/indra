//! `indra skills`: keep the skill mirrors in step with the canonical skills,
//! and install them where other agents look for skills.
//!
//! The canonical copy is `plugins/indra/skills/` (fixed by the Claude Code
//! plugin format). `skills/` at the repository root is a generated mirror:
//! Gemini CLI loads an extension's skills from `<extension root>/skills`, and
//! the extension root is the repository root.

use clap::{Args, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Args)]
pub struct SkillsArgs {
    /// The repository (default: found from this folder up)
    #[arg(long, global = true)]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    cmd: SkillsCmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Agent {
    /// ~/.agents/skills: the agentskills.io path (opencode, Gemini CLI, Codex)
    Agents,
    /// ~/.claude/skills: Claude Code user skills
    Claude,
    /// ~/.config/opencode/skills
    Opencode,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Scope {
    User,
    Workspace,
}

#[derive(Subcommand)]
enum SkillsCmd {
    /// Refresh the skills/ mirror from plugins/indra/skills/ (or --check it)
    Sync {
        /// Exit non-zero if the mirror is stale (for CI)
        #[arg(long)]
        check: bool,
    },
    /// Copy the skills into a folder an agent discovers
    Install {
        #[arg(long, value_enum)]
        to: Agent,
        /// Into your home folder, or this workspace
        #[arg(long, value_enum, default_value = "user")]
        scope: Scope,
    },
}

fn repo_root(given: &Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(r) = given {
        return Ok(r.clone());
    }
    let mut d = std::env::current_dir().ok();
    while let Some(x) = d {
        if x.join("plugins").join("indra").join("skills").is_dir() {
            return Ok(x);
        }
        d = x.parent().map(Path::to_path_buf);
    }
    Err(
        "no plugins/indra/skills here or above; run inside the indra repository or pass --repo"
            .into(),
    )
}

/// Every file under `root`, relative and sorted.
fn tree_files(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if e.file_name() != "__pycache__" {
                    walk(root, &p, out);
                }
            } else if let Ok(r) = p.strip_prefix(root) {
                out.push(r.to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn copy_tree(src: &Path, dst: &Path) -> Result<usize, String> {
    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(|e| format!("{}: {}", dst.display(), e))?;
    }
    let files = tree_files(src);
    for rel in &files {
        let to = dst.join(rel);
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        std::fs::copy(src.join(rel), &to).map_err(|e| format!("{}: {}", to.display(), e))?;
        // Bundled scripts keep their exec bit whoever regenerates the mirror.
        #[cfg(unix)]
        if rel.extension().map(|x| x == "sh").unwrap_or(false) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755));
        }
    }
    Ok(files.len())
}

fn same(a: &Path, b: &Path) -> bool {
    matches!((std::fs::read(a), std::fs::read(b)), (Ok(x), Ok(y)) if x == y)
}

pub fn run(a: SkillsArgs) -> Result<ExitCode, String> {
    let repo = repo_root(&a.repo)?;
    let source = repo.join("plugins").join("indra").join("skills");
    let mirror = repo.join("skills");
    if !source.is_dir() {
        eprintln!("error: canonical skills not found at {}", source.display());
        return Ok(ExitCode::from(2));
    }
    match a.cmd {
        SkillsCmd::Sync { check: true } => {
            let src = tree_files(&source);
            let mut problems = Vec::new();
            if !mirror.is_dir() {
                problems.push(format!("mirror does not exist: {}", mirror.display()));
            } else {
                let dst = tree_files(&mirror);
                for m in src.iter().filter(|f| !dst.contains(f)) {
                    problems.push(format!("missing from mirror: {}", m.display()));
                }
                for x in dst.iter().filter(|f| !src.contains(f)) {
                    problems.push(format!("stale in mirror: {}", x.display()));
                }
                for f in src.iter().filter(|f| dst.contains(f)) {
                    if !same(&source.join(f), &mirror.join(f)) {
                        problems.push(format!("differs: {}", f.display()));
                    }
                }
            }
            if problems.is_empty() {
                println!("skills/ mirror is up to date ({} files).", src.len());
                return Ok(ExitCode::SUCCESS);
            }
            eprintln!("skills/ mirror is STALE. Run: indra skills sync");
            for p in problems {
                eprintln!("  {}", p);
            }
            Ok(ExitCode::from(1))
        }
        SkillsCmd::Sync { check: false } => {
            let n = copy_tree(&source, &mirror)?;
            println!("Synced {} files -> skills", n);
            Ok(ExitCode::SUCCESS)
        }
        SkillsCmd::Install { to, scope } => {
            let home = crate::paths::home().ok_or("no home folder")?;
            let here = std::env::current_dir().map_err(|e| e.to_string())?;
            let user = scope == Scope::User;
            let dest = match to {
                Agent::Agents => if user {
                    home.join(".agents")
                } else {
                    here.join(".agents")
                }
                .join("skills"),
                Agent::Claude => if user {
                    home.join(".claude")
                } else {
                    here.join(".claude")
                }
                .join("skills"),
                Agent::Opencode => {
                    if user {
                        home.join(".config").join("opencode").join("skills")
                    } else {
                        here.join(".opencode").join("skills")
                    }
                }
            };
            std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            let mut names: Vec<_> = std::fs::read_dir(&source)
                .map_err(|e| e.to_string())?
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name())
                .collect();
            names.sort();
            for n in &names {
                copy_tree(&source.join(n), &dest.join(n))?;
            }
            println!("Installed {} skills -> {}", names.len(), dest.display());
            Ok(ExitCode::SUCCESS)
        }
    }
}
