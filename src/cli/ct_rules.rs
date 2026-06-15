// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-rules` command grammar (see [`crate::cli`]); the `ct-rules` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;

#[derive(Parser, Debug)]
#[command(
    name = "ct-rules",
    version,
    about = "Record, promote, remove, and list the project's invariant rules (.ct/rules.jsonc).",
    long_about = "ct-rules is the writing side of the invariant surface (also reachable as \
                  `ct rules`): --add verifies a probe and records it as a rule, --pending parks \
                  an aspiration, --promote enforces it once it holds, --def names shared \
                  vocabulary, --hook cargo wires `ct check` into `cargo test`. Verification of \
                  the store is ct-check's job. See `ct-rules --explain` for details."
)]
pub struct Cli {
    /// Rule store. Default: the nearest .ct/rules.jsonc walking upward (created by --init/--add when absent).
    #[arg(long)]
    pub file: Option<PathBuf>,

    /// Create .ct/rules.jsonc (commented scaffold) if it does not exist.
    #[arg(long)]
    pub init: bool,

    /// Record a rule with this id: the probe (after `--`) is gate-validated and RUN now; it must hold unless --pending.
    #[arg(long, value_name = "ID")]
    pub add: Option<String>,

    /// With --add: record an aspiration that does not yet hold; reported as PENDING, never enforced, until --promote.
    #[arg(long)]
    pub pending: bool,

    /// With --add: the question this rule answers (required).
    #[arg(long)]
    pub question: Option<String>,

    /// With --add: why this invariant exists; printed whenever it fails. Accepts file:PATH / text:VALUE payloads.
    #[arg(long)]
    pub why: Option<String>,

    /// With --add: the verbatim human request behind this rule, retained in the store so the intent can be revisited; strip all prompts later with --flatten. Accepts file:PATH / text:VALUE payloads.
    #[arg(long)]
    pub prompt: Option<String>,

    /// With --add: tags for selection (comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub tag: Vec<String>,

    /// With --add: fail (default) or warn (violations report but never redden the exit).
    #[arg(long)]
    pub severity: Option<String>,

    /// With --add: outcome adapter for bridge probes: exit (default) or empty.
    #[arg(long, value_name = "exit|empty")]
    pub expect: Option<String>,

    /// With --add: matcher adapter — the rule holds when this pattern appears in the probe's output.
    #[arg(long, value_name = "PATTERN")]
    pub expect_ok: Option<String>,

    /// With --add: matcher adapter — a violation when this pattern appears in the probe's output.
    #[arg(long, value_name = "PATTERN")]
    pub expect_err: Option<String>,

    /// With --add: permit network access where the bridge entry deems it meaningful (cargo deny).
    #[arg(long)]
    pub network: bool,

    /// With --add: per-rule probe bound in seconds (fractional allowed).
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    /// Re-run a pending rule's probe; if it now holds, clear the pending flag (enforce it).
    #[arg(long, value_name = "ID")]
    pub promote: Option<String>,

    /// Remove the rule with this exact id.
    #[arg(long, value_name = "ID")]
    pub remove: Option<String>,

    /// Set a def: NAME=VALUE. VALUE is parsed as JSON (e.g. ["A","B"]) or taken as a string.
    #[arg(long, value_name = "NAME=VALUE")]
    pub def: Option<String>,

    /// Print defs and rules without changing anything.
    #[arg(long)]
    pub list: bool,

    /// Strip the retained "prompt" prose from every rule, leaving only the mechanical definitions.
    #[arg(long)]
    pub flatten: bool,

    /// Write the build hook for an ecosystem (currently: cargo — a tests/ shim that runs `ct check`).
    #[arg(long, value_name = "ECOSYSTEM")]
    pub hook: Option<String>,

    /// Suppress informational output.
    #[arg(long)]
    pub quiet: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,

    /// The probe for --add (after `--`): an argv run directly, never through a shell.
    #[arg(last = true, value_name = "PROBE...")]
    pub probe: Vec<String>,
}
