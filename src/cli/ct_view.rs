// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-view` command grammar (see [`crate::cli`]); the `ct-view` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;
use crate::pattern;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-view",
    version,
    about = "Show a file's lines by range, or the regions around a pattern with context.",
    long_about = "ct-view is a focused, bounded reader for a single file (also reachable as \
                  `ct view`): print a line range with --range, or the windows around a \
                  --match pattern with --context lines, rather than dumping the whole file. \
                  See `ct-view --explain` for agent-oriented documentation."
)]
pub struct Cli {
    /// File to view.
    pub path: PathBuf,

    /// Line range A:B (1-based, inclusive); also A: (to end), :B (from start), or A (one line).
    #[arg(long)]
    pub range: Option<String>,

    /// Show only lines matching this pattern (substring->glob->regex promoted), with --context around each. Repeatable: several --match patterns are shown as one merged, de-duplicated set of windows. Accepts file:PATH / stdin: / text:VALUE; a multi-line pattern matches as a line-anchored literal block.
    #[arg(long = "match")]
    pub pattern: Vec<String>,

    /// How the pattern is interpreted: literal, glob, regex, or auto (the promoting default, stated out loud).
    #[arg(long, value_enum)]
    pub mode: Option<pattern::Mode>,

    /// Lines of context shown around each --match hit.
    #[arg(long, short = 'C', default_value_t = 2)]
    pub context: usize,

    /// Cap the number of lines emitted.
    #[arg(long)]
    pub limit: Option<usize>,

    /// Abort with exit 2 if the view exceeds SECS seconds (fractional allowed).
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Suppress the line-number gutter in text output.
    #[arg(long)]
    pub plain: bool,

    /// OKF: show only the concept's frontmatter block.
    #[arg(long)]
    pub frontmatter: bool,

    /// OKF: omit the concept's frontmatter block from the shown lines.
    #[arg(long)]
    pub no_frontmatter: bool,

    /// Emit a structured JSON result instead of text.
    #[arg(long)]
    pub json: bool,

    /// Like `--json`, but pretty-printed (indented).
    #[arg(long)]
    pub json_pretty: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}
