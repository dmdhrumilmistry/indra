//! `indra mem`: graph memory. Notes are plain Markdown files with YAML
//! frontmatter, `[[wikilinks]]` and typed `rel:: [[target]]` relations; a
//! vault is an ordinary folder that also opens in Obsidian.

pub mod cli;
pub mod export;
pub mod frontmatter;
pub mod graph;
pub mod import;
pub mod note;
pub mod record;
pub mod search;
pub mod time;
pub mod tree;
pub mod vault;
