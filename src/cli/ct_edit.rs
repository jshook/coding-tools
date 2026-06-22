// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-edit` command grammar (see [`crate::cli`]); the `ct-edit` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;
use crate::pulse::HeartbeatOpts;
use crate::{blockdoc, pattern};

#[derive(Parser, Debug)]
#[command(
    name = "ct-edit",
    version,
    about = "Find/replace across selected files, gated by an --expect verdict and previewable with --dry-run.",
    long_about = "ct-edit applies a find/replace to the files chosen by ct-search-style predicates \
                  (also reachable as `ct edit`). It computes every replacement first, classifies \
                  the total against --expect, and writes only when the verdict is SUCCESS and \
                  --dry-run is not set. --find/--replace accept file:PATH / text:VALUE payloads; \
                  a multi-line find matches as a literal block. --script runs a .ctb batch \
                  atomically: everything is verified in memory before anything is written. \
                  See `ct-edit --explain` for agent-oriented documentation."
)]
pub struct Cli {
    /// Search root (relative or absolute); a file edits just that file, a directory is descended.
    #[arg(long, default_value = ".")]
    pub base: PathBuf,

    /// Limit to files whose name matches; '|'-separated alternatives, each substring->glob->regex promoted and anchored.
    #[arg(long)]
    pub name: Option<String>,

    /// Include dot-entries (names starting with '.'); default skips them.
    #[arg(long)]
    pub hidden: bool,

    /// Follow symlinks while traversing.
    #[arg(long)]
    pub follow: bool,

    /// Walk gitignored / .ignore files too (the .git directory is always skipped); by default the walk skips what git would.
    #[arg(long)]
    pub no_ignore: bool,

    /// Pattern to find (substring->glob->regex promoted); matched per line. Accepts file:PATH / text:VALUE; a multi-line payload matches as a line-anchored literal block. Required unless --script is given.
    #[arg(long, conflicts_with = "script")]
    pub find: Option<String>,

    /// Replacement text. With a regex --find, $1/${name} expand; otherwise literal. Accepts file:PATH / text:VALUE; for a block --find, an empty payload deletes the matched lines. Required unless --script is given.
    #[arg(long, conflicts_with = "script")]
    pub replace: Option<String>,

    /// Pin how --find is interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum, conflicts_with = "script")]
    pub mode: Option<pattern::Mode>,

    /// Run a .ctb edit script: a batch of find/replace blocks verified in full before any write (see --explain).
    #[arg(long, value_name = "PATH")]
    pub script: Option<PathBuf>,

    /// Fence string opening script directive lines (for payloads that contain the default at line start).
    #[arg(long, default_value = blockdoc::DEFAULT_FENCE, requires = "script")]
    pub fence: String,

    /// Script edits match pristine content instead of cascading; overlapping edits become a usage error.
    #[arg(long, requires = "script")]
    pub no_cascade: bool,

    /// Verdict expectation over the total replacement count: any|none|N|=N|+N|-N (default: any). In scripts, per-edit expect= defaults to =1.
    #[arg(long, conflicts_with = "script")]
    pub expect: Option<String>,

    /// Show what would change and the verdict, but write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Suppress the per-site diff; print only the summary line.
    #[arg(long)]
    pub quiet: bool,

    /// Emit a structured JSON result instead of text.
    #[arg(long)]
    pub json: bool,

    /// Like `--json`, but pretty-printed (indented).
    #[arg(long)]
    pub json_pretty: bool,

    /// Abort with exit 2 if the scan exceeds SECS seconds (fractional allowed). Never interrupts the write phase: once a SUCCESS verdict starts writing, every write completes.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}
