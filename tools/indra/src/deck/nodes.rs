//! Nodes: the machines agents run on.
//!
//! `local` is always present. A remote node is reached over SSH, so there is
//! no daemon to install, no port to open, and authentication and encryption
//! are SSH's (keys, agents, `~/.ssh/config`, known_hosts). The remote needs
//! `indra` on its PATH; the hub only ever runs `indra node <info|runs|exec>`
//! there and passes everything else as JSON on stdin, so no prompt or path
//! ever goes through the remote shell.
//!
//! The remote runs only runners from its *own* configuration, by name, in a
//! directory that must exist on it, and kills the whole process tree when the
//! SSH session ends (stdin EOF).
//!
//! Protocol of `indra node exec` (one JSON object per line on stdout):
//!   {"t":"start","runner":..,"format":..,"pid":..,"host":..}
//!   {"t":"out","l":"<a stdout line of the agent>"}
//!   {"t":"err","l":"<a stderr line>"}
//!   {"t":"error","msg":".."}     the job could not start
//!   {"t":"exit","code":N}

use crate::deck::config::{self, Harness, Runner, Settings, StageCfg};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const LOCAL: &str = "local";

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub name: String,
    pub description: String,
    /// SSH destination: `host`, `user@host`, or a `Host` alias from ~/.ssh/config.
    pub ssh: String,
    pub port: Option<u16>,
    pub identity: Option<PathBuf>,
    /// The indra binary on the remote (default `indra`).
    pub indra: String,
    /// Extra arguments for every remote `indra node ...` call, e.g. `--config`.
    pub args: Vec<String>,
    pub max_parallel: usize,
    /// Codebases on the remote (remote paths).
    pub targets: Vec<String>,
    /// The ssh client to run (default `ssh`).
    pub ssh_command: String,
}

/// A destination is a host, `user@host` or an ssh_config alias. Refusing a
/// leading `-` and anything outside a conservative set means a config value
/// can never smuggle in an ssh option such as `-oProxyCommand=...`.
pub fn validate_destination(s: &str) -> Result<(), String> {
    let ok = !s.is_empty()
        && !s.starts_with('-')
        && s.len() <= 255
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._-:[]%".contains(c));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid ssh destination '{}': use host, user@host or an ssh_config alias",
            s
        ))
    }
}

/// Node names appear in URLs, logs and the UI.
pub fn validate_name(s: &str) -> Result<(), String> {
    let ok = !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid node name '{}': use letters, digits, '-' and '_'",
            s
        ))
    }
}

/// POSIX shell quoting for one word of the remote command. ssh joins its
/// arguments into a single string that the remote shell parses, so every
/// word is quoted unless it is plainly safe.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl Node {
    /// `(program, args)` that run `indra node <sub>` on this node over SSH.
    pub fn ssh_command_for(&self, sub: &str) -> Result<(PathBuf, Vec<String>), String> {
        let program = config::which(&self.ssh_command).ok_or_else(|| {
            format!(
                "node '{}': ssh client '{}' is not installed",
                self.name, self.ssh_command
            )
        })?;
        let mut args: Vec<String> = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=10".into(),
            "-o".into(),
            "ServerAliveInterval=30".into(),
            "-T".into(),
        ];
        if let Some(p) = self.port {
            args.push("-p".into());
            args.push(p.to_string());
        }
        if let Some(i) = &self.identity {
            args.push("-i".into());
            args.push(i.display().to_string());
        }
        args.push(self.ssh.clone());
        args.push("--".into());
        let words = std::iter::once(self.indra.as_str())
            .chain(["node", sub])
            .chain(self.args.iter().map(String::as_str));
        args.extend(words.map(shell_quote));
        Ok((program, args))
    }

    /// Run `indra node <sub>` remotely with `input` on stdin; its stdout.
    pub async fn call(&self, sub: &str, input: &str, timeout_secs: u64) -> Result<String, String> {
        let (program, args) = self.ssh_command_for(sub)?;
        let mut child = tokio::process::Command::new(program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("cannot run ssh: {}", e))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(input.as_bytes()).await;
            let _ = si.write_all(b"\n").await;
        }
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| {
            format!(
                "node '{}' did not answer within {}s",
                self.name, timeout_secs
            )
        })?
        .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let msg = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            return Err(if msg.is_empty() {
                format!("ssh to '{}' failed ({})", self.name, out.status)
            } else {
                msg
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

// ---- node facts ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerInfo {
    pub name: String,
    pub available: bool,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpus: usize,
    pub version: String,
    pub runners: Vec<RunnerInfo>,
}

fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname NUL-terminates or truncates.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..end]).into_owned();
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

