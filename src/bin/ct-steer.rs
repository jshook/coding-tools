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
    let mode = args.mode.to_lib();
    // Tool-call logging is on by default: record every call (steered or allowed)
    // for later analysis. Best-effort and fail-open — a logging error must never
    // disturb the tool call, so it is swallowed and the decision still emitted.
    if let Some((dir, managed)) = resolve_log_dir(args) {
        append_log(&dir, managed, &envelope, mode);
    }
    if let Some(decision) = steer::hook::process(&envelope, mode) {
        // Compact JSON on stdout is how Claude Code reads the decision.
        println!("{decision}");
    }
    ExitCode::SUCCESS
}

/// The daily-log directory and whether it is the managed default. `--no-log`
/// disables logging; `--log-dir`/`CT_STEER_LOG` name an explicit directory (not
/// managed — we leave its gitignore to the operator); otherwise it defaults to
/// `.ct/tclog` under the nearest `.ct` (the managed case, whose `.ct/.gitignore`
/// we keep current).
fn resolve_log_dir(args: &HookArgs) -> Option<(PathBuf, bool)> {
    if args.no_log {
        return None;
    }
    if let Some(dir) = &args.log_dir {
        return Some((dir.clone(), false));
    }
    if let Some(dir) = std::env::var_os("CT_STEER_LOG") {
        return Some((PathBuf::from(dir), false));
    }
    let cwd = std::env::current_dir().ok()?;
    let root = coding_tools::rules::discover_root(&cwd).unwrap_or(cwd);
    Some((root.join(".ct").join("tclog"), true))
}

/// Append one JSONL record for this envelope to the day's file in `dir`, stamped
/// with the current time. Best-effort: the directory is created as needed and any
/// error is discarded. When `managed`, the enclosing `.ct/.gitignore` is kept
/// current so the log directory stays out of version control.
fn append_log(dir: &std::path::Path, managed: bool, envelope: &str, mode: steer::Mode) {
    let now_ms = epoch_ms();
    let mut record = steer::hook::log_record(envelope, mode);
    if let serde_json::Value::Object(map) = &mut record {
        map.insert("ts_ms".to_string(), serde_json::json!(now_ms));
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if managed {
        ensure_ct_gitignore(dir);
    }
    let stem = steer::date_stem((now_ms / 1000) as i64);
    let file = dir.join(format!("{stem}.jsonl"));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
    {
        use std::io::Write;
        let mut line = record.to_string();
        line.push('\n');
        let _ = f.write_all(line.as_bytes());
    }
}

/// Ensure the `.ct/.gitignore` alongside a `.ct/tclog` log directory carries the
/// `*log` rule, so the logs are excluded from version control. Best-effort.
fn ensure_ct_gitignore(tclog_dir: &std::path::Path) {
    let Some(ct_dir) = tclog_dir.parent() else {
        return;
    };
    let path = ct_dir.join(".gitignore");
    let existing = std::fs::read_to_string(&path).ok();
    if let Some(contents) = steer::gitignore_with_log_rule(existing.as_deref()) {
        let _ = std::fs::write(&path, contents);
    }
}

/// Milliseconds since the Unix epoch (0 if the clock is before it).
fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
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
    let log_dir = args
        .log_dir
        .as_deref()
        .map(|p| p.to_string_lossy().into_owned());
    let command = install::hook_command(args.mode.to_lib(), log_dir.as_deref(), args.no_log);
    // --all-tools installs a single "*" matcher (full coverage); it supersedes
    // any explicit --tools list.
    let tools: Vec<install::Tool> = if args.all_tools {
        vec![install::Tool::All]
    } else {
        args.tools.iter().map(|t| t.to_lib()).collect()
    };

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
