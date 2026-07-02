// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-survey` command grammar (see [`crate::cli`]); the `ct-survey` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;
use crate::pulse::HeartbeatOpts;
use crate::survey::{Depth, GroupKind, SortKey};

#[derive(Parser, Debug)]
#[command(
    name = "ct-survey",
    version,
    about = "Survey a codebase by its build-system units: crates and modules, with file/line/test counts.",
    long_about = "ct-survey reports a format-contextualized survey of a codebase (also reachable as \
                  `ct survey`): for Rust, the workspace -> crate -> module hierarchy, each element \
                  carrying file, line, and test counts. Crate identity, workspace membership, and \
                  cargo target kinds are authoritative (read from `cargo metadata`); file/line counts \
                  are exact; the module bucketing and the #[test] tally are heuristic (marked ~ in the \
                  output). With no --group, the contextual group type is inferred from the given path's \
                  Cargo.toml (a [workspace] table vs a lone [package]). See `ct-survey --explain` for \
                  agent-oriented documentation."
)]
pub struct Cli {
    /// Path to survey: a directory, or a Cargo.toml. Defaults to the current directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Contextual group type; inferred from the path's Cargo.toml when omitted.
    #[arg(long, value_enum)]
    pub group: Option<GroupKind>,

    /// How deep to descend: crate (per-crate only) or module (per-crate then per-module).
    #[arg(long, value_enum, default_value_t = Depth::Module)]
    pub depth: Depth,

    /// Sort crates and modules by this key: name (ascending) or files/lines/tests (largest first).
    #[arg(long, value_enum, default_value_t = SortKey::Name)]
    pub sort: SortKey,

    /// Emit a structured JSON result instead of text.
    #[arg(long)]
    pub json: bool,

    /// Like `--json`, but pretty-printed (indented).
    #[arg(long)]
    pub json_pretty: bool,

    /// Abort with exit 2 if the run exceeds SECS seconds (fractional allowed).
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}
