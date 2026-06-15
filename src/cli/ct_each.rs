// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-each` command grammar (see [`crate::cli`]); the `ct-each` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;
use crate::pattern;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-each",
    version,
    about = "Run a command template once per item (no shell), with per-item verdicts and an aggregate --expect.",
    long_about = "ct-each dispatches one command over a set of distinct items: {ITEM} and {INDEX} \
                  expand inside the argv elements after `--`, each expansion is launched directly \
                  (never through a shell), each run is classified by exit status, and the SUCCESS \
                  count is judged against --expect (also reachable as `ct each`). See \
                  `ct-each --explain` for agent-oriented documentation."
)]
pub struct Cli {
    /// Items to dispatch over, in order (repeatable; one run per item). file:PATH expands to the file's non-empty lines; text:VALUE is one literal item.
    #[arg(long, num_args = 1.., value_name = "ITEM")]
    pub items: Vec<String>,

    /// Pin how --name/--ext walker patterns are interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    pub mode: Option<pattern::Mode>,

    /// Also read items from standard input, one per line (blank lines skipped), after any walker items.
    #[arg(long)]
    pub stdin: bool,

    /// Walker item source: files under this root become items (paths). A file yields itself; a directory is descended.
    #[arg(long)]
    pub base: Option<PathBuf>,

    /// Walker item source: limit to files whose name matches; '|'-separated alternatives, each substring->glob->regex promoted and anchored. Implies --base . when --base is absent.
    #[arg(long)]
    pub name: Option<String>,

    /// Walker item source: restrict to these extensions (comma-separated, no dots). Combined with --name as alternatives. Implies --base . when --base is absent.
    #[arg(long, value_delimiter = ',')]
    pub ext: Vec<String>,

    /// Include dot-entries while walking; default skips them.
    #[arg(long)]
    pub hidden: bool,

    /// Follow symlinks while walking.
    #[arg(long)]
    pub follow: bool,

    /// Question this sweep answers; printed as a "== ... ==" banner.
    #[arg(long)]
    pub question: Option<String>,

    /// Expectation over the per-item SUCCESS count: all|any|none|N|=N|+N|-N (default: all).
    #[arg(long)]
    pub expect: Option<String>,

    /// Stop after the first per-item ERROR; remaining items are reported as skipped.
    #[arg(long)]
    pub fail_fast: bool,

    /// Permit the suite's mutating tools (ct-edit, ct-patch) as the command.
    #[arg(long)]
    pub mutating: bool,

    /// Print each expanded command without running anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Per item: kill the run and classify that item ERROR after SECS seconds (fractional allowed); its {CODE} becomes "timeout".
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Per-item template written to stdout. Tokens: {RESULT} {ITEM} {INDEX} {CODE} {CMD} {STDOUT} {STDERR}. Default (unless --quiet): "{RESULT} {ITEM}".
    #[arg(long, value_name = "TEMPLATE")]
    pub emit_each: Option<String>,

    /// Summary template written to stdout after the sweep. Tokens: {RESULT} {OK} {ERRORS} {SKIPPED} {TOTAL} {QUESTION} {EXPECT} {REASON}.
    #[arg(long, alias = "emit-stdout", value_name = "TEMPLATE")]
    pub emit: Option<String>,

    /// Summary template written to stderr (same tokens as --emit).
    #[arg(long, value_name = "TEMPLATE")]
    pub emit_stderr: Option<String>,

    /// Also pass each child's stdout/stderr through verbatim.
    #[arg(long)]
    pub show_output: bool,

    /// Suppress the question banner, the default per-item lines, and the default summary.
    #[arg(long)]
    pub quiet: bool,

    /// Emit a structured JSON result instead of text (overrides the emit templates).
    #[arg(long)]
    pub json: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,

    /// Command and arguments run per item (after `--`); {ITEM} and {INDEX} expand in every element.
    #[arg(last = true, value_name = "CMD [ARGS...]")]
    pub command: Vec<String>,
}
