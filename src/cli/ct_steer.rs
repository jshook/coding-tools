// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-steer` command grammar (see [`crate::cli`]). Like `ct-okf`, this
//! tool is **subcommand**-shaped (`ct steer hook`, `ct steer install`, …)
//! because its surface spans the runtime hook, settings installation, and a
//! dry-run check. The `ct-steer` bin is a parse-and-dispatch wrapper over this
//! `Cli`.
//!
//! Global flags (`--json`, `--quiet`, `--timeout`, the heartbeat, `--explain`)
//! are declared `global` so they may appear before or after the subcommand;
//! per-verb flags live on each subcommand's args struct.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::explain::Format;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-steer",
    version,
    about = "Steer ad-hoc shell commands to the ct tool that serves them; install the PreToolUse hook.",
    long_about = "ct-steer recognises the shell idioms a ct tool serves better (find | xargs grep, \
                  sed -i, cat | head, for-loops, && / || chains) and, as a Claude Code PreToolUse \
                  hook, steers the agent to the ct equivalent instead. Also reachable as `ct steer`. \
                  Subcommands: `hook` is the runtime hook (reads a PreToolUse envelope on stdin); \
                  `install`/`uninstall` wire it into .claude/settings.json; `check` shows what the \
                  hook would decide for a command. See `ct-steer --explain` for agent docs."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Emit a structured JSON result instead of text (where applicable).
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress informational output (exit status still reports).
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

/// The `ct-steer` verbs.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Runtime PreToolUse hook: read a tool-call envelope on stdin, emit a decision.
    Hook(HookArgs),
    /// Runtime PostToolUse recorder: log the executed call (for effectiveness analysis).
    Post(PostArgs),
    /// Merge the steering hook into a Claude Code settings file.
    Install(InstallArgs),
    /// Remove the steering hook from a Claude Code settings file.
    Uninstall(InstallArgs),
    /// Show (and exit-code) what the hook would decide for a command string.
    Check(CheckArgs),
}

/// How the hook steers a matched command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// Block the call and feed the ct suggestion back to the agent (default).
    Deny,
    /// Surface a confirmation prompt naming the ct suggestion.
    Ask,
    /// Allow the call, but inject the ct suggestion as context.
    Warn,
}

impl Mode {
    /// Bridge to the library's mode.
    pub fn to_lib(self) -> crate::steer::Mode {
        match self {
            Mode::Deny => crate::steer::Mode::Deny,
            Mode::Ask => crate::steer::Mode::Ask,
            Mode::Warn => crate::steer::Mode::Warn,
        }
    }
}

/// A harness tool the steering hook can gate (one `PreToolUse` matcher each).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Tool {
    /// Shell commands (the default) — the full shell-idiom matcher.
    #[value(name = "Bash")]
    Bash,
    /// The harness content search → ct search.
    #[value(name = "Grep")]
    Grep,
    /// The harness file glob → ct search.
    #[value(name = "Glob")]
    Glob,
    /// The harness file read → ct view (images/PDF/notebooks pass through).
    #[value(name = "Read")]
    Read,
    /// Every tool (a "*" matcher) — full-coverage logging; only recognised idioms are steered.
    #[value(name = "all")]
    All,
}

impl Tool {
    /// Bridge to the library's tool.
    pub fn to_lib(self) -> crate::steer::install::Tool {
        match self {
            Tool::Bash => crate::steer::install::Tool::Bash,
            Tool::Grep => crate::steer::install::Tool::Grep,
            Tool::Glob => crate::steer::install::Tool::Glob,
            Tool::Read => crate::steer::install::Tool::Read,
            Tool::All => crate::steer::install::Tool::All,
        }
    }
}

/// Which settings file `install`/`uninstall` target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Scope {
    /// .claude/settings.json (shared, committed).
    Project,
    /// .claude/settings.local.json (personal, gitignored).
    Local,
    /// ~/.claude/settings.json (all projects).
    User,
}

