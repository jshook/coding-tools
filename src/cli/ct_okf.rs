// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-okf` command grammar (see [`crate::cli`]). Unlike the other leaf
//! tools — which are flat-flag — `ct-okf` is **subcommand**-shaped (`ct okf
//! search`, `ct okf roots add`, …), because its surface spans querying,
//! root management, index maintenance, and authoring. The `ct-okf` bin is a
//! parse-and-dispatch wrapper over this `Cli`.
//!
//! Global flags (`--json`, `--quiet`, `--base`, the walker vocabulary, …) are
//! declared `global` so they may appear before or after the subcommand; the
//! per-verb flags live on each subcommand's args struct.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::explain::Format;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-okf",
    version,
    about = "Author, query, and index Open Knowledge Format bundles across a project's content roots.",
    long_about = "ct-okf manages Open Knowledge Format (OKF) knowledge for a project (also reachable \
                  as `ct okf`). It works over the project's configured content roots and keeps a \
                  lazily-maintained full-text index so `ct okf search` is always current. Pick a \
                  subcommand: search/find query, roots/index/init configure, validate/links check, \
                  show/add/mv/set/log/gen-index/script author. See `ct-okf --explain` for \
                  agent-oriented documentation."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Search root for bundle-scoped verbs (validate/links/find/show), and the
    /// directory project discovery starts from for search/index/roots/init.
    #[arg(long, default_value = ".", global = true)]
    pub base: PathBuf,

    /// Limit to files whose name matches; '|'-separated alternatives.
    #[arg(long, global = true)]
    pub name: Option<String>,

    /// Include dot-entries (names starting with '.').
    #[arg(long, global = true)]
    pub hidden: bool,

    /// Follow symlinks while traversing.
    #[arg(long, global = true)]
    pub follow: bool,

    /// Walk gitignored / .ignore files too (the .git directory is always skipped).
    #[arg(long, global = true)]
    pub no_ignore: bool,

    /// Emit a structured JSON result instead of text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Like --json, but pretty-printed (indented).
    #[arg(long, global = true)]
    pub json_pretty: bool,

    /// Suppress informational output (exit status and --emit still report).
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Abort with exit 2 if the run exceeds SECS seconds (fractional allowed).
    #[arg(long, value_name = "SECS", global = true)]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}

/// The `ct-okf` verbs.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Full-text search the project's OKF content roots (auto-updates the index first).
    Search(SearchArgs),
    /// List concepts by metadata (--type / --tag) in the --base bundle.
    Find(FindArgs),
    /// Manage the project's OKF content roots.
    Roots(RootsArgs),
    /// Maintain the search index.
    Index(IndexArgs),
    /// Onboard: discover content roots and record them (optionally write markers).
    Init(InitArgs),
    /// Judge the --base bundle's OKF conformance (framed verdict).
    Validate(CheckArgs),
    /// Report broken bundle cross-links (framed verdict).
    Links(CheckArgs),
    /// Print one concept's frontmatter.
    Show(ShowArgs),
    /// Scaffold a new concept file (alias: new).
    #[command(alias = "new")]
    Add(AddArgs),
    /// Move/rename a concept, fixing bundle cross-links (alias: rename).
    #[command(alias = "rename")]
    Mv(MvArgs),
    /// Set or update a scalar frontmatter field on a concept.
    Set(SetArgs),
    /// Prepend a dated entry to a bundle's log.md.
    Log(LogArgs),
    /// (Re)generate a bundle directory's index.md from its concepts.
    GenIndex(GenIndexArgs),
    /// Run a .ctb batch of OKF mutations atomically.
    Script(ScriptArgs),
}

/// Framing options shared by the check verbs (`validate`, `links`).
#[derive(Args, Debug, Default)]
pub struct Framing {
    /// Print a `== QUESTION ==` banner before the check.
    #[arg(long)]
    pub question: Option<String>,
    /// Classify the violation count: any|none|N|=N|+N|-N (default none).
    #[arg(long)]
    pub expect: Option<String>,
    /// Expand a template to stdout after the check (tokens {RESULT} {COUNT} …).
    #[arg(long, alias = "emit-stdout")]
    pub emit: Option<String>,
    /// Expand a template to stderr after the check.
    #[arg(long)]
    pub emit_stderr: Option<String>,
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Query terms. Supports `term`, `term*` (prefix), `term~`/`term~2` (fuzzy), and `/regex/`.
    #[arg(value_name = "QUERY", required = true)]
    pub query: Vec<String>,
    /// Maximum number of hits to return.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Only hits of this exact OKF type.
    #[arg(long = "type", value_name = "TYPE")]
    pub type_: Option<String>,
    /// Only hits carrying all of these tags.
    #[arg(long, value_delimiter = ',')]
    pub tag: Vec<String>,
}

