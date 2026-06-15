// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-await` command grammar (see [`crate::cli`]); the `ct-await` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use clap::Parser;

use crate::explain::Format;
use crate::pattern;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-await",
    version,
    about = "Poll a read-only probe until it succeeds, an abort pattern appears, or the bound expires.",
    long_about = "ct-await runs a gated read-only probe every --every seconds until the condition \
                  is established — probe exit 0, or a required --ok-match appearing in its output — \
                  or until an --err-match appears (immediate ERROR) or the required --timeout \
                  expires (ERROR). Observe an external process's effects without owning its \
                  execution (also reachable as `ct await`). See `ct-await --explain` for \
                  agent-oriented documentation."
)]
pub struct Cli {
    /// Question this wait answers; printed as a "== ... ==" banner.
    #[arg(long)]
    pub question: Option<String>,

    /// Seconds between probe runs (fractional allowed).
    #[arg(long, value_name = "SECS", default_value_t = 5.0)]
    pub every: f64,

    /// Hard bound on the whole wait (fractional allowed). Required: a wait is bounded by design.
    #[arg(long, value_name = "SECS")]
    pub timeout: f64,

    /// SUCCESS when this pattern (substring->glob->regex promoted) appears in the probe's output. When supplied it is the REQUIRED proof: a clean exit without it means "not yet".
    #[arg(long, value_name = "PATTERN")]
    pub ok_match: Option<String>,

    /// End the wait immediately with ERROR when this pattern appears in the probe's output (decisive over --ok-match, exactly as in ct-test).
    #[arg(long, value_name = "PATTERN")]
    pub err_match: Option<String>,

    /// Pin how matcher patterns are interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    pub mode: Option<pattern::Mode>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Template written to stdout when the wait ends. Tokens: {RESULT} {ELAPSED} {TICKS} {REASON} {QUESTION} {CMD}.
    #[arg(long, alias = "emit-stdout")]
    pub emit: Option<String>,

    /// Template written to stderr when the wait ends (same tokens as --emit).
    #[arg(long)]
    pub emit_stderr: Option<String>,

    /// Suppress the banner and the default outcome line.
    #[arg(long)]
    pub quiet: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,

    /// The probe (after `--`): an argv run directly each tick, never through a shell. Exit 0 ends the wait with SUCCESS; any other exit means "not yet".
    #[arg(last = true, value_name = "PROBE...")]
    pub probe: Vec<String>,
}