impl Scope {
    /// Bridge to the library's scope.
    pub fn to_lib(self) -> crate::steer::install::Scope {
        match self {
            Scope::Project => crate::steer::install::Scope::Project,
            Scope::Local => crate::steer::install::Scope::Local,
            Scope::User => crate::steer::install::Scope::User,
        }
    }
}

#[derive(Args, Debug)]
pub struct HookArgs {
    /// Steering action on a match: deny (default), ask, or warn.
    #[arg(long, value_enum, default_value_t = Mode::Deny)]
    pub mode: Mode,

    /// Directory for the daily tool-call log; defaults to .ct/tclog (nearest .ct). Also settable via CT_STEER_LOG.
    #[arg(long, value_name = "DIR")]
    pub log_dir: Option<PathBuf>,

    /// Disable tool-call logging (it is on by default).
    #[arg(long)]
    pub no_log: bool,

    /// Also nudge (warn-only, never deny) against ANY shell pipeline the specific rules did not steer, prompting harder use of ct.
    #[arg(long)]
    pub nudge_pipelines: bool,
}

#[derive(Args, Debug)]
pub struct PostArgs {
    /// Directory for the daily log; defaults to .ct/tclog (nearest .ct). Also settable via CT_STEER_LOG.
    #[arg(long, value_name = "DIR")]
    pub log_dir: Option<PathBuf>,

    /// Disable logging (the recorder does nothing).
    #[arg(long)]
    pub no_log: bool,
}

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Which settings file to write: project (default), local, or user.
    #[arg(long, value_enum, default_value_t = Scope::Project)]
    pub scope: Scope,

    /// The steering action baked into the installed hook command.
    #[arg(long, value_enum, default_value_t = Mode::Deny)]
    pub mode: Mode,

    /// Harness tools to gate, comma-joined or repeated: Bash (default), Grep, Glob, Read. Grep/Glob steer to ct search, Read to ct view. Ignored when --all-tools is set.
    #[arg(long, value_enum, value_delimiter = ',', default_value = "Bash")]
    pub tools: Vec<Tool>,

    /// Gate every tool call under a single "*" matcher (superseding --tools) — for full-coverage logging.
    #[arg(long)]
    pub all_tools: bool,

    /// Bake a `--log-dir DIR` override into the installed hook command (logging is on by default to .ct/tclog).
    #[arg(long, value_name = "DIR")]
    pub log_dir: Option<PathBuf>,

    /// Bake `--no-log` into the installed hook command, disabling tool-call logging.
    #[arg(long)]
    pub no_log: bool,

    /// Bake `--nudge-pipelines` into the installed hook (warn-only nudge against any un-steered shell pipeline).
    #[arg(long)]
    pub nudge_pipelines: bool,

    /// Also install a PostToolUse recorder (a `*` matcher running `ct steer post`) to measure whether steer guidance was followed.
    #[arg(long)]
    pub measure: bool,

    /// Bake the absolute path of THIS ct-steer binary into the hook (instead of resolving `ct` on PATH), so a version-skewed or missing `ct` can't break the hook.
    #[arg(long)]
    pub pin: bool,

    /// Skip the preflight that verifies the resolving `ct` can parse the hook command; install even if it looks incompatible.
    #[arg(long)]
    pub force: bool,

    /// Show the resulting settings file without writing it.
    #[arg(long)]
    pub dry_run: bool,

    /// Print just the hook snippet (for manual paste) and exit.
    #[arg(long)]
    pub print: bool,
}

#[derive(Args, Debug)]
pub struct CheckArgs {
    /// The shell command to classify.
    #[arg(value_name = "COMMAND", required = true)]
    pub command: String,

    /// The steering action to report (affects the printed decision only).
    #[arg(long, value_enum, default_value_t = Mode::Deny)]
    pub mode: Mode,
}
