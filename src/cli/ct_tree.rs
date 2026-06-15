// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-tree` command grammar (see [`crate::cli`]); the `ct-tree` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;
use crate::pattern;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-tree",
    version,
    about = "Report a file tree with per-file line/word/char counts, filtered, sorted, and summarised.",
    long_about = "ct-tree walks a directory for chosen file types and reports the effective tree with \
                  per-file line, word, and character counts (also reachable as `ct tree`). Filter by \
                  metric predicates (--min-lines etc.) and per-folder counts, sort by any column, and \
                  choose a summarisation level (--tree, --flat, --summary). See `ct-tree --explain` \
                  for agent-oriented documentation."
)]
#[command(group = clap::ArgGroup::new("output_mode")
    .args(["tree", "flat", "summary"])
    .multiple(false))]
pub struct Cli {
    /// Root to walk (relative or absolute), independent of the current directory.
    #[arg(long, default_value = ".")]
    pub base: PathBuf,

    /// File-name pattern; '|'-separated alternatives, each substring->glob->regex promoted and anchored.
    #[arg(long)]
    pub name: Option<String>,

    /// Pin how --name/--ext patterns are interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    pub mode: Option<pattern::Mode>,

    /// Restrict to these extensions (comma-separated, no dots), e.g. --ext rs,toml. Combined with --name as alternatives.
    #[arg(long, value_delimiter = ',')]
    pub ext: Vec<String>,

    /// Include dot-entries (names starting with '.'); default skips them.
    #[arg(long)]
    pub hidden: bool,

    /// Follow symlinks while traversing.
    #[arg(long)]
    pub follow: bool,

    /// Only include files with at least N lines.
    #[arg(long)]
    pub min_lines: Option<u64>,
    /// Only include files with at most N lines.
    #[arg(long)]
    pub max_lines: Option<u64>,
    /// Only include files with at least N words.
    #[arg(long)]
    pub min_words: Option<u64>,
    /// Only include files with at most N words.
    #[arg(long)]
    pub max_words: Option<u64>,
    /// Only include files with at least N characters.
    #[arg(long)]
    pub min_chars: Option<u64>,
    /// Only include files with at most N characters.
    #[arg(long)]
    pub max_chars: Option<u64>,

    /// Only include folders that directly contain at least N matching files.
    #[arg(long)]
    pub min_files_per_folder: Option<usize>,
    /// Only include folders that directly contain at most N matching files.
    #[arg(long)]
    pub max_files_per_folder: Option<usize>,

    /// Sort key: path, name, lines, words, chars, or ext.
    #[arg(long, value_enum, default_value_t = SortKey::Path)]
    pub sort: SortKey,
    /// Sort descending instead of ascending.
    #[arg(long)]
    pub desc: bool,

    /// Output mode: an indented file tree with per-file and per-folder counts (default).
    #[arg(long)]
    pub tree: bool,
    /// Output mode: one matching file per line with its counts.
    #[arg(long)]
    pub flat: bool,
    /// Output mode: aggregate counts only, grouped by --group.
    #[arg(long)]
    pub summary: bool,

    /// Grouping for --summary: ext, dir, or none (grand total only).
    #[arg(long, value_enum, default_value_t = GroupBy::Ext)]
    pub group: GroupBy,

    /// Emit a structured JSON result instead of text.
    #[arg(long)]
    pub json: bool,

    /// Abort with exit 2 if the report exceeds SECS seconds (fractional allowed).
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SortKey {
    Path,
    Name,
    Lines,
    Words,
    Chars,
    Ext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GroupBy {
    Ext,
    Dir,
    None,
}
