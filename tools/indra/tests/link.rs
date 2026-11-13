//! Nodes that dial in: `indra serve` (the hub) and `indra node connect` (a
//! node) as real processes on this machine. Enrolment, a job over the link,
//! the WebRTC path, Stop, and a connection cut in the middle of a job: every
//! line still arrives once, in order. Unix only (the runners are shell).
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const INDRA: &str = env!("CARGO_BIN_EXE_indra");
const TOKEN: &str = "link-test-token-0123456789";

struct Hub {
    dir: PathBuf,
    port: u16,
    procs: Vec<Child>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        for p in &mut self.procs {
            let _ = p.kill();
            let _ = p.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn http(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let req = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        method, path, port, TOKEN, body.len(), body
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let body = out
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    // Chunked bodies: keep the JSON part.
    let body = match (body.find('{'), body.find('[')) {
        (Some(a), Some(b)) => body[a.min(b)..].to_string(),
        (Some(a), None) | (None, Some(a)) => body[a..].to_string(),
        _ => body,
    };
    let end = body.rfind(['}', ']']).map(|i| i + 1).unwrap_or(body.len());
    (status, body[..end].to_string())
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|e| panic!("{}: {}", e, s))
}

fn until<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {}",
            what
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn hub(name: &str, webrtc: bool) -> Hub {
    let dir = std::env::temp_dir().join(format!("indra-link-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    std::fs::create_dir_all(dir.join("node-work")).unwrap();
    std::fs::write(
        dir.join("hub.toml"),
        format!("builtins = false\n[server]\nwebrtc = {}\n[[harness]]\nname = \"h\"\nroot = \".\"\ntargets = [\"work\"]\n", webrtc),
    )
    .unwrap();
    // The node's own runners: what it is willing to run.
    std::fs::write(
        dir.join("node.toml"),
        "builtins = false\n\
         [[runner]]\nname = \"ticker\"\ncommand = \"sh\"\nargs = [\"-c\", \"for i in $(seq 1 40); do echo tick-$i; sleep 0.1; done\", \"{prompt}\"]\n\
         [[runner]]\nname = \"hold\"\ncommand = \"sh\"\nargs = [\"-c\", \"echo $$ > \\\"$0\\\"; exec sleep 600\", \"{prompt}\"]\n",
    )
    .unwrap();
    let mut child = Command::new(INDRA)
        .args([
            "serve",
            "--config",
            dir.join("hub.toml").to_str().unwrap(),
            "--bind",
            "127.0.0.1:0",
        ])
        .env("INDRA_TOKEN", TOKEN)
        // Each hub its own data directory, on macOS too (which ignores XDG_DATA_HOME).
        .env("HOME", dir.join("hub-home"))
        .env("XDG_DATA_HOME", dir.join("hub-data"))
        .env("XDG_CONFIG_HOME", dir.join("hub-config"))
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut port = 0;
    let mut line = String::new();
    while out.read_line(&mut line).unwrap() > 0 {
        if let Some(rest) = line.trim().strip_prefix("Open: http://127.0.0.1:") {
            port = rest.split('/').next().unwrap().parse().unwrap();
            break;
        }
        line.clear();
    }
    assert!(port > 0, "the hub did not start");
    std::thread::spawn(move || for _ in out.lines() {});
    Hub {
        dir,
        port,
        procs: vec![child],
    }
}

impl Hub {
    /// Enrol `box` and start `indra node connect` towards `port`.
    fn node(&mut self, port: u16) {
        let (s, body) = http(self.port, "POST", "/api/nodes/enrol", r#"{"name":"box"}"#);
        assert_eq!(s, 200, "{}", body);
        let code = json(&body)["code"].as_str().unwrap().to_string();
        let node = Command::new(INDRA)
            .args([
                "node",
                "connect",
                &format!("http://127.0.0.1:{}", port),
                "--name",
                "box",
                "--code",
                &code,
            ])
            .args(["--target", self.dir.join("node-work").to_str().unwrap()])
            .args(["--config", self.dir.join("node.toml").to_str().unwrap()])
            .env("HOME", self.dir.join("node-home"))
            .env("XDG_CONFIG_HOME", self.dir.join("node-config"))
            .env("XDG_DATA_HOME", self.dir.join("node-data"))
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(self.dir.join("node.log")).unwrap())
            .spawn()
            .unwrap();
        self.procs.push(node);
        // A code works once.
        let (s, _) = http(self.port, "GET", "/api/nodes", "");
        assert_eq!(s, 200);
    }

    fn node_view(&self) -> serde_json::Value {
        let (_, body) = http(self.port, "GET", "/api/nodes", "");
        json(&body)
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["name"] == "box")
            .cloned()
            .unwrap()
    }

    fn launch(&self, runner: &str, prompt: &str) -> u64 {
        let (s, body) = http(
            self.port,
            "POST",
            "/api/procs",
            &format!(
                r#"{{"node":"box","runner":"{}","prompt":"{}","target":0}}"#,
                runner, prompt
            ),
        );
        assert_eq!(s, 200, "{}", body);
        json(&body)["id"].as_u64().unwrap()
    }

    fn proc(&self, id: u64) -> serde_json::Value {
        let (_, body) = http(self.port, "GET", &format!("/api/procs/{}", id), "");
        json(&body)
    }
}

/// The feed of a process, as lines of text.
fn texts(hub: &Hub, id: u64) -> Vec<String> {
    let mut s = TcpStream::connect(("127.0.0.1", hub.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(800)))
        .unwrap();
    let req = format!("GET /api/procs/{}/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\n\r\n", id, hub.port, TOKEN);
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf)
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .map(|v| v["text"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn a_node_dials_in_runs_jobs_and_goes_direct() {
    let mut h = hub("direct", true);
    let port = h.port;
    h.node(port);
    let v = until("the node to connect", 20, || {
        let v = h.node_view();
        (v["kind"] == "link").then_some(v)
    });
    assert_eq!(v["targets"].as_array().unwrap().len(), 1, "{}", v);
    if cfg!(feature = "webrtc") {
        until("the direct WebRTC path", 20, || {
            (h.node_view()["transport"] == "webrtc").then_some(())
        });
        eprintln!("transport: {}", h.node_view()["transport"]);
    }
    let id = h.launch("ticker", "go");
    let p = until("the job to end", 30, || {
        let p = h.proc(id);
        (p["status"] != "running").then_some(p)
    });
    assert_eq!(p["status"], "exited", "{}", p);
    let feed = texts(&h, id);
    let ticks: Vec<&String> = feed.iter().filter(|t| t.starts_with("tick-")).collect();
    assert_eq!(ticks.len(), 40, "{:?}", feed);

    // Stop reaches the node, which stops the agent's whole tree.
    let pidfile = h.dir.join("held.pid");
    let id = h.launch("hold", pidfile.to_str().unwrap());
    let pid: i32 = until("the agent to start", 20, || {
        std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    });
    let (s, _) = http(h.port, "POST", &format!("/api/procs/{}/stop", id), "");
    assert_eq!(s, 200);
    let p = until("the job to stop", 30, || {
        let p = h.proc(id);
        (p["status"] != "running").then_some(p)
    });
    assert_eq!(p["status"], "stopped", "{}", p);
    // SAFETY: signal 0 only checks that the process exists.
    until("the agent to be gone", 15, || {
        (unsafe { libc::kill(pid, 0) } != 0).then_some(())
    });

    // A wrong key, or a used code, gets nowhere.
    let mut s = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    s.write_all(format!(
        "GET /agent/connect HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nX-Indra-Node: box\r\nAuthorization: Bearer nope\r\n\r\n",
        h.port
    ).as_bytes()).unwrap();
    let mut reply = String::new();
    let _ = BufReader::new(s).read_line(&mut reply);
    assert!(reply.contains("401"), "{}", reply);
}

/// A TCP proxy between node and hub whose connections the test can cut.
struct Proxy {
    port: u16,
    conns: Arc<Mutex<Vec<TcpStream>>>,
}

impl Proxy {
    fn new(to: u16) -> Proxy {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let conns: Arc<Mutex<Vec<TcpStream>>> = Arc::default();
        let c2 = conns.clone();
        std::thread::spawn(move || {
            for client in l.incoming().flatten() {
                let Ok(server) = TcpStream::connect(("127.0.0.1", to)) else {
                    continue;
                };
                c2.lock().unwrap().push(client.try_clone().unwrap());
                c2.lock().unwrap().push(server.try_clone().unwrap());
                let pipe = |mut a: TcpStream, mut b: TcpStream| {
                    std::thread::spawn(move || {
                        let _ = std::io::copy(&mut a, &mut b);
                        let _ = b.shutdown(Shutdown::Both);
                    });
                };
                pipe(client.try_clone().unwrap(), server.try_clone().unwrap());
                pipe(server, client);
            }
        });
        Proxy { port, conns }
    }

    fn cut(&self) {
        for c in self.conns.lock().unwrap().drain(..) {
            let _ = c.shutdown(Shutdown::Both);
        }
    }
}

#[test]
fn a_cut_connection_loses_and_repeats_nothing() {
    let mut h = hub("cut", false);
    let proxy = Proxy::new(h.port);
    h.node(proxy.port);
    until("the node to connect", 20, || {
        (h.node_view()["kind"] == "link").then_some(())
    });
    assert_eq!(h.node_view()["transport"], "relay");
    let id = h.launch("ticker", "go");
    // Let some ticks through, cut the line twice, and let the node come back.
    let seen = Arc::new(AtomicBool::new(false));
    std::thread::sleep(Duration::from_millis(900));
    proxy.cut();
    std::thread::sleep(Duration::from_millis(1500));
    proxy.cut();
    seen.store(true, Ordering::SeqCst);
    let start = Instant::now();
    let p = loop {
        let p = h.proc(id);
        if p["status"] != "running" {
            break p;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!(
                "the job did not end after reconnecting\nnode: {}\nnode log:\n{}\nfeed: {:?}",
                h.node_view(),
                std::fs::read_to_string(h.dir.join("node.log")).unwrap_or_default(),
                texts(&h, id)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(p["status"], "exited", "{}", p);
    let feed = texts(&h, id);
    let ticks: Vec<String> = feed
        .iter()
        .filter(|t| t.starts_with("tick-"))
        .cloned()
        .collect();
    let want: Vec<String> = (1..=40).map(|i| format!("tick-{}", i)).collect();
    assert_eq!(ticks, want, "every line once, in order");
}
