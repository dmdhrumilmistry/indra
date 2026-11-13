//! Per-user locations, the same on every command.
//!
//! Everything indra writes for a user lives under one data directory:
//! `memory/` (the default memory vault) and `logs/` (agent process logs).

use std::path::PathBuf;

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

fn env(k: &str) -> Option<PathBuf> {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `$XDG_DATA_HOME/indra` (or `~/.local/share/indra`), `~/Library/Application
/// Support/indra` on macOS, `%LOCALAPPDATA%\indra` on Windows.
pub fn data_dir() -> PathBuf {
    let h = home().unwrap_or_else(|| PathBuf::from("."));
    if cfg!(windows) {
        return env("LOCALAPPDATA").unwrap_or(h).join("indra");
    }
    if cfg!(target_os = "macos") {
        return h.join("Library").join("Application Support").join("indra");
    }
    env("XDG_DATA_HOME")
        .unwrap_or_else(|| h.join(".local").join("share"))
        .join("indra")
}

/// `$XDG_CONFIG_HOME/indra` (or `~/.config/indra`), `%APPDATA%\indra` on Windows.
pub fn config_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(x) = env("XDG_CONFIG_HOME").or_else(|| home().map(|h| h.join(".config"))) {
        v.push(x.join("indra"));
    }
    if cfg!(windows) {
        if let Some(a) = env("APPDATA") {
            v.push(a.join("indra"));
        }
    }
    v
}
