// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-check` command grammar (see [`crate::cli`]); the `ct-check` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;
use crate::pattern;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-check",
    version,
    about = "Verify the project's recorded invariants from .ct/rules.jsonc (read-only).",
    long_about = "ct-check runs the rule store's probes in order and reports each rule as SUCCESS, \
                  ERROR, WARN, PENDING, or BROKEN (also reachable as `ct check`). It never writes \
                  anything; rules are recorded with ct-rules. See `ct-check --explain` for \
                  agent-oriented documentation."
)]
pub struct Cli {
    /// Rule store. Default: the nearest .ct/rules.jsonc walking upward from the current directory.
    #[arg(long)]
    pub file: Option<PathBuf>,

    /// Select rules whose id matches (substring->glob->regex promoted, anchored).
    #[arg(long)]
    pub id: Option<String>,

    /// Pin how --id is interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    pub mode: Option<pattern::Mode>,

    /// Select rules carrying any of these tags (comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub tag: Vec<String>,

    /// Stop after the first enforced violation; remaining rules are reported as skipped.
    #[arg(long)]
    pub fail_fast: bool,

    /// Print the selected rules (id, lanes, question, tags); run nothing.
    #[arg(long)]
    pub list: bool,

    /// Suppress per-rule lines and the default summary.
    #[arg(long)]
    pub quiet: bool,

    /// Emit a structured JSON result instead of text (overrides the emit templates).
    #[arg(long)]
    pub json: bool,

    /// Per-rule template written to stdout. Tokens: {RESULT} {ID} {QUESTION} {CODE} {WHY} {CMD}.
    #[arg(long, value_name = "TEMPLATE")]
    pub emit_each: Option<String>,

    /// Summary template written to stdout. Tokens: {RESULT} {OK} {ERRORS} {WARNED} {PENDING} {BROKEN} {SKIPPED} {TOTAL} {REASON}.
    #[arg(long, alias = "emit-stdout", value_name = "TEMPLATE")]
    pub emit: Option<String>,

    /// Summary template written to stderr (same tokens as --emit).
    #[arg(long, value_name = "TEMPLATE")]
    pub emit_stderr: Option<String>,

    /// Default per-rule bound in seconds (fractional allowed); a rule's own timeout field overrides it. A timed-out probe is BROKEN.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}
