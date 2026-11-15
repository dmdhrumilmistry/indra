//! `indra mcp`: an MCP server (stdio, JSON-RPC 2.0) that lets an AI agent
//! design workflows: read the format and what this machine has, look at the
//! workflows there are, validate a draft, and save it to `indra.toml` or a
//! repository's `.indra/workflows/`.
//!
//! Add it to Claude Code with `claude mcp add indra -- indra mcp`; the
//! console's **Create with Claude** starts Claude Code with it. Nothing here
//! runs a workflow or a command: it only reads and writes workflow
//! definitions, each checked exactly as `indra serve` would load it.

use crate::deck::config::{self, ConfigFile, LoadOptions, RepoFile, Settings, WorkflowCfg};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The format, for the agent that writes a workflow.
pub const GUIDE: &str = r#"# indra workflows

A workflow is a graph of steps. Each step runs an AI agent (a prompt), a
skill, a shell command, or another workflow, in a target directory on a node.
`needs` orders steps; steps with nothing between them run in parallel.

```toml
[[workflow]]
name = "ci-fix"                       # letters, digits, - and _
description = "Test, and let an agent fix failures"
schedule = "0 9 * * 1-5"              # optional: runs by itself (cron, the hub's local time)

[[workflow.input]]                    # asked for when it runs
name = "branch"                       # used as {input.branch} in prompts
label = "Branch"
required = true
# kind = "text" | "choice" | "bool" | "target" | "profile"
# choice: options = [{ value = "a", label = "A" }]; bool: when_true = "--deep"
# text: pattern = '[a-z0-9/_-]+' (a regular expression for the whole value)

[[workflow.task]]
id = "test"                           # unique in the workflow
run = "cargo test 2>&1"               # a SHELL STEP: bash (sh), PowerShell on Windows
allow_failure = true                  # its failure does not fail the workflow

[[workflow.task]]
id = "passed"
needs = ["test"]
if = "test.succeeded"                 # an IF STEP: runs nothing, answers yes or no

[[workflow.task]]
id = "fix"
needs = ["passed"]
branch = "no"                         # runs only when the If answered no
profile = "worker"                    # how the agent runs (see Profiles below)
prompt = """Tests failed on {input.branch}:
{steps.test.output}
Fix the code so they pass."""         # an AGENT STEP

[[workflow.task]]
id = "retest"
needs = ["fix"]
run = "cargo test"
until = "self.exit_code == 0"         # a LOOP: run again until it holds
max_runs = 3                          # (default 3, at most 20)

[[workflow.task]]
id = "per-service"
foreach = ["api", "web", "worker"]    # a LOOP: one copy per item, in parallel
run = "make -C services/$INDRA_ITEM lint"   # or "{input.services}" (comma or newline separated)

[[workflow.task]]
id = "notify-failure"
needs = ["retest", "per-service"]
on = "failure"                        # runs only if something it needs failed
run = "echo \"failed: $INDRA_STATUS_RETEST\""

[[workflow.task]]
id = "nothing-to-do"
needs = ["passed"]
branch = "yes"                        # the other side of the If
stop = "success"                      # a STOP STEP: ends the workflow here, as success
prompt = "Tests pass; nothing to fix."  # (or "failure"); steps not started are skipped

[[workflow.task]]
id = "report"
needs = ["retest", "per-service"]
on = "always"                         # runs once they finish, however they went
skill = "summarize"                   # a SKILL STEP: Claude Code runs /summarize <prompt>
prompt = "{target}"
```

