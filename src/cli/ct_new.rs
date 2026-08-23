// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-new` command grammar (see [`crate::cli`]); the `ct-new` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::blockdoc;
use crate::explain::Format;
use crate::newfile::OnExists;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-new",
    version,
    about = "Create a new file with the content you give it, refusing to clobber what is already there.",
    long_about = "ct-new creates files (also reachable as `ct new`): give it a PATH and a \
                  --content payload (file:PATH / stdin: / text:VALUE, so a heredoc or a template \
                  pastes in with no escaping) and it writes the file atomically. An existing \
                  target is refused by default — --on-exists says whether to skip it or \
                  overwrite it — and --parents is what creates a missing directory, so a typo'd \
                  path fails loudly instead of scattering directories. Framed by --expect and \
                  previewable with --dry-run. --script creates a whole batch of files from one \
                  .ctb document, all-or-nothing. See `ct-new --explain` for agent-oriented \
                  documentation."
)]
pub struct Cli {
    /// File to create. Required unless --script is given.
    pub path: Option<PathBuf>,

    /// The file's content. Accepts file:PATH / stdin: / text:VALUE, so a heredoc, a template file, or piped text needs no escaping. Omit with --empty for a zero-byte file.
    #[arg(long, conflicts_with_all = ["script", "empty"])]
    pub content: Option<String>,

    /// Create the file with no content at all (the deliberate zero-byte case; without it, a missing --content is a usage error).
    #[arg(long, conflicts_with = "script")]
    pub empty: bool,

    /// What to do when the file already exists: fail (default, nothing written), skip (accept it as-is), or overwrite (replace its contents).
    #[arg(long, value_enum, default_value_t = OnExists::Fail)]
    pub on_exists: OnExists,

    /// Create missing parent directories; without it, a missing parent is an error naming the directory.
    #[arg(long)]
    pub parents: bool,

    /// Write the content exactly as given, without adding the final newline a text file normally ends with.
    #[arg(long)]
    pub no_final_newline: bool,

    /// Create a whole batch from a .ctb document of `new` items, all-or-nothing (see --explain).
    #[arg(long, value_name = "PATH")]
    pub script: Option<PathBuf>,

    /// Fence string opening script directive lines (for payloads that contain the default at line start).
    #[arg(long, default_value = blockdoc::DEFAULT_FENCE, requires = "script")]
    pub fence: String,

    /// Verdict expectation over the number of files ensured — created, overwritten, or already present under --on-exists skip: any|none|N|=N|+N|-N (default: any). A blocked target is ERROR whatever --expect says.
    #[arg(long)]
    pub expect: Option<String>,

    /// Show what would be written, with a bounded preview of each body, but write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Suppress the per-file lines; print only the summary.
    #[arg(long)]
    pub quiet: bool,

    /// Emit a structured JSON result instead of text.
    #[arg(long)]
    pub json: bool,

    /// Like `--json`, but pretty-printed (indented).
    #[arg(long)]
    pub json_pretty: bool,

    /// Abort with exit 2 if planning exceeds SECS seconds (fractional allowed). Never interrupts the write phase: once a SUCCESS verdict starts writing, every write completes.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}
