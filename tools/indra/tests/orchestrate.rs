//! End-to-end orchestration over the SSH transport. A stand-in `ssh` drops
//! the options and destination and runs the remote command through `sh -c`,
//! exactly as sshd would, so the whole path is real: hub -> ssh -> remote
//! `indra node exec` -> runner. Unix only (the stand-in is a shell script).
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const INDRA: &str = env!("CARGO_BIN_EXE_indra");

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("indra-orch-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("remote-api")).unwrap();
        fs::create_dir_all(dir.join("local-web")).unwrap();
        let ssh = dir.join("fake-ssh");
        fs::write(&ssh, "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do if [ \"$1\" = \"--\" ]; then shift; break; fi; shift; done\nexec sh -c \"$*\"\n").unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            dir.join("remote.toml"),
            "builtins = false\n\
             [[runner]]\nname = \"echo\"\ncommand = \"echo\"\nargs = [\"remote says: {prompt}\"]\n\
             [[runner]]\nname = \"fail\"\ncommand = \"false\"\nargs = [\"{prompt}\"]\n\
             [[runner]]\nname = \"hold\"\ncommand = \"sh\"\nargs = [\"-c\", \"echo $$ > \\\"$0\\\"; exec sleep 600\", \"{prompt}\"]\n",
        )
        .unwrap();
        Env { dir }
    }

    fn p(&self, rel: &str) -> String {
        self.dir.join(rel).display().to_string()
    }

    /// The hub config: a local echo runner, the `box` node, and `workflows`.
    fn hub(&self, workflows: &str) -> PathBuf {
        let cfg = format!(
            "builtins = false\n\
             [local]\nmax_parallel = 2\n\
             [[runner]]\nname = \"echo\"\ncommand = \"echo\"\nargs = [\"local says: {{prompt}}\"]\n\
             [[node]]\nname = \"box\"\nssh = \"me@box.internal\"\nssh_command = \"{ssh}\"\nindra = \"{indra}\"\n\
             args = [\"--config\", \"{remote}\"]\ntargets = [\"{api}\"]\n{workflows}",
            ssh = self.p("fake-ssh"),
            indra = INDRA,
            remote = self.p("remote.toml"),
            api = self.p("remote-api"),
            workflows = workflows,
        );
        let path = self.dir.join("hub.toml");
        fs::write(&path, cfg).unwrap();
        path
    }

    fn indra(&self, args: &[&str]) -> Command {
        let mut c = Command::new(INDRA);
        c.args(args)
            // A space in the data path, as on macOS (~/Library/Application Support).
            .env("XDG_DATA_HOME", self.dir.join("app data"))
            .env("HOME", &self.dir)
            .current_dir(&self.dir);
        c
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn nodes_are_checked_over_ssh() {
    let e = Env::new("nodes");
    let hub = e.hub("");
    let out = e
        .indra(&["nodes", "--config", hub.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v[0]["name"], "local");
    assert_eq!(v[1]["name"], "box");
    let runners: Vec<&str> = v[1]["info"]["runners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    // Its own config first, then the built-in runners (claude, codex, gemini).
    assert_eq!(
        &runners[..3],
        ["echo", "fail", "hold"],
        "the remote reports its own runners: {:?}",
        runners
    );

    // A node that cannot be reached is reported, and the command fails.
    let bad = fs::read_to_string(&hub)
        .unwrap()
        .replace(&e.p("fake-ssh"), "/bin/false");
    fs::write(&hub, bad).unwrap();
    let out = e
        .indra(&["nodes", "--config", hub.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("DOWN"));
}

#[test]
fn workflow_orders_retries_skips_and_never_shells_the_prompt() {
    let e = Env::new("run");
    let pwned = e.p("pwned");
    let hub = e.hub(&format!(
        "[[workflow]]\nname = \"demo\"\n\
         [[workflow.task]]\nid = \"api\"\nnode = \"box\"\ntarget = \"{api}\"\nrunner = \"echo\"\nprompt = \"review {{target}} on {{node}}; $(touch {pwned}) `touch {pwned}`\"\n\
         [[workflow.task]]\nid = \"web\"\ntarget = \"local-web\"\nrunner = \"echo\"\nprompt = \"web after api\"\nneeds = [\"api\"]\n\
         [[workflow.task]]\nid = \"flaky\"\nnode = \"box\"\ntarget = \"{api}\"\nrunner = \"fail\"\nprompt = \"x\"\nretries = 1\n\
         [[workflow.task]]\nid = \"report\"\ntarget = \"local-web\"\nrunner = \"echo\"\nprompt = \"summary\"\nneeds = [\"web\", \"flaky\"]\n",
        api = e.p("remote-api"),
        pwned = pwned,
    ));
    let out = e
        .indra(&["run", "demo", "--config", hub.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a failed task fails the run:\n{}",
        text
    );
    let final_state = |id: &str| {
        text.lines()
            .rev()
            .find(|l| l.split_whitespace().next() == Some(id))
            .and_then(|l| l.split_whitespace().nth(2))
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(final_state("api"), "succeeded", "{}", text);
    assert_eq!(final_state("web"), "succeeded", "{}", text);
    assert_eq!(final_state("flaky"), "failed", "{}", text);
    assert_eq!(final_state("report"), "skipped", "{}", text);
    assert!(
        text.contains("attempt 2"),
        "flaky was retried once:\n{}",
        text
    );
    // web started only after api succeeded.
    let api_done = text
        .find(&format!("{:<18} {:<12} succeeded", "api", "box"))
        .unwrap();
    let web_run = text
        .find(&format!("{:<18} {:<12} running", "web", "local"))
        .unwrap();
    assert!(api_done < web_run, "{}", text);

    assert!(
        !Path::new(&pwned).exists(),
        "the prompt must never reach a shell"
    );
    // The run's summary names each task's log; the data directory differs by
    // OS (XDG on Linux, ~/Library/Application Support on macOS).
    let log_of = |id: &str| -> String {
        let line = text
            .lines()
            .rev()
            .find(|l| l.split_whitespace().next() == Some(id))
            .unwrap_or_else(|| panic!("no summary line for {}:\n{}", id, text));
        // id, node and state are single words; the path is the rest of the
        // line and may contain spaces (~/Library/Application Support).
        let mut rest = line.trim_start();
        for _ in 0..3 {
            rest = rest
                .split_once(char::is_whitespace)
                .map(|(_, r)| r.trim_start())
                .unwrap_or_else(|| panic!("no log path for {}: {}", id, line));
        }
        let path = rest.trim_end();
        fs::read_to_string(path).unwrap_or_else(|err| panic!("cannot read {}: {}", path, err))
    };
    let logs = format!("{}\n{}", log_of("api"), log_of("web"));
    assert!(
        logs.contains(&format!(
            "remote says: review {} on box; $(touch {})",
            e.p("remote-api"),
            pwned
        )),
        "{}",
        logs
    );
    assert!(logs.contains("local says: web after api"), "{}", logs);
}

#[test]
fn bad_workflows_are_rejected_before_anything_runs() {
    let e = Env::new("invalid");
    let hub = e.hub(
        "[[workflow]]\nname = \"loop\"\n\
         [[workflow.task]]\nid = \"a\"\ntarget = \".\"\nprompt = \"x\"\nneeds = [\"b\"]\n\
         [[workflow.task]]\nid = \"b\"\ntarget = \".\"\nprompt = \"x\"\nneeds = [\"a\"]\n",
    );
    let out = e
        .indra(&["run", "loop", "--config", hub.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dependency cycle"));

    let bad_dest = fs::read_to_string(&hub)
        .unwrap()
        .replace("me@box.internal", "-oProxyCommand=touch");
    fs::write(&hub, bad_dest).unwrap();
    let out = e
        .indra(&["nodes", "--config", hub.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid ssh destination"));
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn cancelling_stops_the_remote_agent() {
    let e = Env::new("cancel");
    let pidfile = e.p("agent.pid");
    let hub = e.hub(&format!(
        "[[workflow]]\nname = \"long\"\n[[workflow.task]]\nid = \"hold\"\nnode = \"box\"\ntarget = \"{api}\"\nrunner = \"hold\"\nprompt = \"{pidfile}\"\n",
        api = e.p("remote-api"),
        pidfile = pidfile,
    ));
    let mut child = e
        .indra(&["run", "long", "--config", hub.to_str().unwrap()])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let pid = loop {
        if let Some(p) = fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            break p;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the remote agent never started"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(alive(pid));
    // Ctrl-C on `indra run` cancels the workflow.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(1));
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !alive(pid),
        "the agent on the node must be stopped when the run is cancelled"
    );
}
