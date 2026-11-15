//! Keep `indra node connect` running on a machine that joined a hub, and
//! start it again after a log-in or reboot, with what each OS has for that:
//!
//! - Linux: a systemd user service (`indra-node.service`); without systemd,
//!   a background process until the next reboot.
//! - macOS: a launchd agent (`~/Library/LaunchAgents/dev.indra.node.plist`).
//! - Windows: a background process now, and the same at every log-in (the
//!   `indra-node` value under HKCU\...\Run). No administrator needed anywhere.
//!
//! These restart the link when it fails, not when it ends on purpose: a
//! machine removed on the hub exits with success and stays stopped.
//!
//! The link's PATH is the one `indra join` ran with, so the agents' CLIs it
//! found then are found later too. Output goes to `logs/node.log` under
//! indra's data folder (on Linux with systemd: `journalctl --user -u indra-node`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const UNIT: &str = "indra-node.service";
const PLIST: &str = "dev.indra.node";
const RUN_VALUE: &str = "indra-node";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

pub fn log_path() -> PathBuf {
    crate::paths::data_dir().join("logs").join("node.log")
}

fn pid_path() -> PathBuf {
    crate::paths::data_dir().join("node.pid")
}

/// What runs: this binary, `node connect`, and the config it was given.
fn command(config: Option<&Path>) -> Result<(PathBuf, Vec<String>), String> {
    let exe = std::env::current_exe().map_err(|e| format!("where is indra? {}", e))?;
    let mut args = vec!["node".to_string(), "connect".to_string()];
    if let Some(c) = config {
        args.push("--config".into());
        args.push(c.display().to_string());
    }
    Ok((exe, args))
}

fn quiet(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Start `node connect` now, detached, its output in the log; remembers the
/// process so a later join or leave can stop it.
pub fn spawn_background(config: Option<&Path>) -> Result<u32, String> {
    let (exe, args) = command(config)?;
    let log = log_path();
    if let Some(d) = log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // Keep the log small: start over past 5 MB.
    if std::fs::metadata(&log)
        .map(|m| m.len() > 5 << 20)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&log);
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("{}: {}", log.display(), e))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.args(&args).stdin(Stdio::null()).stdout(out).stderr(err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0008); // no window, detached
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot start indra node connect: {}", e))?;
    let pid = child.id();
    let _ = std::fs::write(pid_path(), pid.to_string());
    Ok(pid)
}

/// Stop a background `node connect` started by [`spawn_background`].
fn stop_background() {
    let Ok(t) = std::fs::read_to_string(pid_path()) else {
        return;
    };
    let _ = std::fs::remove_file(pid_path());
    let Ok(pid) = t.trim().parse::<u32>() else {
        return;
    };
    if pid == std::process::id() {
        return;
    }
    #[cfg(unix)]
    {
        // Only if it is still indra (the number may have been reused).
        let ours = std::fs::read_to_string(format!("/proc/{}/cmdline", pid))
            .map(|c| c.contains("indra") && c.contains("connect"))
            .unwrap_or(cfg!(target_os = "macos"));
        if ours {
            // SAFETY: plain kill(2) on a process id.
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }
    #[cfg(windows)]
    {
        let filter = format!("PID eq {}", pid);
        let listed = Command::new("tasklist")
            .args(["/FI", &filter, "/NH"])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .to_lowercase()
                    .contains("indra")
            })
            .unwrap_or(false);
        if listed {
            let _ = quiet(Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]));
        }
    }
}

fn systemd_unit_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::paths::home().map(|h| h.join(".config")))?;
    Some(base.join("systemd").join("user").join(UNIT))
}

fn launchd_plist_path() -> Option<PathBuf> {
    crate::paths::home().map(|h| {
        h.join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", PLIST))
    })
}

