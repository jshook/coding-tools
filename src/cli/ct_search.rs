// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-search` command grammar (see [`crate::cli`]); the `ct-search` bin is
//! a thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;
use crate::pattern;
use crate::pulse::HeartbeatOpts;
use crate::walk::EntryType;

#[derive(Parser, Debug)]
#[command(
    name = "ct-search",
    version,
    about = "Recursively find files by name, type, size, and content from a chosen root.",
    long_about = "ct-search combines the predicates you would otherwise assemble from find, xargs, \
                  and grep into one declarative command (also reachable as `ct search`). An entry \
                  matches only when every supplied predicate holds. See `ct-search --explain` for \
                  agent-oriented documentation."
)]
#[command(group = clap::ArgGroup::new("output_mode")
    .args(["list", "summary", "detail", "quiet"])
    .multiple(false))]
pub struct Cli {
    /// Search root (relative or absolute), independent of the current directory.
    #[arg(long, default_value = ".")]
    pub base: PathBuf,

    /// File-name pattern; '|'-separated alternatives, each substring->glob->regex promoted and anchored to the whole name.
    #[arg(long)]
    pub name: Option<String>,

    /// Restrict to entry kinds: f=file, d=dir, l=symlink (repeatable or comma-joined).
    #[arg(long, value_enum, value_delimiter = ',')]
    pub r#type: Vec<EntryType>,

    /// Content pattern (substring->glob->regex promoted); searches file contents. Accepts file:PATH / text:VALUE; a multi-line pattern matches as a line-anchored literal block.
    #[arg(long)]
    pub grep: Option<String>,

    /// Pin how patterns are interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    pub mode: Option<pattern::Mode>,

    /// Size predicate [+|-]N[k|m|g]: +N larger than, -N smaller than, N at least N.
    #[arg(long)]
    pub size: Option<String>,

    /// Include dot-entries (names starting with '.'); default skips them.
    #[arg(long)]
    pub hidden: bool,

    /// Follow symlinks while traversing.
    #[arg(long)]
    pub follow: bool,

    /// Walk gitignored / .ignore files too (the .git directory is always skipped); by default the walk skips what git would.
    #[arg(long)]
    pub no_ignore: bool,

    /// Stop after N matches.
    #[arg(long)]
    pub limit: Option<usize>,

    /// Abort with exit 2 if the search exceeds SECS seconds (fractional allowed).
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Question this search answers, framing it as a test; printed as a "== ... ==" banner unless --quiet.
    #[arg(long)]
    pub question: Option<String>,

    /// Verdict expectation over the match count: any|none|N|=N|+N|-N (default: any). Turns the search into a pass/fail test whose exit status follows the verdict.
    #[arg(long)]
    pub expect: Option<String>,

    /// Template written to stdout after the search. Tokens: {RESULT} {QUESTION} {COUNT} {LINES} {BASE} {MATCHES}.
    #[arg(long, alias = "emit-stdout")]
    pub emit: Option<String>,

    /// Template written to stderr after the search (same tokens as --emit).
    #[arg(long)]
    pub emit_stderr: Option<String>,

    /// Output mode: print one matching path per line (default).
    #[arg(long)]
    pub list: bool,

    /// Output mode: print counts only.
    #[arg(long)]
    pub summary: bool,

    /// Output mode: print matches plus, for --grep, each hit as path:line:text.
    #[arg(long)]
    pub detail: bool,

    /// Output mode: print nothing; report via exit status only.
    #[arg(long)]
    pub quiet: bool,

    /// Emit a structured JSON result instead of text (overrides the output mode and --emit).
    #[arg(long)]
    pub json: bool,

    /// Like `--json`, but pretty-printed (indented).
    #[arg(long)]
    pub json_pretty: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}
