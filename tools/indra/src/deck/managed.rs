//! Settings an organization deploys to its machines with an MDM (Jamf,
//! Microsoft Intune, Kandji, Workspace ONE, or any tool that can place a
//! file), next to each user's own `indra.toml`:
//!
//! - `defaults.toml`: starting values. The user's file wins where it says
//!   something else.
//! - `managed.toml`: enforced values. They win over the user's file, and the
//!   console says which settings they cover.
//!
//! Both are ordinary `indra.toml` files (any section), in a folder only an
//! administrator can write:
//!
//! | OS | Folder |
//! |---|---|
//! | macOS | `/Library/Application Support/indra` |
//! | Linux | `/etc/indra` |
//! | Windows | `%ProgramData%\indra` (usually `C:\ProgramData\indra`) |
//!
//! `INDRA_SYSTEM_DIR` points elsewhere (tests, unusual layouts).
//!
//! How they combine: tables merge key by key; lists of named entries
//! (`[[runner]]`, `[[profile]]`, `[[node]]`, `[[account]]`, ...) merge by
//! `name`, an entry from the higher layer replacing one with the same name;
//! anything else is replaced by the higher layer.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use toml::Table;

pub const DEFAULTS: &str = "defaults.toml";
pub const MANAGED: &str = "managed.toml";

/// Where the organization's files live on this machine.
pub fn system_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("INDRA_SYSTEM_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(d);
    }
    if cfg!(windows) {
        let base = std::env::var_os("ProgramData")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        return base.join("indra");
    }
    if cfg!(target_os = "macos") {
        return PathBuf::from("/Library/Application Support/indra");
    }
    PathBuf::from("/etc/indra")
}

fn read(path: &Path) -> Result<Option<Table>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => t
            .parse::<Table>()
            .map(Some)
            .map_err(|e| format!("{}: {}", path.display(), e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {}", path.display(), e)),
    }
}

/// The organization's two layers, if there are any.
pub struct Layers {
    pub defaults: Option<Table>,
    pub managed: Option<Table>,
}

pub fn layers() -> Result<Layers, String> {
    let dir = system_dir();
    Ok(Layers {
        defaults: read(&dir.join(DEFAULTS))?,
        managed: read(&dir.join(MANAGED))?,
    })
}

/// The user's settings with the organization's around them: defaults under,
/// managed over.
pub fn apply(user: Table, l: &Layers) -> Table {
    let mut out = l.defaults.clone().unwrap_or_default();
    merge(&mut out, user);
    if let Some(m) = &l.managed {
        merge(&mut out, m.clone());
    }
    out
}

/// `top` over `base` (see the module docs for the rules).
pub fn merge(base: &mut Table, top: Table) {
    for (k, v) in top {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(t)) => merge(b, t),
            (Some(toml::Value::Array(b)), toml::Value::Array(t)) if named(b) && named(&t) => {
                for item in t {
                    let name = name_of(&item);
                    match b.iter_mut().find(|x| name_of(x) == name) {
                        Some(slot) => *slot = item,
                        None => b.push(item),
                    }
                }
            }
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

fn name_of(v: &toml::Value) -> Option<&str> {
    v.as_table()?.get("name")?.as_str()
}

/// A list of tables that all have a name.
fn named(a: &[toml::Value]) -> bool {
    a.iter().all(|x| name_of(x).is_some())
}

/// The settings `managed.toml` enforces, as paths like `routing`,
/// `local.max_parallel` or `profile.reviewer`, for the console to show.
pub fn enforced(m: &Table) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in m {
        match v {
            toml::Value::Table(t) => {
                for kk in t.keys() {
                    out.push(format!("{}.{}", k, kk));
                }
            }
            toml::Value::Array(a) if named(a) => {
                for x in a {
                    out.push(format!("{}.{}", k, name_of(x).unwrap_or("?")));
                }
            }
            _ => out.push(k.clone()),
        }
    }
    out
}

/// For the console and `indra check`: where the files are, which exist, and
/// what the managed one enforces.
pub fn view() -> Value {
    let dir = system_dir();
    let d = dir.join(DEFAULTS);
    let m = dir.join(MANAGED);
    let (enforced, error) = match read(&m) {
        Ok(Some(t)) => (enforced(&t), None),
        Ok(None) => (vec![], None),
        Err(e) => (vec![], Some(e)),
    };
    json!({
        "dir": dir.display().to_string(),
        "defaults": d.is_file(), "managed": m.is_file(),
        "enforced": enforced, "error": error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Table {
        s.parse().unwrap()
    }

    #[test]
    fn defaults_under_user_under_managed() {
        let l = Layers {
            defaults: Some(t(
                "[local]\nmax_parallel = 2\nshell_steps = true\n[[profile]]\nname = \"reviewer\"\nmodel = \"sonnet\"\n[[profile]]\nname = \"fast\"\nmodel = \"haiku\"\n",
            )),
            managed: Some(t(
                "[routing]\nstrategy = \"in_order\"\n[[profile]]\nname = \"reviewer\"\nmodel = \"opus\"\n[[runner]]\nname = \"claude\"\ncommand = \"claude\"\n",
            )),
        };
        let user = t("[local]\nmax_parallel = 8\n[routing]\nstrategy = \"round_robin\"\n[[profile]]\nname = \"fast\"\nmodel = \"sonnet\"\n[[profile]]\nname = \"mine\"\n");
        let out = apply(user, &l);
        // The user's value over the default, key by key.
        assert_eq!(out["local"]["max_parallel"].as_integer(), Some(8));
        assert_eq!(out["local"]["shell_steps"].as_bool(), Some(true));
        // Managed over the user's.
        assert_eq!(out["routing"]["strategy"].as_str(), Some("in_order"));
        // Named lists merge by name.
        let p = out["profile"].as_array().unwrap();
        let model = |n: &str| {
            p.iter()
                .find(|x| x["name"].as_str() == Some(n))
                .and_then(|x| x.get("model"))
                .and_then(|m| m.as_str())
                .map(String::from)
        };
        assert_eq!(model("reviewer").as_deref(), Some("opus"));
        assert_eq!(model("fast").as_deref(), Some("sonnet"));
        assert!(p.iter().any(|x| x["name"].as_str() == Some("mine")));
        assert_eq!(out["runner"].as_array().unwrap().len(), 1);
        let mut e = enforced(l.managed.as_ref().unwrap());
        e.sort();
        assert_eq!(
            e,
            vec!["profile.reviewer", "routing.strategy", "runner.claude"]
        );
    }

    #[test]
    fn no_files_change_nothing() {
        let user = t("[local]\nmax_parallel = 3\n");
        let out = apply(
            user.clone(),
            &Layers {
                defaults: None,
                managed: None,
            },
        );
        assert_eq!(out, user);
    }
}