pub fn local_info(s: &Settings) -> NodeInfo {
    NodeInfo {
        hostname: hostname(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        cpus: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        version: env!("CARGO_PKG_VERSION").into(),
        runners: s
            .runners
            .iter()
            .map(|r| RunnerInfo {
                name: r.name.clone(),
                available: r.program.is_some(),
                format: r.format.clone(),
            })
            .collect(),
    }
}

pub async fn remote_info(n: &Node) -> Result<NodeInfo, String> {
    let out = n.call("info", "{}", 20).await?;
    let line = out
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or("");
    serde_json::from_str(line).map_err(|_| {
        format!(
            "node '{}' did not return indra node info (is indra installed there?)",
            n.name
        )
    })
}

/// What `indra node runs` needs to list run directories on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunsSpec {
    pub targets: Vec<String>,
    pub runs: String,
    pub findings: Vec<String>,
    pub group_by: Option<String>,
    pub stages: Vec<StageCfg>,
}

impl RunsSpec {
    pub fn for_node(h: &Harness, targets: &[String]) -> Option<RunsSpec> {
        Some(RunsSpec {
            targets: targets.to_vec(),
            runs: h.runs.clone()?,
            findings: h.findings.clone(),
            group_by: h.group_by.clone(),
            stages: h.stages.clone(),
        })
    }
}

pub async fn remote_runs(n: &Node, spec: &RunsSpec) -> Result<Value, String> {
    let input = serde_json::to_string(spec).map_err(|e| e.to_string())?;
    let out = n.call("runs", &input, 30).await?;
    let line = out
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('['))
        .unwrap_or("[]");
    serde_json::from_str(line).map_err(|e| e.to_string())
}

// ---- the remote side: `indra node info|runs|exec` -----------------------------------

