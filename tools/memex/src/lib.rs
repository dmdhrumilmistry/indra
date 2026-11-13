//! memex as a library: the note parser, vault loader, graph and exports
//! behind the `memex` CLI, for tools that want to read a vault directly
//! (agentdeck uses it to draw memory graphs).

pub mod export;
pub mod frontmatter;
pub mod graph;
pub mod note;
pub mod search;
pub mod time;
pub mod tree;
pub mod vault;
