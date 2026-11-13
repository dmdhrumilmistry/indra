//! Editing `indra.toml` from the console.
//!
//! The console edits one top-level section at a time (`[[runner]]`,
//! `[[node]]`, `[local]`, ...) or the whole text. A section edit rewrites only
//! that section: comments, ordering and formatting everywhere else in the file
//! are kept (comments inside the replaced section are not). Every edit is
//! checked by loading the result exactly as `indra serve` would, so a change
//! that would not load is refused before anything is written.

use crate::deck::config::{
    ConfigFile, HarnessCfg, LocalCfg, NodeCfg, PresetCfg, ProfileCfg, Result, RunnerCfg, ServerCfg,
    WorkflowCfg,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use toml_edit::{DocumentMut, Item};

/// The top-level keys of `indra.toml`, in the order a new file gets them.
pub const SECTIONS: [&str; 10] = [
    "builtins",
    "server",
    "local",
    "harness",
    "runner",
    "profile",
    "preset",
    "node",
    "workflow",
    "connector",
];

/// The first lines of a config file the console creates.
const NEW_FILE: &str =
    "# indra configuration (see `indra init` for every option, with comments).\n\
# Written by the indra console; edits by hand are kept.\n";

/// A short fingerprint of the file, so a save can tell that the file changed
/// since the browser loaded it (FNV-1a, 64 bit).
pub fn version(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:016x}", h)
}

/// Read a config file; a missing file reads as empty.
pub fn read(path: &Path) -> Result<(String, bool)> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok((t, true)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((String::new(), false)),
        Err(e) => Err(format!("cannot read {}: {}", path.display(), e)),
    }
}

/// Parse a config text the way `indra serve` does.
pub fn parse(text: &str) -> Result<ConfigFile> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// `value` as TOML for `section`, after checking it has the right shape.
/// Returns the normalized value too, to compare with what the file reads back as.
fn typed<T: Serialize + DeserializeOwned>(section: &str, value: &Value) -> Result<(String, Value)> {
    let v: T = serde_json::from_value(value.clone()).map_err(|e| format!("{}: {}", section, e))?;
    let normal = serde_json::to_value(&v).map_err(|e| e.to_string())?;
    let mut m = BTreeMap::new();
    m.insert(section, v);
    let text = toml::to_string(&m).map_err(|e| format!("{}: {}", section, e))?;
    Ok((text, normal))
}

fn fragment(section: &str, value: &Value) -> Result<Option<(String, Value)>> {
    if value.is_null() {
        return Ok(None);
    }
    let r = match section {
        "builtins" => typed::<bool>(section, value),
        "server" => typed::<ServerCfg>(section, value),
        "local" => typed::<LocalCfg>(section, value),
        "harness" => typed::<Vec<HarnessCfg>>(section, value),
        "runner" => typed::<Vec<RunnerCfg>>(section, value),
        "preset" => typed::<Vec<PresetCfg>>(section, value),
        "profile" => typed::<Vec<ProfileCfg>>(section, value),
        "connector" => typed::<Vec<crate::deck::config::ConnectorCfg>>(section, value),
        "node" => typed::<Vec<NodeCfg>>(section, value),
        "workflow" => typed::<Vec<WorkflowCfg>>(section, value),
        _ => return Err(format!("unknown section '{}'", section)),
    }?;
    Ok(Some(r))
}

/// Tables built elsewhere carry positions from that document; without one, a
/// table is laid out right after the table before it, so the section stays
/// where it was in the file.
fn clear_positions(item: &mut Item) {
    match item {
        Item::Table(t) => {
            t.set_position(None);
            for (_, v) in t.iter_mut() {
                clear_positions(v);
            }
        }
        Item::ArrayOfTables(a) => {
            for t in a.iter_mut() {
                t.set_position(None);
                for (_, v) in t.iter_mut() {
                    clear_positions(v);
                }
            }
        }
        _ => {}
    }
}

/// The comments and blank lines above a section's first table header.
fn lead(item: &Item) -> Option<String> {
    let t = match item {
        Item::Table(t) => Some(t),
        Item::ArrayOfTables(a) => a.get(0),
        _ => None,
    }?;
    t.decor()
        .prefix()
        .and_then(|p| p.as_str())
        .map(str::to_string)
}

fn set_lead(item: &mut Item, prefix: &str) {
    let t = match item {
        Item::Table(t) => Some(t),
        Item::ArrayOfTables(a) => a.get_mut(0),
        _ => None,
    };
    if let Some(t) = t {
        t.decor_mut().set_prefix(prefix);
    }
}

fn is_empty(item: &Item) -> bool {
    match item {
        Item::None => true,
        Item::Table(t) => t.is_empty(),
        Item::ArrayOfTables(a) => a.is_empty(),
        Item::Value(v) => v.as_array().map(|a| a.is_empty()).unwrap_or(false),
    }
}

fn section_of(cf: &ConfigFile, section: &str) -> Value {
    serde_json::to_value(cf)
        .ok()
        .and_then(|v| v.get(section).cloned())
        .unwrap_or(Value::Null)
}

