//! indra - one CLI for AI agent harnesses.
//!
//! Named for Indra's net: an endless net with a jewel at every knot, each
//! reflecting all the others. `indra mem` keeps that net of memories, and
//! `indra serve` is a console over the agents of a harness.

mod completions;
mod deck;
mod findings;
mod mem;
mod paths;
mod publish;
mod review;
mod site;
mod skills;
mod update;

use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "indra",
    version,
    about = "One CLI for AI agent harnesses: graph memory and a web console to view, monitor and run agents",
    long_about = "One CLI for AI agent harnesses.\n\n\
        indra mem     graph memory: Markdown notes with typed [[links]], ranked recall, Obsidian-compatible\n\
        indra serve   web console: agents, skills, prompts, graph maps, memory, runs, launch and watch agents\n\
        indra ls      the console's picture in the terminal\n\
        indra nodes   the machines agents run on, local and over SSH\n\
        indra run     run a multi-step, multi-node workflow and follow it\n\n\
        The security harness is the default profile; any Claude Code project or custom harness works too.",
    after_help = "Start here:\n  \
        indra serve --open           open the console in your browser\n  \
        indra claude -m qwen3-coder  Claude Code on a local model (Ollama)\n  \
        indra update                 get the newest version (--auto on: by itself)\n  \
        indra completions --install  Tab completion in your shell"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Graph memory: add, link, recall, tree, export... (`indra mem --help`)
    #[command(
        alias = "m",
        long_about = "Graph memory for people and AI agents.\n\n\
            Notes are plain Markdown with [[wikilinks]] and typed relations (`mitigated-by:: [[note]]`). \
            The vault is an ordinary folder that also opens in Obsidian.\n\n\
            Vault location: --vault, else $INDRA_VAULT, else the nearest parent folder holding `.indra/`, \
            else a per-user default (see `indra mem where`)."
    )]
    Mem(mem::cli::MemArgs),
    /// Start the web console (prints a URL with an access token)
    Serve(deck::cli::ServeArgs),
    /// Summarise harnesses, agents, skills, prompts and runs in the terminal
    Ls(deck::cli::LsArgs),
    /// Validate the console configuration and report what was found
    Check(deck::cli::CheckArgs),
    /// Write a commented starter indra.toml for custom harnesses and runners
    Init(deck::cli::InitArgs),
    /// Check the machines agents run on: this one and every [[node]] over SSH
    Nodes(deck::cli::NodesArgs),
    /// Run a workflow from indra.toml and follow it here (omit the name to list them)
    Run(deck::cli::RunArgs),
    /// The remote side of a node; the hub runs `indra node ...` over SSH
    Node(deck::cli::NodeArgs),
    /// Secrets for agents' profiles: set (value from stdin), list, rm
    Secret(deck::cli::SecretArgs),
    /// List the agents a running console knows about (`indra attach` steers one)
    Agents(deck::cli::AgentsArgs),
    /// Follow an agent of a running console and steer it from this terminal
    Attach(deck::cli::AttachArgs),
    /// Claude Code in this terminal on a model served by Ollama (or only its subagents there)
    Claude(deck::claude::ClaudeArgs),
    /// Publish plans for Jira, Confluence, GitHub Issues or Notion from a review run
    Publish(publish::PublishArgs),
    /// PR review support: the review cache, local metrics, posting to GitHub
    Review(review::ReviewArgs),
    /// Sync the skill mirror, or install the skills for an agent
    Skills(skills::SkillsArgs),
    /// Update indra to the newest release (checks its SHA-256 first); --auto on keeps it up to date
    Update(update::UpdateArgs),
    /// Tab completion for your shell: print the script, or --install it
    Completions(completions::CompletionsArgs),
    /// MCP server for AI agents to design workflows (`claude mcp add indra -- indra mcp`)
    Mcp(deck::cli::McpArgs),
    /// Accounts agents run with: what each used today and this month, and its limits
    Accounts(deck::cli::AccountsArgs),
    /// Console users and roles: add, list, passwd, role, rm
    User(deck::users::UserArgs),
    /// Maintainer tools: build the website
    #[command(hide = true)]
    Dev {
        #[command(subcommand)]
        cmd: DevCmd,
    },
}

#[derive(Subcommand)]
enum DevCmd {
    /// Build the website (landing page + docs) into _site
    Site(site::SiteArgs),
    /// Find em and en dashes in the repository (exit 1 if any), or --fix them
    Dashes {
        /// The repository (default: this folder)
        #[arg(long)]
        repo: Option<std::path::PathBuf>,
        #[arg(long)]
        fix: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Say what the daily release check found, and start today's. Not for
    // update and completions themselves, nor the remote side of a node
    // (its output is read by the hub).
    if !matches!(
        cli.cmd,
        Cmd::Update(_) | Cmd::Completions(_) | Cmd::Node(_) | Cmd::Mcp(_) | Cmd::Dev { .. }
    ) {
        update::on_start();
    }
    let res = match cli.cmd {
        Cmd::Mem(a) => mem::cli::run(a),
        Cmd::Serve(a) => deck::cli::run_serve(a),
        Cmd::Ls(a) => deck::cli::run_ls(a),
        Cmd::Check(a) => deck::cli::run_check(a),
        Cmd::Init(a) => deck::cli::init(a),
        Cmd::Nodes(a) => deck::cli::run_nodes(a),
        Cmd::Run(a) => deck::cli::run_workflow(a),
        Cmd::Node(a) => deck::cli::run_node(a),
        Cmd::Secret(a) => deck::cli::run_secret(a),
        Cmd::Agents(a) => deck::cli::run_agents(a),
        Cmd::Attach(a) => deck::cli::run_attach(a),
        Cmd::Claude(a) => deck::claude::run(a),
        Cmd::Update(a) => update::run(a),
        Cmd::Completions(a) => completions::run(a),
        Cmd::Mcp(a) => deck::cli::run_mcp(a),
        Cmd::Accounts(a) => deck::cli::run_accounts(a),
        Cmd::User(a) => deck::users::run(a),
        Cmd::Publish(a) => publish::run(a),
        Cmd::Review(a) => review::run(a),
        Cmd::Skills(a) => skills::run(a),
        Cmd::Dev {
            cmd: DevCmd::Site(a),
        } => site::run(a),
        Cmd::Dev {
            cmd: DevCmd::Dashes { repo, fix },
        } => site::dashes(repo, fix),
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            eprintln!("indra: {}", e);
            ExitCode::FAILURE
        }
    }
}
