// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-steer` — steer ad-hoc shell to the `ct` tool that serves it.
//!
//! `ct-steer` recognises the high-confidence shell idioms a suite tool serves
//! better — `find | xargs grep`, `grep -r`, `sed -i`, `cat | head`, `for`
//! loops, `&&`/`||` chains — and, run as a Claude Code `PreToolUse` hook,
//! steers the agent to the `ct` equivalent instead of letting the raw command
//! run. Reachable as `ct steer` or `ct-steer`. The verbs:
//!
//! * `hook` — the runtime hook: read a `PreToolUse` envelope on stdin, emit a
//!   deny/ask/warn decision (fail-open: anything it doesn't recognise is
//!   allowed silently).
//! * `install` / `uninstall` — merge or remove the hook in a project's
//!   `.claude/settings.json` (idempotent; `--dry-run`/`--print` to preview).
//! * `check` — show, and exit-code, what the hook would decide for a command.
//!
//! The classifier itself lives in `coding_tools::steer`. The canonical
//! reference is `docs/explain/ct-steer.md`; `docs/explain/ct-steer.json` is the
//! MCP tool-use definition. Both are embedded below.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use coding_tools::cli::ct_steer::{CheckArgs, Cli, Command, HookArgs, InstallArgs};
use coding_tools::explain::Format;
use coding_tools::pulse::{self, PulseState};
use coding_tools::steer::{self, install};

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-steer.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-steer.json");

/// The user's home directory, for `--scope user`.
fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "cannot find your home directory (HOME / USERPROFILE unset)".to_string())
}

/// `ct steer hook`: read the PreToolUse envelope on stdin, print a decision.
/// Always exits 0 — a hook must never fail the tool call on its own account.
fn cmd_hook(args: &HookArgs) -> ExitCode {
    let mut envelope = String::new();
    if std::io::stdin().read_to_string(&mut envelope).is_err() {
        return ExitCode::SUCCESS; // fail-open
    }
    if let Some(decision) = steer::hook::process(&envelope, args.mode.to_lib()) {
        // Compact JSON on stdout is how Claude Code reads the decision.
        println!("{decision}");
    }
    ExitCode::SUCCESS
}

/// Resolve the settings file path for an install/uninstall.
fn settings_path(args: &InstallArgs) -> Result<PathBuf, String> {
    let root = std::env::current_dir().map_err(|e| format!("cannot read current dir: {e}"))?;
    let home = match args.scope {
        coding_tools::cli::ct_steer::Scope::User => home_dir()?,
        _ => root.clone(),
    };
    Ok(args.scope.to_lib().path(&root, &home))
}

/// `ct steer install`.
fn cmd_install(cli: &Cli, args: &InstallArgs) -> Result<ExitCode, String> {
    let command = install::hook_command(args.mode.to_lib());
    let tools: Vec<install::Tool> = args.tools.iter().map(|t| t.to_lib()).collect();

    // `--print` just shows the snippet to paste; it reads/writes nothing.
    if args.print {
        let (snippet, _) = install::install(None, &command, &tools)?;
        print!("{snippet}");
        return Ok(ExitCode::SUCCESS);
    }

    let path = settings_path(args)?;
    let existing = read_settings(&path)?;
    let (text, changed) = install::install(existing.as_deref(), &command, &tools)?;

    if args.dry_run {
        if !cli.quiet {
            eprintln!("# would write {}", path.display());
        }
        print!("{text}");
        return Ok(ExitCode::SUCCESS);
    }

    write_settings(&path, &text)?;
    report(cli, &path, changed, "installed", "already present");
    Ok(ExitCode::SUCCESS)
}

/// `ct steer uninstall`.
fn cmd_uninstall(cli: &Cli, args: &InstallArgs) -> Result<ExitCode, String> {
    let path = settings_path(args)?;
    let Some(existing) = read_settings(&path)? else {
        report(cli, &path, false, "removed", "no settings file");
        return Ok(ExitCode::SUCCESS);
    };
    let (text, changed) = install::uninstall(Some(&existing))?;
    if args.dry_run {
        print!("{text}");
        return Ok(ExitCode::SUCCESS);
    }
    if changed {
        write_settings(&path, &text)?;
    }
    report(cli, &path, changed, "removed", "not present");
    Ok(ExitCode::SUCCESS)
}

/// `ct steer check`: classify a command, print the decision, exit 0 (allow) or
/// 1 (would steer).
fn cmd_check(cli: &Cli, args: &CheckArgs) -> ExitCode {
    let mode = args.mode.to_lib();
    match steer::analyze(&args.command) {
        None => {
            if cli.json {
                println!("{}", serde_json::json!({ "decision": "allow" }));
            } else if !cli.quiet {
                println!("ALLOW — no ct tool clearly fits this command");
            }
            ExitCode::SUCCESS
        }
        Some(s) => {
            if cli.json {
                println!("{}", steer::hook::decision(&s, mode));
            } else if !cli.quiet {
                println!("{} [{}] — {}", mode_label(mode), s.rule_id, s.tool);
                println!("  {}", s.suggestion);
                println!("({})", s.note);
            }
            ExitCode::from(1)
        }
    }
}

/// Human label for a steering mode.
fn mode_label(mode: steer::Mode) -> &'static str {
    match mode {
        steer::Mode::Deny => "DENY",
        steer::Mode::Ask => "ASK",
        steer::Mode::Warn => "WARN",
    }
}

/// Read a settings file, returning `None` if it does not exist.
fn read_settings(path: &std::path::Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Write a settings file, creating the `.claude` directory if needed.
fn write_settings(path: &std::path::Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Report the outcome of an install/uninstall (unless `--quiet`/`--json`).
fn report(cli: &Cli, path: &std::path::Path, changed: bool, did: &str, noop: &str) {
    if cli.json {
        println!(
            "{}",
            serde_json::json!({ "path": path.display().to_string(), "changed": changed })
        );
        return;
    }
    if cli.quiet {
        return;
    }
    if changed {
        println!("ct steer hook {did} in {}", path.display());
    } else {
        println!("ct steer hook {noop} ({})", path.display());
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let _watchdog = pulse::watchdog("ct-steer", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-steer", PulseState::new())?;

    let Some(command) = &cli.command else {
        return Err(
            "specify a subcommand (hook, install, uninstall, check — see `ct-steer --help`)"
                .to_string(),
        );
    };
    match command {
        Command::Hook(a) => Ok(cmd_hook(a)),
        Command::Install(a) => cmd_install(&cli, a),
        Command::Uninstall(a) => cmd_uninstall(&cli, a),
        Command::Check(a) => Ok(cmd_check(&cli, a)),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Some(fmt) = cli.explain {
        let body = match fmt {
            Format::Md => EXPLAIN_MD,
            Format::Json => EXPLAIN_JSON,
        };
        print!("{body}");
        return ExitCode::SUCCESS;
    }

    match run(cli) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("ct-steer: {msg}");
            ExitCode::from(2)
        }
    }
}