/// `text` with one section replaced by `value` (JSON, the shape of that
/// section in [`ConfigFile`]). `null`, an empty list or an empty table
/// removes the section.
pub fn set_section(text: &str, section: &str, value: &Value) -> Result<String> {
    if !SECTIONS.contains(&section) {
        return Err(format!("unknown section '{}'", section));
    }
    let mut doc: DocumentMut = text.parse().map_err(|e| {
        format!(
            "the config file is not valid TOML, so fix it as text first: {}",
            e
        )
    })?;
    let new = match fragment(section, value)? {
        None => None,
        Some((frag, normal)) => {
            let mut f: DocumentMut = frag.parse().map_err(|e| format!("{}: {}", section, e))?;
            let mut item = f.remove(section).unwrap_or(Item::None);
            clear_positions(&mut item);
            // Keep the comment that heads the section, or start it on a new line.
            let prefix = doc
                .get(section)
                .and_then(lead)
                .unwrap_or_else(|| "\n".into());
            set_lead(&mut item, &prefix);
            (!is_empty(&item)).then_some((item, normal))
        }
    };
    let expect = match new {
        None => {
            doc.remove(section);
            None
        }
        Some((item, normal)) => {
            doc.insert(section, item);
            Some(normal)
        }
    };
    let mut out = doc.to_string();
    if text.trim().is_empty() {
        out = format!("{}\n{}", NEW_FILE, out.trim_start());
    }
    // Belt and braces: the file must read back with exactly this section.
    let back = parse(&out).map_err(|e| format!("the edit did not produce a valid file: {}", e))?;
    let got = section_of(&back, section);
    let empty_default = section_of(&ConfigFile::default(), section);
    let want = expect.unwrap_or(empty_default);
    if got != want {
        return Err(format!(
            "the edit to '{}' did not read back as written",
            section
        ));
    }
    Ok(out)
}

/// Write `text` to `path` so that a crash never leaves half a file: a
/// temporary file next to it, then a rename. Keeps the file's permissions.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "indra.toml".into());
    let tmp = dir.join(format!(".{}.{}.tmp", name, std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {}", tmp.display(), e))?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {}", path.display(), e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FILE: &str = "# my settings\nbuiltins = true\n\n[local]\nmax_parallel = 3 # laptop\n\n\
# the build box\n[[node]]\nname = \"box\"\nssh = \"me@box\"\n\n# runners\n[[runner]]\nname = \"a\"\ncommand = \"echo\"\nargs = [\"{prompt}\"]\n\n\
# keep this comment\n[[workflow]]\nname = \"w\"\n[[workflow.task]]\nid = \"t\"\ntarget = \".\"\nprompt = \"x\"\n";

    #[test]
    fn a_section_edit_keeps_the_rest_of_the_file() {
        let out = set_section(
            FILE,
            "runner",
            &json!([{ "name": "a", "command": "claude", "args": ["-p", "{prompt}"] }, { "name": "b", "command": "codex", "args": ["exec", "{prompt}"] }]),
        )
        .unwrap();
        for kept in [
            "# my settings",
            "max_parallel = 3 # laptop",
            "# the build box",
            "# keep this comment",
        ] {
            assert!(out.contains(kept), "lost {:?}:\n{}", kept, out);
        }
        let cf = parse(&out).unwrap();
        assert_eq!(cf.runner.len(), 2);
        assert_eq!(cf.runner[0].command, "claude");
        assert_eq!(cf.workflow[0].tasks.len(), 1, "the workflow is untouched");
        // The runners stay between the node and the workflow.
        let (n, r, w) = (
            out.find("[[node]]").unwrap(),
            out.find("[[runner]]").unwrap(),
            out.find("[[workflow]]").unwrap(),
        );
        assert!(n < r && r < w, "{}", out);
        assert!(
            !out.contains("resume_args"),
            "empty lists are left out:\n{}",
            out
        );
    }

    #[test]
    fn nested_tables_and_removal() {
        let out = set_section(
            FILE,
            "workflow",
            &json!([{ "name": "w2", "task": [
                { "id": "a", "target": "/srv", "prompt": "p", "node": "box" },
                { "id": "b", "target": ".", "prompt": "q", "needs": ["a"], "retries": 2 }
            ] }]),
        )
        .unwrap();
        let cf = parse(&out).unwrap();
        assert_eq!(cf.workflow[0].name, "w2");
        assert_eq!(cf.workflow[0].tasks[1].needs, vec!["a"]);
        assert_eq!(cf.workflow[0].tasks[1].retries, 2);
        let out = set_section(&out, "node", &json!([])).unwrap();
        assert!(!out.contains("[[node]]"), "{}", out);
        let out = set_section(&out, "builtins", &Value::Null).unwrap();
        assert!(!out.contains("builtins"), "{}", out);
        let out = set_section(&out, "local", &json!({})).unwrap();
        assert!(!out.contains("[local]"), "{}", out);
    }

    #[test]
    fn bad_values_are_refused() {
        let e = set_section(FILE, "runner", &json!([{ "name": "a" }])).unwrap_err();
        assert!(e.contains("command"), "{}", e);
        let e = set_section(
            FILE,
            "runner",
            &json!([{ "name": "a", "command": "x", "shell": true }]),
        )
        .unwrap_err();
        assert!(e.contains("shell"), "{}", e);
        assert!(set_section(FILE, "secrets", &json!(1)).is_err());
        assert!(set_section("[[runner]\n", "local", &json!({})).is_err());
    }

    #[test]
    fn a_new_file_gets_a_header() {
        let out = set_section("", "local", &json!({ "max_parallel": 6 })).unwrap();
        assert!(out.starts_with("# indra configuration"), "{}", out);
        assert_eq!(parse(&out).unwrap().local.max_parallel, Some(6));
    }

    #[test]
    fn versions_change_with_the_text() {
        assert_eq!(version("a"), version("a"));
        assert_ne!(version("a"), version("b"));
    }
}
