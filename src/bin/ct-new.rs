// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-new` — create a file, without clobbering what is already there.
//!
//! The suite's *locate → read → change → verify* loop had no first move for
//! "this file does not exist yet": an agent fell back to `cat > f <<EOF`, which
//! silently truncates an existing file, silently succeeds when the path is
//! wrong, and reports nothing. `ct-new` makes the create the same shape as
//! every other change in the suite: a declared target, a payload, an explicit
//! collision policy (`--on-exists`), a framed `--expect` verdict, and
//! `--dry-run` to see the bytes first. `--script` creates a whole batch from
//! one `.ctb` document under the prepare/confirm/write standard — every target
//! is decided in memory, and one blocked target means zero writes. Reachable
//! directly or as `ct new`. The canonical reference is
//! `docs/explain/ct-new.md`; `docs/explain/ct-new.json` is the MCP tool-use
//! definition. Both are embedded below.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use coding_tools::atomicfile;
use coding_tools::blockdoc;
use coding_tools::cli::ct_new::Cli;
use coding_tools::explain::Format;
use coding_tools::newfile::{self, Action, Plan, Tally};
use coding_tools::payload;
use coding_tools::pulse::{self, PulseState, Watchdog};
use coding_tools::verdict::{Expect, Verdict};
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-new.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-new.json");

/// Lines of a body shown per file under `--dry-run`. A create can be arbitrarily
/// large; the preview stays bounded so it never floods the caller's context.
const PREVIEW_LINES: usize = 12;

/// Check one target's surroundings without touching them: the path must not
/// already be a directory, and its parent must exist unless `--parents` says to
/// create it. Both are mistakes rather than outcomes — a wrong path should fail
/// loudly, not scatter directories — so they abort with exit `2`.
fn check_target(path: &Path, parents: bool) -> Result<(), String> {
    if path.is_dir() {
        return Err(format!("{} is a directory, not a file", path.display()));
    }
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() || parent.is_dir() {
        return Ok(());
    }
    if parent.exists() {
        return Err(format!(
            "{}: {} is not a directory",
            path.display(),
            parent.display()
        ));
    }
    if !parents {
        return Err(format!(
            "{}: parent directory {} does not exist; pass --parents to create it",
            path.display(),
            parent.display()
        ));
    }
    Ok(())
}

/// Plan the single-file form: resolve the payload, decide the action.
fn plan_one(cli: &Cli) -> Result<Vec<Plan>, String> {
    let path = cli
        .path
        .clone()
        .ok_or_else(|| "give a PATH to create (or a batch with --script)".to_string())?;
    let raw = match (&cli.content, cli.empty) {
        (Some(c), _) => payload::resolve(c)?.text,
        (None, true) => String::new(),
        (None, false) => {
            return Err(
                "give --content (file:PATH / stdin: / text:VALUE accepted), or --empty for a \
                 zero-byte file"
                    .to_string(),
            );
        }
    };
    check_target(&path, cli.parents)?;
    Ok(vec![Plan {
        action: newfile::action_for(path.exists(), cli.on_exists),
        content: newfile::finalize(&raw, !cli.no_final_newline),
        path,
        line: 0,
    }])
}

/// Plan the `--script` form: parse the `.ctb` document and decide every target
/// before anything is written.
fn plan_script(cli: &Cli) -> Result<Vec<Plan>, String> {
    let script_path = cli.script.as_ref().unwrap();
    let src = std::fs::read_to_string(script_path)
        .map_err(|e| format!("reading script {}: {e}", script_path.display()))?;
    let items = blockdoc::parse(&src, &cli.fence, &["new"])
        .map_err(|e| format!("{}: {e}", script_path.display()))?;
    if items.is_empty() {
        return Err(format!(
            "{}: script contains no `new` items",
            script_path.display()
        ));
    }
    let specs = items
        .iter()
        .enumerate()
        .map(|(i, it)| newfile::compile_item(it, i + 1))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{}: {e}", script_path.display()))?;
    newfile::check_unique(&specs).map_err(|e| format!("{}: {e}", script_path.display()))?;

    let mut plans = Vec::with_capacity(specs.len());
    for spec in &specs {
        let path = PathBuf::from(&spec.file);
        check_target(&path, cli.parents)
            .map_err(|e| format!("{}: new {}: {e}", script_path.display(), spec.ordinal))?;
        plans.push(Plan {
            action: newfile::action_for(path.exists(), spec.on_exists.unwrap_or(cli.on_exists)),
            content: newfile::finalize(&spec.content, !cli.no_final_newline),
            path,
            line: spec.line,
        });
    }
    Ok(plans)
}

/// Confirm every write is possible before any of them happens: an existing
/// target that is read-only would fail mid-batch, and a batch never half-applies.
fn preflight(plans: &[Plan]) -> Result<(), String> {
    for p in plans.iter().filter(|p| p.action == Action::Overwritten) {
        let meta = std::fs::metadata(&p.path).map_err(|e| {
            format!(
                "write pre-flight: {}: {e}; nothing written",
                p.path.display()
            )
        })?;
        if meta.permissions().readonly() {
            return Err(format!(
                "write pre-flight: {} is not writable; nothing written",
                p.path.display()
            ));
        }
    }
    Ok(())
}

