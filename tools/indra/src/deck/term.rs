//! A terminal in the browser: a real PTY, bridged over a WebSocket.
//!
//! This is a shell on the machine, so it is off unless `indra serve` is
//! started with `--shell`, and only on a loopback address. On top of the
//! console's token and Host checks, the WebSocket must come from the
//! console's own origin (browsers do not apply CORS to WebSockets). Each
//! session is appended to `logs/terminal.log`.
//!
//! What it runs: your shell, or Claude Code, in one of the harness's targets;
//! or an agent's own Claude Code session (`--resume`, branched with
//! `--fork-session` while the agent still runs), to look around and take it
//! somewhere else interactively.

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Terminals open at once, at most.
pub const MAX_OPEN: usize = 8;
pub static OPEN: AtomicUsize = AtomicUsize::new(0);

/// An open terminal, for the list of running terminals and to close one.
struct Live {
    label: String,
    cwd: String,
    user: String,
    started: u64,
    close: Arc<tokio::sync::Notify>,
}

fn live() -> &'static Mutex<std::collections::BTreeMap<u64, Live>> {
    static L: std::sync::OnceLock<Mutex<std::collections::BTreeMap<u64, Live>>> =
        std::sync::OnceLock::new();
    L.get_or_init(Default::default)
}

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The terminals open now: id, what, where, whose, since when.
pub fn sessions() -> Vec<serde_json::Value> {
    live()
        .lock()
        .unwrap()
        .iter()
        .map(|(id, l)| {
            serde_json::json!({ "id": id, "label": l.label, "cwd": l.cwd, "user": l.user, "started": l.started })
        })
        .collect()
}

/// End a terminal (its program and everything it started). False when no
/// terminal has that id.
pub fn close(id: u64) -> bool {
    match live().lock().unwrap().get(&id) {
        Some(l) => {
            l.close.notify_one();
            true
        }
        None => false,
    }
}

/// What to start in the terminal.
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// For the log and the tab title.
    pub label: String,
    /// Extra environment (a profile's, or what puts Claude Code on Ollama).
    pub env: Vec<(String, String)>,
}

/// Whether the locale this process passes on is UTF-8 (LC_ALL wins, then
/// LC_CTYPE, then LANG, as the C library decides).
fn utf8_locale() -> bool {
    let v = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default()
        .to_lowercase();
    v.contains("utf-8") || v.contains("utf8")
}

/// The user's shell.
pub fn shell() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("COMSPEC")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("cmd.exe"))
    } else {
        std::env::var_os("SHELL")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"))
    }
}

/// SIGKILL every process left in session `sid` (the terminal's program
/// leads its own session, so its jobs and their children are all in it).
#[cfg(target_os = "linux")]
fn kill_session(sid: libc::pid_t) {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return;
    };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<libc::pid_t>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) else {
            continue;
        };
        // pid (comm) state ppid pgrp session ...; comm may hold spaces.
        let session = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(3))
            .and_then(|x| x.parse::<libc::pid_t>().ok());
        if session == Some(sid) && pid != std::process::id() as libc::pid_t {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

/// The shell a terminal starts by default, and its arguments: the
/// configured `[terminal] shell`, else the user's.
pub fn default_shell(cfg: &crate::deck::config::TerminalCfg) -> (PathBuf, Vec<String>) {
    match cfg
        .shell
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => (
            resolve(s).unwrap_or_else(|| PathBuf::from(s)),
            cfg.args.clone(),
        ),
        None => (shell(), cfg.args.clone()),
    }
}

/// A shell named by path, or found on PATH.
fn resolve(s: &str) -> Option<PathBuf> {
    let p = PathBuf::from(s);
    if p.is_absolute() {
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|d| {
        let c = d.join(s);
        if c.is_file() {
            return Some(c);
        }
        let e = d.join(format!("{}.exe", s));
        e.is_file().then_some(e)
    })
}

