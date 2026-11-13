# tools

Rust tools that ship with security-harness. Each one is a standalone binary that also works
outside this repository.

| Tool | What it is |
|---|---|
| [`memex`](memex/README.md) | A graph-based memory: Markdown notes with typed `[[links]]`, Obsidian-compatible, with ranked recall for agents. CLI. |
| [`agentdeck`](agentdeck/README.md) | A local web console for agent harnesses: agents, skills and prompts, graph maps, memory, run monitoring, and launching agents with a live view. |

Install either one:

```bash
cargo install --locked --git https://github.com/dmdhrumilmistry/security-harness memex
cargo install --locked --git https://github.com/dmdhrumilmistry/security-harness agentdeck
```

or download a prebuilt binary from a `memex-v*` / `agentdeck-v*`
[release](https://github.com/dmdhrumilmistry/security-harness/releases).

This folder is a Cargo workspace (agentdeck uses memex as a library):

```bash
cd tools
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

`.github/workflows/tools.yml` runs the same checks on Linux, macOS and Windows, and builds
release binaries when a tool's tag is pushed.
