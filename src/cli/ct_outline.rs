// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-outline` command grammar (see [`crate::cli`]); the `ct-outline` bin
//! is a thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;
use crate::pattern;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-outline",
    version,
    about = "Report the declarations in a file or tree: kind, name, start:end span, and nesting.",
    long_about = "ct-outline detects declarations heuristically per language (Rust, Python, Markdown) \
                  and reports each with its kind, name, and 1-based start:end line span (also \
                  reachable as `ct outline`) — locate a symbol, then read exactly that region with \
                  ct-view --range. Start lines are exact; an underivable end renders as start:?. \
                  See `ct-outline --explain` for agent-oriented documentation."
)]
pub struct Cli {
    /// Root to outline; a file outlines just that file, a directory is descended.
    #[arg(long, default_value = ".")]
    pub base: PathBuf,

    /// Limit to files whose name matches; '|'-separated alternatives, each substring->glob->regex promoted and anchored.
    #[arg(long)]
    pub name: Option<String>,

    /// Restrict to these extensions (comma-separated, no dots), e.g. --ext rs,py. Combined with --name as alternatives.
    #[arg(long, value_delimiter = ',')]
    pub ext: Vec<String>,

    /// Include dot-entries (names starting with '.'); default skips them.
    #[arg(long)]
    pub hidden: bool,

    /// Follow symlinks while traversing.
    #[arg(long)]
    pub follow: bool,

    /// Walk gitignored / .ignore files too (the .git directory is always skipped); by default the walk skips what git would.
    #[arg(long)]
    pub no_ignore: bool,

    /// Keep entries whose name matches (substring->glob->regex promoted, anchored to the whole declaration name).
    #[arg(long = "match")]
    pub pattern: Option<String>,

    /// Pin how --match/--name patterns are interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    pub mode: Option<pattern::Mode>,

    /// Keep entries of these kinds (comma-separated), e.g. --kind fn,struct. Kinds are per-language keywords.
    #[arg(long, value_delimiter = ',')]
    pub kind: Vec<String>,

    /// Keep entries nested at most N levels deep (1 = top-level only).
    #[arg(long)]
    pub depth: Option<usize>,

    /// Output one grep-friendly row per matched entry: path:start:end:kind:name.
    #[arg(long)]
    pub flat: bool,

    /// Question this outline answers, framing it as a test; printed as a "== ... ==" banner unless --quiet.
    #[arg(long)]
    pub question: Option<String>,

    /// Verdict expectation over the matched-entry count: any|none|N|=N|+N|-N (default: any).
    #[arg(long)]
    pub expect: Option<String>,

    /// Template written to stdout after the outline. Tokens: {RESULT} {QUESTION} {COUNT} {BASE} {MATCHES}.
    #[arg(long, alias = "emit-stdout")]
    pub emit: Option<String>,

    /// Template written to stderr after the outline (same tokens as --emit).
    #[arg(long)]
    pub emit_stderr: Option<String>,

    /// Print nothing; report via exit status (and --emit, which still fires).
    #[arg(long)]
    pub quiet: bool,

    /// Emit a structured JSON result instead of text (overrides the text modes and --emit).
    #[arg(long)]
    pub json: bool,

    /// Abort with exit 2 if the run exceeds SECS seconds (fractional allowed).
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}