/// The shells a terminal may start: the configured one, `[terminal] shells`,
/// the user's own, and those listed in /etc/shells (or PowerShell and cmd on
/// Windows) that are installed. Nothing else can be asked for.
pub fn shells(cfg: &crate::deck::config::TerminalCfg) -> Vec<PathBuf> {
    let mut names: Vec<String> = Vec::new();
    names.push(default_shell(cfg).0.display().to_string());
    names.extend(cfg.shells.iter().cloned());
    names.push(shell().display().to_string());
    if cfg!(windows) {
        names.extend(["pwsh.exe", "powershell.exe", "cmd.exe"].map(String::from));
    } else if let Ok(t) = std::fs::read_to_string("/etc/shells") {
        names.extend(
            t.lines()
                .map(str::trim)
                .filter(|l| l.starts_with('/'))
                .map(String::from),
        );
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for n in names {
        if let Some(p) = resolve(&n) {
            // One entry per program (/bin/zsh and /usr/bin/zsh are often the same file).
            let real = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            if !out
                .iter()
                .any(|o| std::fs::canonicalize(o).unwrap_or_else(|_| o.clone()) == real)
            {
                out.push(p);
            }
        }
    }
    out
}

/// Append one line to the terminal log.
pub fn audit(log_dir: &std::path::Path, spec: &Spec) {
    let _ = std::fs::create_dir_all(log_dir);
    let line = serde_json::json!({
        "ts": crate::deck::runs::now(), "what": spec.label, "cwd": spec.cwd.display().to_string(),
        "program": spec.program.display().to_string(), "args": spec.args,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("terminal.log"))
    {
        let _ = writeln!(f, "{}", line);
    }
}

/// Run `spec` on a PTY and bridge it to the WebSocket until either ends.
/// Binary frames are keystrokes; a text frame `{"t":"resize","cols":..,"rows":..}`
/// resizes. The terminal's output goes back as binary frames.
pub async fn bridge(spec: Spec, user: String, socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    struct Slot(u64);
    impl Drop for Slot {
        fn drop(&mut self) {
            OPEN.fetch_sub(1, Ordering::SeqCst);
            live().lock().unwrap().remove(&self.0);
        }
    }
    OPEN.fetch_add(1, Ordering::SeqCst);
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    let close_me = Arc::new(tokio::sync::Notify::new());
    live().lock().unwrap().insert(
        id,
        Live {
            label: spec.label.clone(),
            cwd: spec.cwd.display().to_string(),
            user,
            started: crate::deck::runs::now(),
            close: close_me.clone(),
        },
    );
    let _slot = Slot(id);
    let (mut sink, mut stream) = socket.split();
    // The page learns which terminal this is (to list and close it).
    let _ = sink
        .send(Message::Text(
            serde_json::json!({ "t": "session", "id": id, "label": spec.label })
                .to_string()
                .into(),
        ))
        .await;
    let size = PtySize {
        rows: 30,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    };
    let started = (|| -> Result<_, String> {
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| e.to_string())?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // A UTF-8 locale, or shells cannot print the icons and box drawing
        // prompts use (indra may run from a service with no locale at all).
        if !utf8_locale() {
            cmd.env(
                "LANG",
                if cfg!(target_os = "macos") {
                    "en_US.UTF-8"
                } else {
                    "C.UTF-8"
                },
            );
            cmd.env_remove("LC_ALL");
            cmd.env_remove("LC_CTYPE");
        }
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        Ok((pair.master, child, reader, writer))
    })();
    let (master, mut child, mut reader, mut writer) = match started {
        Ok(x) => x,
        Err(e) => {
            let _ = sink
                .send(Message::Binary(
                    format!(
                        "\r\nindra: cannot start {}: {}\r\n",
                        spec.program.display(),
                        e
                    )
                    .into_bytes()
                    .into(),
                ))
                .await;
            return;
        }
    };
    let master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(master));
    // The PTY's output, read on a blocking thread.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        return;
                    }
                }
            }
        }
    });
    // Keystrokes, written on a blocking thread.
    let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(b) = in_rx.recv() {
            if writer.write_all(&b).and_then(|_| writer.flush()).is_err() {
                return;
            }
        }
    });
    // The program can end while something it started in the background still
    // holds the terminal open, so watch the program itself, not just its output.
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    let mut ended = None;
    loop {
        tokio::select! {
            // Closed from the list of running terminals.
            _ = close_me.notified() => break,
            _ = tick.tick() => {
                if let Ok(Some(s)) = child.try_wait() {
                    ended = Some(s.exit_code());
                    // Pass on what it printed last.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    while let Ok(b) = out_rx.try_recv() {
                        let _ = sink.send(Message::Binary(b.into())).await;
                    }
                    break;
                }
            }
            out = out_rx.recv() => match out {
                Some(b) => {
                    if sink.send(Message::Binary(b.into())).await.is_err() {
                        break;
                    }
                }
                None => break, // the program ended
            },
            m = stream.next() => match m {
                Some(Ok(Message::Binary(b))) => {
                    let _ = in_tx.send(b.to_vec());
                }
                Some(Ok(Message::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str()) {
                        if v.get("t").and_then(|x| x.as_str()) == Some("resize") {
                            let cols = v.get("cols").and_then(|x| x.as_u64()).unwrap_or(100).clamp(10, 500) as u16;
                            let rows = v.get("rows").and_then(|x| x.as_u64()).unwrap_or(30).clamp(4, 300) as u16;
                            let _ = master.lock().unwrap().resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    // The page went away or the program ended: make sure it, and anything it
    // started, is gone. The program leads its own session (and process group).
    drop(in_tx);
    let code = tokio::task::spawn_blocking(move || {
        if ended.is_some() {
            return ended;
        }
        #[cfg(unix)]
        let group = child.process_id().map(|p| p as libc::pid_t);
        #[cfg(unix)]
        if let Some(g) = group {
            unsafe { libc::kill(-g, libc::SIGHUP) };
        }
        let mut code = None;
        for _ in 0..20 {
            if let Ok(Some(s)) = child.try_wait() {
                code = Some(s.exit_code());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if code.is_none() {
            #[cfg(unix)]
            if let Some(g) = group {
                unsafe { libc::kill(-g, libc::SIGKILL) };
            }
            let _ = child.kill();
            code = child.wait().map(|s| s.exit_code()).ok();
        }
        // Background jobs have process groups of their own, and a shell
        // that dies before passing on the hangup leaves them running: end
        // whatever is left in the terminal's session.
        #[cfg(target_os = "linux")]
        if let Some(g) = group {
            kill_session(g);
        }
        code
    })
    .await
    .ok()
    .flatten();
    drop(master);
    let _ = sink
        .send(Message::Binary(
            format!(
                "\r\n[indra: {} ended{}]\r\n",
                spec.label,
                code.map(|c| format!(", exit code {}", c))
                    .unwrap_or_default()
            )
            .into_bytes()
            .into(),
        ))
        .await;
    let _ = sink.close().await;
}
