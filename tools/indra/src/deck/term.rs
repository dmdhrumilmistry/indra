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

/// What to start in the terminal.
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// For the log and the tab title.
    pub label: String,
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
pub async fn bridge(spec: Spec, socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    struct Slot;
    impl Drop for Slot {
        fn drop(&mut self) {
            OPEN.fetch_sub(1, Ordering::SeqCst);
        }
    }
    OPEN.fetch_add(1, Ordering::SeqCst);
    let _slot = Slot;
    let (mut sink, mut stream) = socket.split();
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
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
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
        for _ in 0..20 {
            if let Ok(Some(s)) = child.try_wait() {
                return Some(s.exit_code());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        #[cfg(unix)]
        if let Some(g) = group {
            unsafe { libc::kill(-g, libc::SIGKILL) };
        }
        let _ = child.kill();
        child.wait().map(|s| s.exit_code()).ok()
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
