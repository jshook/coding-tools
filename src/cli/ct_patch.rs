// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-patch` command grammar (see [`crate::cli`]); the `ct-patch` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;
use crate::pattern;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-patch",
    version,
    about = "Set/add/delete/move nodes by path in JSON/JSONC/JSONL/YAML, preserving comments and formatting.",
    long_about = "ct-patch makes structured edits to JSON, JSONC, JSONL, and YAML files (also reachable \
                  as `ct patch`): address a node by path (keys, [N] indices, or [key=value] predicates) \
                  and --set, --add, --delete, or --move-*. JSON-family edits are byte-range splices so \
                  everything outside the changed node is preserved; YAML uses the pure-Rust yaml-edit \
                  backend. Gated by --expect and previewable with --dry-run. See `ct-patch --explain` \
                  for agent-oriented documentation."
)]
pub struct Cli {
    /// Root to patch; a file patches just that file, a directory is descended.
    #[arg(long, default_value = ".")]
    pub base: PathBuf,

    /// Limit to files whose name matches; '|'-separated alternatives, each substring->glob->regex promoted and anchored.
    #[arg(long)]
    pub name: Option<String>,

    /// Pin how --name is interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    pub mode: Option<pattern::Mode>,

    /// Include dot-entries (names starting with '.'); default skips them.
    #[arg(long)]
    pub hidden: bool,

    /// Follow symlinks while traversing.
    #[arg(long)]
    pub follow: bool,

    /// Walk gitignored / .ignore files too (the .git directory is always skipped); by default the walk skips what git would.
    #[arg(long)]
    pub no_ignore: bool,

    /// Set PATH to VALUE (repeatable). VALUE is parsed as JSON, or taken as a string if it is not valid JSON. file:PATH reads the value verbatim as a string; text:VALUE escapes the prefix.
    #[arg(long, value_name = "PATH=VALUE")]
    pub set: Vec<String>,

    /// Delete the node at PATH (repeatable).
    #[arg(long, value_name = "PATH")]
    pub delete: Vec<String>,

    /// Append VALUE to the array at PATH, no index needed (repeatable). VALUE is parsed as JSON or taken as a string; file:PATH reads it verbatim as a string.
    #[arg(long, value_name = "PATH=VALUE")]
    pub add: Vec<String>,

    /// Move the array element selected by PATH to the front of its list (repeatable).
    #[arg(long, value_name = "PATH")]
    pub move_first: Vec<String>,

    /// Move the array element selected by PATH to the end of its list (repeatable).
    #[arg(long, value_name = "PATH")]
    pub move_last: Vec<String>,

    /// Move the array element selected by PATH one position earlier (repeatable).
    #[arg(long, value_name = "PATH")]
    pub move_up: Vec<String>,

    /// Move the array element selected by PATH one position later (repeatable).
    #[arg(long, value_name = "PATH")]
    pub move_down: Vec<String>,

    /// Force the document format instead of detecting it from the file extension.
    #[arg(long, value_enum)]
    pub format: Option<DocFormat>,

    /// Verdict expectation over the total number of changes: any|none|N|=N|+N|-N (default: any).
    #[arg(long)]
    pub expect: Option<String>,

    /// Show what would change and the verdict, but write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Suppress the per-file lines; print only the summary.
    #[arg(long)]
    pub quiet: bool,

    /// Emit a structured JSON result instead of text.
    #[arg(long)]
    pub json: bool,

    /// Abort with exit 2 if the scan exceeds SECS seconds (fractional allowed). Never interrupts the write phase: once a SUCCESS verdict starts writing, every write completes.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}

/// Document format. JSON, JSONC, and JSONL parse through the same lenient
/// `jsonc-parser` tree; YAML uses the pure-Rust `yaml-edit` backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DocFormat {
    Json,
    Jsonc,
    Jsonl,
    Yaml,
}

impl DocFormat {
    /// Detect a format from a file extension.
    pub fn from_ext(ext: &str) -> Option<DocFormat> {
        match ext.to_ascii_lowercase().as_str() {
            "json" => Some(DocFormat::Json),
            "jsonc" => Some(DocFormat::Jsonc),
            "jsonl" | "ndjson" => Some(DocFormat::Jsonl),
            "yaml" | "yml" => Some(DocFormat::Yaml),
            _ => None,
        }
    }
}
