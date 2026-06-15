// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-test` command grammar (see [`crate::cli`]); the `ct-test` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use clap::Parser;

use crate::explain::Format;
use crate::pattern;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-test",
    version,
    about = "Run a command as a framed experiment and emit a templated SUCCESS/ERROR verdict.",
    long_about = "ct-test frames a command with the question it answers, classifies the result from \
                  what the command prints (not only its exit code), and emits a templated verdict \
                  (also reachable as `ct test`). The command is always launched directly — there is \
                  no shell mode. See `ct-test --explain` for agent-oriented documentation."
)]
pub struct Cli {
    /// Question this experiment answers; printed as a "== ... ==" banner.
    #[arg(long)]
    pub question: Option<String>,

    /// Program to run (must be on the fixed read-only allowlist).
    #[arg(long)]
    pub cmd: Option<String>,

    /// Text written to the child's standard input. Accepts file:PATH / text:VALUE payloads.
    #[arg(long)]
    pub stdin: Option<String>,

    /// Pin how matcher patterns are interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    pub mode: Option<pattern::Mode>,

    /// Kill the command and classify ERROR if it runs longer than SECS seconds (fractional allowed); {CODE} becomes "timeout".
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Match in stdout OR stderr forces ERROR (synonym for the -stdout/-stderr pair).
    #[arg(long)]
    pub err_match: Option<String>,

    /// Match in stdout forces ERROR.
    #[arg(long)]
    pub err_match_stdout: Option<String>,

    /// Match in stderr forces ERROR.
    #[arg(long)]
    pub err_match_stderr: Option<String>,

    /// Match in stdout OR stderr indicates SUCCESS (synonym for the -stdout/-stderr pair).
    #[arg(long)]
    pub ok_match: Option<String>,

    /// Match in stdout indicates SUCCESS.
    #[arg(long)]
    pub ok_match_stdout: Option<String>,

    /// Match in stderr indicates SUCCESS.
    #[arg(long)]
    pub ok_match_stderr: Option<String>,

    /// Verdict when neither an --ok-match nor an --err-match matched: success, error, or exit (follow the exit code). Default: error if any --ok-match was given, else exit.
    #[arg(long, value_enum)]
    pub otherwise: Option<Otherwise>,

    /// Distil captured output to lines matching this pattern (with --context around each), printed to stderr and available as {FOCUS}.
    #[arg(long)]
    pub focus: Option<String>,

    /// Lines of context shown around each --focus match.
    #[arg(long, default_value_t = 2)]
    pub context: usize,

    /// Keep only the last N lines of each captured stream in the {STDOUT}/{STDERR} emit tokens (matchers and --focus still see everything).
    #[arg(long, value_name = "N")]
    pub capture_tail: Option<usize>,

    /// Template written to stdout after running. Tokens: {RESULT} {CODE} {QUESTION} {CMD} {STDOUT} {STDERR} {REASON} {FOCUS}.
    #[arg(long, alias = "emit-stdout")]
    pub emit: Option<String>,

    /// Template written to stderr after running (same tokens as --emit).
    #[arg(long)]
    pub emit_stderr: Option<String>,

    /// Also pass the child's stdout/stderr through verbatim.
    #[arg(long)]
    pub show_output: bool,

    /// Suppress the question banner.
    #[arg(long)]
    pub quiet: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,

    /// Arguments passed through to --cmd (after `--`).
    #[arg(last = true)]
    pub args: Vec<String>,
}

/// What an *inconclusive* run resolves to — neither an `--ok-match` nor an
/// `--err-match` fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Otherwise {
    /// Treat an inconclusive run as `SUCCESS`.
    Success,
    /// Treat an inconclusive run as `ERROR` (fail-closed).
    Error,
    /// Follow the child's exit status (`0` ⇒ `SUCCESS`).
    Exit,
}

impl Otherwise {
    pub fn label(self) -> &'static str {
        match self {
            Otherwise::Success => "success",
            Otherwise::Error => "error",
            Otherwise::Exit => "exit",
        }
    }
}