/// systemd quoting for one word of ExecStart.
fn sd_quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn systemd_unit(exe: &Path, args: &[String], path_env: &str) -> String {
    let mut exec = sd_quote(&exe.display().to_string());
    for a in args {
        exec.push(' ');
        exec.push_str(&sd_quote(a));
    }
    format!(
        "# Written by `indra join`; `indra node leave` removes it.\n\
         [Unit]\n\
         Description=indra node: runs agent jobs for its hub\n\
         After=network-online.target\n\
         Wants=network-online.target\n\n\
         [Service]\n\
         ExecStart={}\n\
         Environment={}\n\
         Restart=on-failure\n\
         RestartSec=5\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        exec,
        sd_quote(&format!("PATH={}", path_env))
    )
}

pub fn launchd_plist(exe: &Path, args: &[String], path_env: &str, log: &Path) -> String {
    let mut prog = format!("    <string>{}</string>\n", xml(&exe.display().to_string()));
    for a in args {
        prog.push_str(&format!("    <string>{}</string>\n", xml(a)));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<!-- Written by `indra join`; `indra node leave` removes it. -->\n\
<plist version=\"1.0\">\n<dict>\n\
  <key>Label</key><string>{}</string>\n\
  <key>ProgramArguments</key>\n  <array>\n{}  </array>\n\
  <key>EnvironmentVariables</key>\n  <dict><key>PATH</key><string>{}</string></dict>\n\
  <key>RunAtLoad</key><true/>\n\
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n\
  <key>StandardOutPath</key><string>{}</string>\n\
  <key>StandardErrorPath</key><string>{}</string>\n\
</dict>\n</plist>\n",
        PLIST,
        prog,
        xml(path_env),
        xml(&log.display().to_string()),
        xml(&log.display().to_string())
    )
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {}", d.display(), e))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {}", path.display(), e))
}

/// Keep the link running from now on. Says what it set up; an error means
/// nothing will keep it running (the caller can run it in the foreground).
pub fn install(config: Option<&Path>) -> Result<String, String> {
    let (exe, args) = command(config)?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    // One link at a time: stop one started by an earlier join.
    stop_background();
    if cfg!(target_os = "linux") {
        let systemd = quiet(Command::new("systemctl").args(["--user", "show-environment"]));
        if systemd {
            let unit = systemd_unit_path().ok_or("no home folder")?;
            write(&unit, &systemd_unit(&exe, &args, &path_env))?;
            let ok = quiet(Command::new("systemctl").args(["--user", "daemon-reload"]))
                && quiet(Command::new("systemctl").args(["--user", "enable", UNIT]))
                && quiet(Command::new("systemctl").args(["--user", "restart", UNIT]));
            if ok {
                // Also before anyone logs in (servers); fine if not allowed.
                let user = std::env::var("USER").unwrap_or_default();
                let linger = !user.is_empty()
                    && quiet(Command::new("loginctl").args(["enable-linger", &user]));
                return Ok(format!(
                    "a systemd user service ({}){}; logs: journalctl --user -u indra-node",
                    UNIT,
                    if linger {
                        ", which also starts at boot"
                    } else {
                        ", which starts when you log in"
                    }
                ));
            }
        }
        spawn_background(config)?;
        return Ok(format!(
            "a background process (no systemd here, so after a reboot run `indra node connect` again); logs: {}",
            log_path().display()
        ));
    }
    if cfg!(target_os = "macos") {
        let plist = launchd_plist_path().ok_or("no home folder")?;
        let log = log_path();
        if let Some(d) = log.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let p = plist.display().to_string();
        let _ = quiet(Command::new("launchctl").args(["unload", &p]));
        write(&plist, &launchd_plist(&exe, &args, &path_env, &log))?;
        if quiet(Command::new("launchctl").args(["load", "-w", &p])) {
            return Ok(format!(
                "a launchd agent ({}), which starts when you log in; logs: {}",
                PLIST,
                log.display()
            ));
        }
        let _ = std::fs::remove_file(&plist);
        spawn_background(config)?;
        return Ok(format!(
            "a background process (launchd refused the agent, so after a reboot run `indra node connect` again); logs: {}",
            log.display()
        ));
    }
    if cfg!(windows) {
        let mut line = format!("\"{}\"", exe.display());
        for a in &args {
            line.push_str(&format!(" \"{}\"", a));
        }
        line.push_str(" --background");
        let at_login = quiet(Command::new("reg").args([
            "add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &line, "/f",
        ]));
        spawn_background(config)?;
        return Ok(format!(
            "a background process{}; logs: {}",
            if at_login {
                ", started again when you sign in to Windows"
            } else {
                " (it could not be added to sign-in, so after a reboot run `indra node connect` again)"
            },
            log_path().display()
        ));
    }
    spawn_background(config)?;
    Ok(format!(
        "a background process until the next reboot; logs: {}",
        log_path().display()
    ))
}