pub fn cmd_info(s: &Settings) -> Result<ExitCode, String> {
    println!(
        "{}",
        serde_json::to_string(&local_info(s)).map_err(|e| e.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

fn read_stdin_line() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(line)
}

fn expand_target(t: &str) -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    config::expand(&base, t)
}

pub fn cmd_runs() -> Result<ExitCode, String> {
    let spec: RunsSpec = serde_json::from_str(read_stdin_line()?.trim())
        .map_err(|e| format!("bad runs request: {}", e))?;
    let h = Harness {
        name: "remote".into(),
        description: String::new(),
        root: PathBuf::from("."),
        agents: vec![],
        skills: vec![],
        prompts: vec![],
        memory: vec![],
        targets: spec.targets.iter().map(|t| expand_target(t)).collect(),
        runs: Some(spec.runs),
        findings: spec.findings,
        group_by: spec.group_by,
        stages: spec.stages,
        builtin: false,
    };
    println!(
        "{}",
        serde_json::to_string(&crate::deck::runs::list(&h, 100)).map_err(|e| e.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub runner: String,
    pub prompt: String,
    pub target: String,
}

type Out = Arc<Mutex<std::io::Stdout>>;

fn emit(out: &Out, v: Value) {
    let mut o = out.lock().unwrap();
    let _ = writeln!(o, "{}", v);
    let _ = o.flush();
}

pub fn cmd_exec(s: &Settings) -> Result<ExitCode, String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let code = rt.block_on(exec(s));
    // A thread is still blocked reading stdin (the hub keeps it open for the
    // life of the job). Dropping the runtime normally would wait for it.
    rt.shutdown_background();
    code
}

async fn exec(s: &Settings) -> Result<ExitCode, String> {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let fail = |msg: String| {
        emit(&out, json!({ "t": "error", "msg": msg }));
        Ok(ExitCode::from(2))
    };
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    if stdin
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?
        == 0
    {
        return fail("no job on stdin".into());
    }
    let job: Job = match serde_json::from_str(line.trim()) {
        Ok(j) => j,
        Err(e) => return fail(format!("bad job: {}", e)),
    };
    let Some(runner) = s.runners.iter().find(|r| r.name == job.runner) else {
        return fail(format!(
            "runner '{}' is not configured on {}",
            job.runner,
            hostname()
        ));
    };
    let target = expand_target(&job.target);
    if !target.is_dir() {
        return fail(format!(
            "target is not a directory on {}: {}",
            hostname(),
            target.display()
        ));
    }
    let (program, args) = match runner_argv(runner, &job.prompt, &target) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&args)
        .current_dir(&target)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("cannot start {}: {}", program.display(), e)),
    };
    let pid = child.id();
    emit(
        &out,
        json!({ "t": "start", "runner": runner.name, "format": runner.format, "pid": pid, "host": hostname() }),
    );

    let forward = |stream: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
                   tag: &'static str,
                   out: Out| {
        tokio::spawn(async move {
            let Some(r) = stream else { return };
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                emit(&out, json!({ "t": tag, "l": l }));
            }
        })
    };
    let so = child
        .stdout
        .take()
        .map(|x| Box::new(x) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
    let se = child
        .stderr
        .take()
        .map(|x| Box::new(x) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
    let t1 = forward(so, "out", out.clone());
    let t2 = forward(se, "err", out.clone());

    // The hub keeps stdin open for the life of the job; EOF means the SSH
    // session (or the hub) went away, so the agent must stop too.
    let eof = async move {
        let mut buf = String::new();
        loop {
            buf.clear();
            match stdin.read_line(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    };
    let status = tokio::select! {
        st = child.wait() => st,
        _ = eof => {
            crate::deck::procs::terminate_tree(pid);
            match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                Ok(st) => st,
                Err(_) => { let _ = child.start_kill(); child.wait().await }
            }
        }
    };
    let _ = t1.await;
    let _ = t2.await;
    let code = status.ok().and_then(|st| st.code()).unwrap_or(1);
    emit(&out, json!({ "t": "exit", "code": code }));
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// The argument vector a runner executes for a prompt in a target. The
/// prompt is substituted last so its own text is never expanded again.
pub fn runner_argv(
    r: &Runner,
    prompt: &str,
    target: &std::path::Path,
) -> Result<(PathBuf, Vec<String>), String> {
    let program = r.program.clone().ok_or_else(|| {
        format!(
            "runner '{}': '{}' is not installed or not on PATH",
            r.name, r.command
        )
    })?;
    let target = target.to_string_lossy();
    let prompt = prompt.replace("{target}", &target);
    let args = r
        .args
        .iter()
        .map(|a| a.replace("{target}", &target).replace("{prompt}", &prompt))
        .collect();
    Ok((program, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        for ok in [
            "host",
            "user@host.example.com",
            "build-box",
            "10.0.0.5",
            "me@[::1]",
            "alias_1",
        ] {
            assert!(validate_destination(ok).is_ok(), "{}", ok);
        }
        for bad in [
            "",
            "-oProxyCommand=sh",
            "host name",
            "a;b",
            "a$(x)",
            "a`b`",
            "h\nx",
        ] {
            assert!(validate_destination(bad).is_err(), "{}", bad);
        }
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("indra"), "indra");
        assert_eq!(shell_quote("/opt/indra/bin/indra"), "/opt/indra/bin/indra");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("$(rm -rf ~)"), "'$(rm -rf ~)'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn ssh_command_shape() {
        let n = Node {
            name: "box".into(),
            description: String::new(),
            ssh: "me@box".into(),
            port: Some(2222),
            identity: Some(PathBuf::from("/k/id")),
            indra: "/opt/my indra".into(),
            args: vec!["--config".into(), "/etc/indra.toml".into()],
            max_parallel: 2,
            targets: vec![],
            ssh_command: "sh".into(),
        };
        let (_, a) = n.ssh_command_for("exec").unwrap();
        let tail: Vec<&str> = a
            .iter()
            .skip_while(|x| *x != "me@box")
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            vec![
                "me@box",
                "--",
                "'/opt/my indra'",
                "node",
                "exec",
                "--config",
                "/etc/indra.toml"
            ]
        );
        assert!(a.contains(&"BatchMode=yes".to_string()));
        assert!(a.windows(2).any(|w| w[0] == "-p" && w[1] == "2222"));
    }
}