Step fields:
- id (required), needs = [ids], node = "local" (default) or a node name,
  target = directory on that node (in a repository file: default the
  repository itself, relative paths inside it; in indra.toml: default the
  harness's first target; on another node: required).
- One of: prompt (an agent), skill (+ prompt as its arguments, default
  {target}), run (a shell command), workflow = "<other workflow>" (runs it as
  one step), component = "<saved task>" (fields left empty come from it).
- profile, runner (default claude), model, provider ("anthropic" | "ollama"),
  retries (extra attempts after a failure).
- Control flow: on ("success" default | "failure" | "always"), allow_failure,
  foreach, until, max_runs, stop = "success" | "failure" (a step that only ends
  the workflow when reached; prompt is its message), if = "<condition>" (an If
  step: it runs nothing and answers "yes" or "no"), branch = "yes" | "no" (on a
  step after an If: run only on that answer; the other side is skipped, and a
  step both sides lead to runs after whichever side ran), and when (a
  condition on the step itself; prefer an If step, which the editor draws).
- Workflow: schedule = "<cron>" (minute hour day month weekday, e.g.
  "0 9 * * 1-5", "@daily") makes `indra serve` run it by itself; inputs take
  their defaults, so give required inputs a default.

Prompts may use {target}, {node}, {input.NAME}, {item} (in a foreach copy) and
{steps.ID.output}, {steps.ID.exit_code}, {steps.ID.status} (an earlier step's
result; ID must be in its needs, directly or through other steps).

Shell commands never get text substituted (so no value can change the
command): they read environment variables instead.
- $INDRA_INPUT_<NAME> (upper case), $INDRA_ITEM, $INDRA_TARGET, $INDRA_NODE,
  $INDRA_WORKFLOW, $INDRA_STEP
- For each finished step: $INDRA_OUTPUT_<ID>, $INDRA_EXIT_<ID>,
  $INDRA_STATUS_<ID> (ID upper case, - becomes _)
- A command runs in its target, succeeds with exit code 0, and its output is
  what later conditions see.

Conditions (if, when, until):
- <step>.succeeded | failed | skipped | status | output | exit_code,
  input.<name>, and in until: self.<the same fields> (this step's last run).
- ==, !=, contains, <, <=, >, >= (numbers), and, or, not, ( ).
- Literals: "text" or 'text', numbers, true, false.
- A referenced step must run before the step (be in its needs, directly or
  through others), and cannot be a foreach step (follow a foreach step with
  on = "failure" or "always" instead).

Semantics:
- A step with on = "success" runs when every step it needs succeeded (or
  failed with allow_failure); it is skipped when one failed or was skipped.
- The workflow fails if a step failed without allow_failure; skipped steps
  (a condition, a branch not taken) do not fail it.
- until is checked after a successful run; a failed run uses retries.
- Prefer shell steps for exact commands (build, test, lint, git) and agent
  steps for judgement (fixing, reviewing, writing). Use the built-in profiles
  (planner read-only, worker edits, builder cheap edits, reviewer) unless the
  person asked for others.
"#;

fn context(opts: &LoadOptions) -> String {
    let s = match config::load(opts.clone()) {
        Ok(s) => s,
        Err(e) => {
            return format!(
                "\n## This machine\n\nThe configuration does not load: {}\n",
                e
            )
        }
    };
    let mut out = String::from("\n## This machine\n\n");
    out.push_str(&format!(
        "Config file: {}\n",
        s.config_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(
                || config::edit_path(opts, None).display().to_string() + " (created on first save)"
            )
    ));
    out.push_str("\nSave locations (save_workflow `location`):\n- config (this machine's indra.toml; only here)\n");
    for d in &s.repo_dirs {
        out.push_str(&format!(
            "- {} (a repository: commit the file to share it)\n",
            d.display()
        ));
    }
    out.push_str("\nProfiles:\n");
    for p in &s.profiles {
        out.push_str(&format!(
            "- {}: {}{}\n",
            p.name,
            p.description,
            p.model
                .as_deref()
                .map(|m| format!(" (model {})", m))
                .unwrap_or_default()
        ));
    }
    out.push_str("\nNodes: local");
    for n in &s.nodes {
        out.push_str(&format!(", {} (targets: {})", n.name, n.targets.join(", ")));
    }
    out.push_str("\n\nRunners: ");
    out.push_str(
        &s.runners
            .iter()
            .map(|r| {
                format!(
                    "{}{}",
                    r.name,
                    if r.program.is_some() {
                        ""
                    } else {
                        " (not installed)"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
    );
    for h in &s.harnesses {
        let skills: Vec<_> = crate::deck::discover::discover(h)
            .into_iter()
            .filter(|d| d.kind == crate::deck::discover::Kind::Skill)
            .collect();
        out.push_str(&format!(
            "\n\nHarness {} (targets: {}); skills:\n",
            h.name,
            h.targets
                .iter()
                .map(|t| t.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if skills.is_empty() {
            out.push_str("- none\n");
        }
        for d in skills.iter().take(80) {
            out.push_str(&format!(
                "- {}: {}\n",
                d.name,
                d.description.chars().take(160).collect::<String>()
            ));
        }
    }
    out.push_str("\nSaved tasks (component = \"<name>\"):\n");
    if s.components.is_empty() {
        out.push_str("- none\n");
    }
    for c in &s.components {
        out.push_str(&format!(
            "- {}: {}\n",
            c.name,
            c.run
                .as_deref()
                .map(|r| format!("run {}", r))
                .or_else(|| c.skill.as_deref().map(|k| format!("skill {}", k)))
                .unwrap_or_else(|| c.prompt.chars().take(120).collect())
        ));
    }
    out.push_str(
        "\nWorkflows already defined (names must differ unless you mean to replace one):\n",
    );
    for w in &s.workflows {
        out.push_str(&format!(
            "- {}{}\n",
            w.name,
            if w.builtin { " (built in)" } else { "" }
        ));
    }
    out
}

fn tools() -> Value {
    json!([
        { "name": "workflow_guide",
          "description": "The indra workflow format (steps, shell commands, conditions, loops, inputs) and what this machine has: profiles, nodes, repositories to save in, skills, saved tasks, existing workflows. Call it first.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "list_workflows",
          "description": "The workflows defined here, with where each lives.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "get_workflow",
          "description": "One workflow as TOML, as written (to change it, edit and save it back).",
          "inputSchema": { "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] } },
        { "name": "validate_workflow",
          "description": "Check workflow TOML ([[workflow]] tables, optional [[component]] tables) exactly as indra would load it, for a save location. Returns the errors to fix, or a summary of the steps.",
          "inputSchema": { "type": "object", "properties": {
              "toml": { "type": "string" },
              "location": { "type": "string", "description": "\"config\" or a repository directory from workflow_guide (default config)" } },
            "required": ["toml"] } },
        { "name": "save_workflow",
          "description": "Validate and save workflow TOML to this machine's indra.toml (location \"config\") or to a repository's .indra/workflows/<name>.toml (location = the repository directory). A workflow with the same name there is replaced.",
          "inputSchema": { "type": "object", "properties": {
              "toml": { "type": "string" },
              "location": { "type": "string" } },
            "required": ["toml", "location"] } }
    ])
}

/// The config file workflows are checked against and saved to.
fn config_file(opts: &LoadOptions) -> (Option<PathBuf>, PathBuf) {
    let loaded = match &opts.config {
        Some(p) => Some(p.clone()).filter(|p| p.is_file()),
        None => config::default_config_paths(&opts.cwd)
            .into_iter()
            .find(|p| p.is_file()),
    };
    let edit = config::edit_path(opts, loaded.as_deref());
    (loaded, edit)
}

enum Loc {
    Config,
    Repo(PathBuf),
}

fn location(opts: &LoadOptions, raw: Option<&str>) -> Result<Loc, String> {
    let raw = raw
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .unwrap_or("config");
    if raw == "config" {
        return Ok(Loc::Config);
    }
    let s = config::load(opts.clone())?;
    let want = PathBuf::from(raw);
    let want = std::fs::canonicalize(&want).unwrap_or(want);
    s.repo_dirs
        .iter()
        .find(|d| **d == want)
        .map(|d| Loc::Repo(d.clone()))
        .ok_or_else(|| {
            format!(
                "location must be \"config\" or one of these repositories: {}",
                s.repo_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// Load the draft with the rest of the configuration, as indra would.
fn check(opts: &LoadOptions, text: &str, loc: &Loc) -> Result<(RepoFile, Settings), String> {
    let rf: RepoFile =
        toml::from_str(text).map_err(|e| format!("not valid workflow TOML: {}", e))?;
    if rf.workflow.is_empty() {
        return Err("there is no [[workflow]] table in it".into());
    }
    let (loaded, _) = config_file(opts);
    let mut file: ConfigFile = match &loaded {
        Some(p) => {
            let (t, _) = crate::deck::confedit::read(p)?;
            crate::deck::confedit::parse(&t).map_err(|e| format!("{}: {}", p.display(), e))?
        }
        None => ConfigFile::default(),
    };
    for w in &rf.workflow {
        let mut w = w.clone();
        // In a repository, a step without a target runs in it, and relative
        // targets are inside it.
        if let Loc::Repo(dir) = loc {
            for t in &mut w.tasks {
                if t.workflow.is_none()
                    && (t.node.is_empty() || t.node == crate::deck::nodes::LOCAL)
                    && !t.target.contains("{input.")
                {
                    t.target = if t.target.trim().is_empty() {
                        dir.display().to_string()
                    } else {
                        config::expand(dir, &t.target).display().to_string()
                    };
                }
            }
        }
        file.workflow.retain(|x| x.name != w.name);
        file.workflow.push(w);
    }
    for c in &rf.component {
        file.component.retain(|x| x.name != c.name);
        file.component.push(c.clone());
    }
    let s = config::build_settings(opts, loaded, &file)?;
    Ok((rf, s))
}

fn summary(w: &WorkflowCfg) -> String {
    let mut out = format!("workflow '{}': {} steps\n", w.name, w.tasks.len());
    for t in &w.tasks {
        let kind = if let Some(r) = &t.run {
            format!("run {}", r.lines().next().unwrap_or(""))
        } else {
            format!(
                "agent: {}",
                t.prompt
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect::<String>()
            )
        };
        out.push_str(&format!(
            "- {} [{}]{}{}{}{}{}\n",
            t.id,
            kind,
            if t.needs.is_empty() {
                String::new()
            } else {
                format!(" after {}", t.needs.join(", "))
            },
            t.on.as_deref()
                .filter(|o| *o != "success")
                .map(|o| format!(" on {}", o))
                .unwrap_or_default(),
            t.when
                .as_deref()
                .map(|c| format!(" when {}", c))
                .unwrap_or_default(),
            t.foreach
                .as_ref()
                .map(|f| format!(" for each of {}", f.list().join(", ")))
                .unwrap_or_default(),
            t.until
                .as_deref()
                .map(|c| format!(" until {}", c))
                .unwrap_or_default(),
        ));
    }
    out
}

fn call(opts: &LoadOptions, name: &str, args: &Value) -> Result<String, String> {
    let s_arg = |k: &str| args.get(k).and_then(|v| v.as_str());
    match name {
        "workflow_guide" => Ok(format!("{}{}", GUIDE, context(opts))),
        "list_workflows" => {
            let s = config::load(opts.clone())?;
            let mut out = String::new();
            for w in &s.workflows {
                out.push_str(&format!(
                    "- {} ({} steps){}: {}\n",
                    w.name,
                    w.tasks.len(),
                    if w.builtin {
                        " built in".to_string()
                    } else {
                        w.source
                            .as_ref()
                            .map(|p| format!(" in {}", p.display()))
                            .unwrap_or_else(|| " in indra.toml".into())
                    },
                    w.description
                ));
            }
            for e in &s.workflow_errors {
                out.push_str(&format!("! not usable: {}\n", e));
            }
            Ok(if out.is_empty() {
                "No workflows yet.".into()
            } else {
                out
            })
        }
        "get_workflow" => {
            let want = s_arg("name").ok_or("name is required")?;
            let (loaded, _) = config_file(opts);
            let mut found: Option<RepoFile> = None;
            if let Some(p) = &loaded {
                let (t, _) = crate::deck::confedit::read(p)?;
                if let Ok(cf) = crate::deck::confedit::parse(&t) {
                    if let Some(w) = cf.workflow.iter().find(|w| w.name == want) {
                        found = Some(RepoFile {
                            workflow: vec![w.clone()],
                            component: vec![],
                        });
                    }
                }
            }
            if found.is_none() {
                let s = config::load(opts.clone())?;
                for d in &s.repo_dirs {
                    for f in config::repo_workflow_files(d) {
                        if let Ok(rf) = config::read_repo_file(&f) {
                            if let Some(w) = rf.workflow.iter().find(|w| w.name == want) {
                                found = Some(RepoFile {
                                    workflow: vec![w.clone()],
                                    component: rf.component.clone(),
                                });
                            }
                        }
                    }
                }
                if found.is_none() {
                    if let Some(w) = s.workflows.iter().find(|w| w.name == want && w.builtin) {
                        found = Some(RepoFile {
                            workflow: vec![w.clone()],
                            component: vec![],
                        });
                    }
                }
            }
            let rf = found.ok_or_else(|| format!("no workflow named '{}'", want))?;
            toml::to_string(&rf).map_err(|e| e.to_string())
        }
        "validate_workflow" => {
            let loc = location(opts, s_arg("location"))?;
            let (rf, s) = check(opts, s_arg("toml").ok_or("toml is required")?, &loc)?;
            let mut out = String::from("Valid.\n");
            for w in &rf.workflow {
                if let Some(x) = s.workflows.iter().find(|x| x.name == w.name) {
                    out.push_str(&summary(x));
                }
            }
            Ok(out)
        }
        "save_workflow" => {
            let loc = location(opts, s_arg("location"))?;
            let text = s_arg("toml").ok_or("toml is required")?;
            let (rf, _) = check(opts, text, &loc)?;
            match loc {
                Loc::Config => {
                    let (_, path) = config_file(opts);
                    let (current, _) = crate::deck::confedit::read(&path)?;
                    let mut cf = crate::deck::confedit::parse(&current)?;
                    for w in &rf.workflow {
                        cf.workflow.retain(|x| x.name != w.name);
                        cf.workflow.push(w.clone());
                    }
                    let mut next = crate::deck::confedit::set_section(
                        &current,
                        "workflow",
                        &serde_json::to_value(&cf.workflow).map_err(|e| e.to_string())?,
                    )?;
                    if !rf.component.is_empty() {
                        for c in &rf.component {
                            cf.component.retain(|x| x.name != c.name);
                            cf.component.push(c.clone());
                        }
                        next = crate::deck::confedit::set_section(
                            &next,
                            "component",
                            &serde_json::to_value(&cf.component).map_err(|e| e.to_string())?,
                        )?;
                    }
                    crate::deck::confedit::write_atomic(&path, &next)?;
                    Ok(format!(
                        "Saved {} to {}. Run it from the console's Workflows page or with `indra run {}`.",
                        rf.workflow.iter().map(|w| w.name.as_str()).collect::<Vec<_>>().join(", "),
                        path.display(),
                        rf.workflow[0].name
                    ))
                }
                Loc::Repo(dir) => {
                    let mut saved = Vec::new();
                    for (i, w) in rf.workflow.iter().enumerate() {
                        let f = config::repo_workflow_dir(&dir).join(format!("{}.toml", w.name));
                        let mut file = if f.exists() {
                            config::read_repo_file(&f)
                                .map_err(|e| format!("{}: {}", f.display(), e))?
                        } else {
                            RepoFile::default()
                        };
                        file.workflow.retain(|x| x.name != w.name);
                        file.workflow.push(w.clone());
                        // The saved tasks go with the first workflow's file.
                        if i == 0 {
                            for c in &rf.component {
                                file.component.retain(|x| x.name != c.name);
                                file.component.push(c.clone());
                            }
                        }
                        config::write_repo_file(&f, &file)?;
                        saved.push(f.display().to_string());
                    }
                    Ok(format!(
                        "Saved {}. Commit it to share it. Run it from the console's Workflows page or with `indra run {}`.",
                        saved.join(", "),
                        rf.workflow[0].name
                    ))
                }
            }
        }
        _ => Err(format!("no tool named '{}'", name)),
    }
}

/// Answer one JSON-RPC message; None for a notification.
pub fn handle(opts: &LoadOptions, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let ok = |result: Value| json!({ "jsonrpc": "2.0", "id": id, "result": result });
    let err = |code: i64, message: &str| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
    Some(match method {
        "initialize" => ok(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "indra", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Design indra workflows: call workflow_guide first, write the TOML, validate_workflow until it is valid, then save_workflow.",
        })),
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
            let (text, is_error) = match call(opts, name, &args) {
                Ok(t) => (t, false),
                Err(e) => (e, true),
            };
            ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
        }
        _ => err(-32601, "method not found"),
    })
}

pub fn serve(opts: LoadOptions) -> Result<ExitCode, String> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&opts, &msg),
            Err(_) => Some(
                json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } }),
            ),
        };
        if let Some(r) = reply {
            writeln!(out, "{}", r).map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The MCP config file Claude Code is started with (`--mcp-config`): this
/// binary as the `indra` server, loading what the console loads.
pub fn client_config(dir: &Path, opts: &LoadOptions) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut args = vec!["mcp".to_string()];
    let (loaded, edit) = config_file(opts);
    args.push("--config".into());
    args.push(loaded.unwrap_or(edit).display().to_string());
    for t in &opts.targets {
        args.push("--target".into());
        args.push(opts.cwd.join(t).display().to_string());
    }
    let v = json!({ "mcpServers": { "indra": { "type": "stdio", "command": exe.display().to_string(), "args": args } } });
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let f = dir.join(format!("indra-mcp-{}.json", std::process::id()));
    std::fs::write(&f, serde_json::to_string_pretty(&v).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mcp_server_designs_and_saves_workflows() {
        let dir = std::env::temp_dir().join(format!("indra-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo")).unwrap();
        let cfg = dir.join("indra.toml");
        std::fs::write(
            &cfg,
            "# mine\n[[runner]]\nname = \"say\"\ncommand = \"echo\"\nargs = [\"{prompt}\"]\n",
        )
        .unwrap();
        let opts = LoadOptions {
            config: Some(cfg.clone()),
            targets: vec![dir.join("repo")],
            bind: None,
            read_only: false,
            cwd: dir.clone(),
        };
        let rpc = |method: &str, params: Value| {
            handle(
                &opts,
                &json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }),
            )
            .unwrap()
        };
        let init = rpc("initialize", json!({ "protocolVersion": "2025-06-18" }));
        assert_eq!(init["result"]["serverInfo"]["name"], "indra");
        assert!(handle(
            &opts,
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
        )
        .is_none());
        let names: Vec<String> = rpc("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "workflow_guide",
                "list_workflows",
                "get_workflow",
                "validate_workflow",
                "save_workflow"
            ]
        );
        let tool = |name: &str, args: Value| {
            let r = rpc("tools/call", json!({ "name": name, "arguments": args }));
            (
                r["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                r["result"]["isError"] == true,
            )
        };
        let (guide, e) = tool("workflow_guide", json!({}));
        assert!(
            !e && guide.contains("until") && guide.contains("planner") && guide.contains("repo"),
            "{}",
            guide
        );
        // A broken draft: the errors come back to fix.
        let bad = "[[workflow]]\nname = \"w\"\n[[workflow.task]]\nid = \"a\"\ntarget = \"repo\"\nrun = \"true\"\nwhen = \"b.failed\"\n";
        let (msg, e) = tool(
            "validate_workflow",
            json!({ "toml": bad, "location": "config" }),
        );
        assert!(e && msg.contains("'b' is not a step"), "{}", msg);
        // A good one, for the repository (no target needed there).
        let good = "[[workflow]]\nname = \"ci\"\n[[workflow.task]]\nid = \"test\"\nrun = \"make test\"\nallow_failure = true\n[[workflow.task]]\nid = \"fix\"\nneeds = [\"test\"]\nwhen = \"test.failed\"\nprofile = \"worker\"\nprompt = \"fix {steps.test.output}\"\n";
        let repo = std::fs::canonicalize(dir.join("repo"))
            .unwrap()
            .display()
            .to_string();
        let (msg, e) = tool(
            "validate_workflow",
            json!({ "toml": good, "location": repo }),
        );
        assert!(
            !e && msg.contains("fix [agent") && msg.contains("when test.failed"),
            "{}",
            msg
        );
        // In indra.toml, a step without a target runs in the first target.
        let (msg, e) = tool("validate_workflow", json!({ "toml": good }));
        assert!(!e, "{}", msg);
        // On another node, a target is required.
        let remote = "[[workflow]]\nname = \"r\"\n[[workflow.task]]\nid = \"a\"\nnode = \"nowhere\"\nrun = \"true\"\n";
        let (msg, e) = tool("validate_workflow", json!({ "toml": remote }));
        assert!(e, "{}", msg);
        let (msg, e) = tool("save_workflow", json!({ "toml": good, "location": repo }));
        assert!(!e, "{}", msg);
        assert!(dir.join("repo/.indra/workflows/ci.toml").is_file());
        let (list, _) = tool("list_workflows", json!({}));
        assert!(list.contains("ci (2 steps) in"), "{}", list);
        let (got, e) = tool("get_workflow", json!({ "name": "ci" }));
        assert!(!e && got.contains("when = \"test.failed\""), "{}", got);
        // Saved to the config file, keeping its comments.
        let local = "[[workflow]]\nname = \"here\"\n[[workflow.task]]\nid = \"a\"\ntarget = \"repo\"\nrun = \"echo hi\"\n";
        let (msg, e) = tool(
            "save_workflow",
            json!({ "toml": local, "location": "config" }),
        );
        assert!(!e, "{}", msg);
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(
            text.contains("# mine") && text.contains("name = \"here\""),
            "{}",
            text
        );
        // Only known repositories.
        let (msg, e) = tool(
            "save_workflow",
            json!({ "toml": local, "location": "/etc" }),
        );
        assert!(e && msg.contains("location must be"), "{}", msg);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