/// Undo [`install`] and stop the link.
pub fn uninstall() -> Vec<String> {
    let mut done = Vec::new();
    if let Some(unit) = systemd_unit_path().filter(|p| p.exists()) {
        let _ = quiet(Command::new("systemctl").args(["--user", "disable", "--now", UNIT]));
        let _ = std::fs::remove_file(&unit);
        let _ = quiet(Command::new("systemctl").args(["--user", "daemon-reload"]));
        done.push(format!("removed the systemd user service {}", UNIT));
    }
    if let Some(plist) = launchd_plist_path().filter(|p| p.exists()) {
        let _ =
            quiet(Command::new("launchctl").args(["unload", "-w", &plist.display().to_string()]));
        let _ = std::fs::remove_file(&plist);
        done.push(format!("removed the launchd agent {}", PLIST));
    }
    if cfg!(windows) && quiet(Command::new("reg").args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]))
    {
        done.push("removed it from Windows sign-in".into());
    }
    if pid_path().exists() {
        stop_background();
        done.push("stopped the background link".into());
    }
    done
}

/// How the link is kept running here, if it is.
pub fn status() -> Option<String> {
    if systemd_unit_path().map(|p| p.exists()).unwrap_or(false) {
        let active = Command::new("systemctl")
            .args(["--user", "is-active", UNIT])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".into());
        return Some(format!("systemd user service {} ({})", UNIT, active));
    }
    if launchd_plist_path().map(|p| p.exists()).unwrap_or(false) {
        return Some(format!("launchd agent {}", PLIST));
    }
    if cfg!(windows) && quiet(Command::new("reg").args(["query", RUN_KEY, "/v", RUN_VALUE])) {
        return Some("starts when you sign in to Windows".into());
    }
    if pid_path().exists() {
        return Some("a background process (until the next reboot)".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_files_quote_paths_and_keep_path() {
        let args = vec!["node".to_string(), "connect".to_string()];
        let u = systemd_unit(
            Path::new("/home/me/my bin/indra"),
            &args,
            "/home/me/.local/bin:/usr/bin",
        );
        assert!(u.contains("ExecStart=\"/home/me/my bin/indra\" \"node\" \"connect\""));
        assert!(u.contains("Environment=\"PATH=/home/me/.local/bin:/usr/bin\""));
        assert!(u.contains("Restart=on-failure"));
        let p = launchd_plist(
            Path::new("/Users/me/indra"),
            &args,
            "/opt/homebrew/bin:/usr/bin",
            Path::new("/Users/me/Library/Application Support/indra/logs/node.log"),
        );
        assert!(p.contains("<string>/Users/me/indra</string>\n    <string>node</string>"));
        assert!(p.contains("<key>SuccessfulExit</key><false/>"));
        assert!(p.contains("Application Support/indra/logs/node.log"));
        assert_eq!(xml("a<&>\"b"), "a&lt;&amp;&gt;&quot;b");
        assert_eq!(sd_quote("50%\"x\""), "\"50%%\\\"x\\\"\"");
    }
}