/// Write every planned file. The watchdog is disarmed first: a write phase,
/// once begun, always completes. Publication is atomic per file, so a reader
/// never observes a half-written create.
fn write_all(watchdog: &Option<Watchdog>, plans: &[Plan], parents: bool) -> Result<(), String> {
    if let Some(w) = watchdog {
        w.disarm();
    }
    for p in plans.iter().filter(|p| p.action.writes()) {
        if parents
            && let Some(parent) = p.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        atomicfile::write(&p.path, &p.content).map_err(|e| format!("writing {e}"))?;
    }
    Ok(())
}

/// The hint attached to a blocked target — the two ways to say what should
/// happen to a file that is already there.
const BLOCKED_HINT: &str = "already exists; --on-exists skip to accept it, overwrite to replace it";

fn run(mut cli: Cli) -> Result<ExitCode, String> {
    // --json-pretty enables JSON output on its own; treat it as --json
    // everywhere the text path is gated.
    if cli.json_pretty {
        cli.json = true;
    }
    let watchdog = pulse::watchdog("ct-new", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-new", PulseState::new())?;

    let expect = match &cli.expect {
        Some(s) => Expect::parse(s).map_err(|e| format!("invalid --expect: {e}"))?,
        None => Expect::default(),
    };

    // Phase 1: decide every target in memory, touching nothing.
    let plans = if cli.script.is_some() {
        plan_script(&cli)?
    } else {
        plan_one(&cli)?
    };
    let tally = Tally::of(&plans);

    // A refused target is never a success, whatever --expect asked for: the
    // whole point of the default policy is that a create cannot quietly not
    // happen.
    let verdict = if tally.blocked > 0 {
        Verdict::Error
    } else {
        expect.eval(tally.ensured() as u64)
    };

    // Phase 2: confirm, then write — only when the whole plan passed.
    let applied = verdict == Verdict::Success && !cli.dry_run;
    if applied {
        preflight(&plans)?;
        write_all(&watchdog, &plans, cli.parents)?;
    }

    if cli.json {
        let files: Vec<_> = plans
            .iter()
            .map(|p| {
                json!({
                    "path": p.path.display().to_string(),
                    "action": p.action.label(),
                    "lines": p.lines(),
                    "bytes": p.bytes(),
                })
            })
            .collect();
        let mut obj = json!({
            "tool": "ct-new",
            "verdict": verdict.label(),
            "dry_run": cli.dry_run,
            "applied": applied,
            "ensured": tally.ensured(),
            "created": tally.created,
            "overwritten": tally.overwritten,
            "skipped": tally.skipped,
            "blocked": tally.blocked,
            "files": files,
        });
        if let Some(script) = &cli.script {
            obj["script"] = json!(script.display().to_string());
        }
        coding_tools::jsonout::print(&obj, cli.json_pretty);
    } else {
        if !cli.quiet {
            for p in &plans {
                match p.action {
                    Action::Blocked => {
                        println!("{}: blocked — {BLOCKED_HINT}", p.path.display());
                    }
                    Action::Skipped => println!("{}: skipped (already there)", p.path.display()),
                    _ => {
                        println!(
                            "{}: {} ({} line(s), {} byte(s))",
                            p.path.display(),
                            p.action.label(),
                            p.lines(),
                            p.bytes()
                        );
                        // The bytes themselves, bounded, only when previewing.
                        if cli.dry_run {
                            let (shown, more) = newfile::preview(&p.content, PREVIEW_LINES);
                            for (i, line) in shown.iter().enumerate() {
                                println!("{}:{}:+ {line}", p.path.display(), i + 1);
                            }
                            if more > 0 {
                                println!("{}: + {more} more line(s)", p.path.display());
                            }
                        }
                    }
                }
            }
        }
        println!(
            "{} file(s) ensured ({} created, {} overwritten, {} skipped, {} blocked) -> {} ({})",
            tally.ensured(),
            tally.created,
            tally.overwritten,
            tally.skipped,
            tally.blocked,
            verdict.label(),
            if applied && tally.created + tally.overwritten > 0 {
                "applied"
            } else if applied {
                "nothing to write"
            } else if cli.dry_run {
                "dry-run, not written"
            } else {
                "verdict ERROR, not written"
            },
        );
    }

    Ok(verdict.exit_code())
}

fn main() -> ExitCode {
    let cli: Cli = coding_tools::cli::parse();

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
            eprintln!("ct-new: {msg}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use coding_tools::newfile::OnExists;

    /// `OnExists` is re-exported through the CLI grammar; keep the default the
    /// safe one, since it is what an agent gets when it says nothing.
    #[test]
    fn the_default_policy_refuses_to_clobber() {
        assert_eq!(OnExists::default(), OnExists::Fail);
    }
}
