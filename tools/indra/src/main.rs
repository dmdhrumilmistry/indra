//! indra - one CLI for AI agent harnesses.
//!
//! Named for Indra's net: an endless net with a jewel at every knot, each
//! reflecting all the others. `indra mem` keeps that net of memories, and
//! `indra serve` is a console over the agents of a harness.

mod deck;
mod mem;
mod paths;

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
        indra ls      the console's picture in the terminal\n\n\
        The security harness is the default profile; any Claude Code project or custom harness works too."
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let res = match cli.cmd {
        Cmd::Mem(a) => mem::cli::run(a),
        Cmd::Serve(a) => deck::cli::run_serve(a),
        Cmd::Ls(a) => deck::cli::run_ls(a),
        Cmd::Check(a) => deck::cli::run_check(a),
        Cmd::Init(a) => deck::cli::init(a),
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            eprintln!("indra: {}", e);
            ExitCode::FAILURE
        }
    }
}