#[derive(Args, Debug)]
pub struct FindArgs {
    /// Filter to this exact OKF type.
    #[arg(long = "type", value_name = "TYPE")]
    pub type_: Option<String>,
    /// Filter to concepts carrying all of these tags.
    #[arg(long, value_delimiter = ',')]
    pub tag: Vec<String>,
}

#[derive(Args, Debug)]
pub struct RootsArgs {
    #[command(subcommand)]
    pub action: RootsCmd,
}

#[derive(Subcommand, Debug)]
pub enum RootsCmd {
    /// List the configured/detected content roots and how each was detected.
    List,
    /// Register a directory as a content root (records it in .ct/okf.jsonc).
    Add {
        /// The directory to add (project-relative or absolute).
        dir: PathBuf,
        /// Also drop a `.okf` marker file in the directory.
        #[arg(long)]
        marker: bool,
    },
    /// Unregister a content root from .ct/okf.jsonc.
    Rm {
        /// The directory to remove.
        dir: PathBuf,
    },
    /// Discover candidate roots by scanning for OKF concepts.
    Scan {
        /// Record discovered roots in config and drop `.okf` markers.
        #[arg(long)]
        write: bool,
    },
}

#[derive(Args, Debug)]
pub struct IndexArgs {
    #[command(subcommand)]
    pub action: IndexCmd,
}

#[derive(Subcommand, Debug)]
pub enum IndexCmd {
    /// Report index freshness (docs, segments, tombstones, pending changes).
    Status,
    /// Show the effective provider/include/exclude indexing plan.
    Scopes {
        /// Include derived defaults and hard exclusions in the report (the default plan is always effective).
        #[arg(long)]
        effective: bool,
    },
    /// Explain why one path is included in or excluded from the index.
    Why {
        /// File whose effective indexing decision should be explained.
        path: PathBuf,
    },
    /// Preview or materialize the conservative derived indexing configuration.
    Init {
        /// Print the derived configuration without writing it (the default).
        #[arg(long)]
        dry_run: bool,
        /// Write the derived configuration to .ct/index.jsonc.
        #[arg(long, conflicts_with = "dry_run")]
        write: bool,
    },
    /// Inspect or control the opportunistic filesystem watcher.
    Watch {
        #[command(subcommand)]
        action: WatchCmd,
    },
    /// Reconcile the index against the content roots now.
    Update,
    /// Merge segments and drop tombstones, reclaiming space.
    Condense,
    /// Discard and rebuild the index from scratch.
    Rebuild,
}

#[derive(Subcommand, Debug)]
pub enum WatchCmd {
    /// Report watcher health and its last update metrics.
    Status,
    /// Start the per-project watcher if one is not already healthy.
    Start,
    /// Ask the per-project watcher to exit.
    Stop,
    /// Internal foreground daemon body (normally started automatically).
    #[command(hide = true)]
    Run,
}

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Also drop a `.okf` marker file in each discovered root.
    #[arg(long)]
    pub marker: bool,
}

#[derive(Args, Debug)]
pub struct CheckArgs {
    /// Also count broken bundle-relative cross-links as violations.
    #[arg(long)]
    pub strict: bool,
    #[command(flatten)]
    pub framing: Framing,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    /// The concept file to show.
    pub path: PathBuf,
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// The concept file to create (must not exist).
    pub path: PathBuf,
    /// The concept's OKF type (required).
    #[arg(long = "type", value_name = "TYPE")]
    pub type_: String,
    /// Human title (defaults to the file stem).
    #[arg(long)]
    pub title: Option<String>,
    /// One-sentence description.
    #[arg(long)]
    pub description: Option<String>,
    /// Tags (comma-separated or repeated).
    #[arg(long, value_delimiter = ',')]
    pub tag: Vec<String>,
}

#[derive(Args, Debug)]
pub struct MvArgs {
    /// The concept file to move.
    pub src: PathBuf,
    /// The destination path.
    pub dst: PathBuf,
}

#[derive(Args, Debug)]
pub struct SetArgs {
    /// FIELD=VALUE to set on the concept's frontmatter.
    #[arg(value_name = "FIELD=VALUE")]
    pub spec: String,
    /// The concept file to edit.
    #[arg(long)]
    pub file: PathBuf,
}

#[derive(Args, Debug)]
pub struct LogArgs {
    /// The log message.
    #[arg(value_name = "MESSAGE")]
    pub message: String,
    /// Entry label (default Update).
    #[arg(long = "kind", value_name = "LABEL")]
    pub kind: Option<String>,
}

#[derive(Args, Debug)]
pub struct GenIndexArgs {
    /// Scaffold an absent index.md declaring okf_version instead of listing concepts.
    #[arg(long)]
    pub scaffold: bool,
}

#[derive(Args, Debug)]
pub struct ScriptArgs {
    /// The .ctb script of new/set/log/index/init items.
    pub path: PathBuf,
    /// Print the plan and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Directive prefix for script lines (default '#%').
    #[arg(long, value_name = "STR")]
    pub fence: Option<String>,
}
