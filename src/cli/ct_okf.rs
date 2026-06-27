// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-okf` command grammar (see [`crate::cli`]); the `ct-okf` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use std::path::PathBuf;

use clap::Parser;

use crate::explain::Format;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-okf",
    version,
    about = "Author and query Open Knowledge Format (OKF) bundles of Markdown concepts.",
    long_about = "ct-okf works with OKF v0.1 bundles — directory trees of Markdown concepts whose \
                  YAML frontmatter carries a required `type` plus optional metadata (also reachable \
                  as `ct okf`). Read-only verbs: --validate (a conformance verdict), --list (query \
                  concepts by --type/--tag), --show (one concept's metadata), --links (cross-link \
                  report / broken-link verdict). Authoring verbs (these write): --new, --init, \
                  --index, --log, --set. See `ct-okf --explain` for agent-oriented documentation."
)]
pub struct Cli {
    /// Bundle root (or single concept root) to operate on.
    #[arg(long, default_value = ".")]
    pub base: PathBuf,

    /// Limit selection to files whose name matches; '|'-separated alternatives, each substring->glob->regex promoted and anchored.
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

    // ----- verbs (choose exactly one) -----
    /// Check the bundle for OKF conformance and report a verdict (the default verb when no other is given).
    #[arg(long)]
    pub validate: bool,

    /// List the bundle's concepts with their metadata; filter with --type / --tag.
    #[arg(long)]
    pub list: bool,

    /// Show one concept's frontmatter (give the concept path).
    #[arg(long, value_name = "PATH")]
    pub show: Option<PathBuf>,

    /// Report the bundle's cross-links; with --strict, fail on a broken bundle-relative link.
    #[arg(long)]
    pub links: bool,

    /// Scaffold a new concept at PATH (requires --type); refuses to overwrite.
    #[arg(long, value_name = "PATH")]
    pub new: Option<PathBuf>,

    /// Scaffold a bundle root index.md (declaring okf_version) if absent.
    #[arg(long)]
    pub init: bool,

    /// (Re)generate index.md for --base from the concepts' frontmatter.
    #[arg(long)]
    pub index: bool,

    /// Prepend a dated entry to the bundle's log.md (use --log-kind to label it).
    #[arg(long, value_name = "MESSAGE")]
    pub log: Option<String>,

    /// Set or update a frontmatter field on the --file concept: FIELD=VALUE.
    #[arg(long, value_name = "FIELD=VALUE")]
    pub set: Option<String>,

    /// Run a .ctb script of new/set/log/index/init items atomically: simulate the whole batch, write only if every op succeeds.
    #[arg(long, value_name = "PATH")]
    pub script: Option<PathBuf>,

    /// With --script: simulate and print the plan, but write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// With --script: the directive prefix for script lines (default "#%").
    #[arg(long, value_name = "STR")]
    pub fence: Option<String>,

    // ----- authoring / filtering parameters -----
    /// The concept `type` for --new; also filters --list to this type.
    #[arg(long = "type", value_name = "TYPE")]
    pub type_: Option<String>,

    /// With --new: the concept title.
    #[arg(long)]
    pub title: Option<String>,

    /// With --new: the concept description (one sentence).
    #[arg(long)]
    pub description: Option<String>,

    /// With --new: tags (comma-separated); also filters --list to concepts carrying all given tags.
    #[arg(long, value_delimiter = ',')]
    pub tag: Vec<String>,

    /// With --log: the entry label (e.g. Update, Creation). Default: Update.
    #[arg(long, value_name = "LABEL")]
    pub log_kind: Option<String>,

    /// With --set: the concept file to edit.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// With --validate / --links: also treat broken bundle-relative links as failures.
    #[arg(long)]
    pub strict: bool,

    // ----- framed verdict (for --validate / --links) -----
    /// Question this check answers, framing it as a test; printed as a "== ... ==" banner unless --quiet.
    #[arg(long)]
    pub question: Option<String>,

    /// Verdict expectation over the violation count: any|none|N|=N|+N|-N. Default: none (every concept conforms / no broken links).
    #[arg(long)]
    pub expect: Option<String>,

    /// Template written to stdout after a check. Tokens: {RESULT} {QUESTION} {COUNT} {TOTAL} {BASE} {MATCHES} ({COUNT} is the violation count).
    #[arg(long, alias = "emit-stdout")]
    pub emit: Option<String>,

    /// Template written to stderr after a check (same tokens as --emit).
    #[arg(long)]
    pub emit_stderr: Option<String>,

    /// Suppress informational output; report via exit status (and --emit, which still fires).
    #[arg(long)]
    pub quiet: bool,

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
