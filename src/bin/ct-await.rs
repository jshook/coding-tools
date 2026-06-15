// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-await` — wait, observably, for an external outcome.
//!
//! Polls a gated read-only probe every `--every` seconds until the condition
//! is established — probe exit `0`, or a required `--ok-match` appearing in
//! its output — or until an `--err-match` appears (`ERROR`, immediately) or
//! the required `--timeout` expires (`ERROR`). The probe is
//! the suite's read-only set (plus `ct-test`; `ct-check` included, so "wait
//! until the invariants hold" is one command) — execution authority stays
//! with whoever runs the real work; `ct-await` only observes its effects.
//! Reachable directly or as `ct await`. The canonical reference is
//! `docs/explain/ct-await.md`; `docs/explain/ct-await.json` is the MCP
//! tool-use definition. Both are embedded below.

use std::process::{Command, ExitCode};
use std::time::Instant;

use clap::Parser;
use coding_tools::allowlist;
use coding_tools::cli::ct_await::Cli;
use coding_tools::explain::Format;
use coding_tools::pattern;
use coding_tools::pulse::{self, PulseState};
use coding_tools::supervise;
use coding_tools::template;
use coding_tools::verdict::Verdict;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-await.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-await.json");

/// The refusal shown for a non-gated probe.
fn deny_message(name: &str) -> String {
    let base = allowlist::BUILTIN.join(" ");
    format!(
        "ct-await: '{name}' is not an allowed probe, so nothing was run.\n\
         \n\
         ct-await polls this fixed set of read-only commands:\n  \
         {base} ct-test ct-each\n\
         \n\
         The list is immutable; ct-await observes, it never executes the work \
         itself, and there is no shell mode.\n"
    )
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    if cli.probe.is_empty() {
        return Err("missing probe: supply one after `--`, e.g. `ct-await --timeout 600 -- ct-search --base target/build.log --grep 'BUILD SUCCESS' --quiet`".to_string());
    }
    let name = allowlist::gated_name(&cli.probe[0]);
    let gated_ok = allowlist::is_allowed_for_each(&name, false)
        && !(name == "ct-each" && cli.probe.iter().any(|a| a == "--mutating"));
    if !gated_ok {
        eprint!("{}", deny_message(&name));
        return Ok(ExitCode::from(2));
    }
    let every = pulse::secs("--every", cli.every)?;
    let limit = pulse::secs("--timeout", cli.timeout)?;
    let ok_re = cli
        .ok_match
        .as_deref()
        .map(|p| {
            pattern::compile_with(p, cli.mode)
                .map_err(|e| format!("invalid --ok-match pattern: {e}"))
        })
        .transpose()?;
    let err_re = cli
        .err_match
        .as_deref()
        .map(|p| {
            pattern::compile_with(p, cli.mode)
                .map_err(|e| format!("invalid --err-match pattern: {e}"))
        })
        .transpose()?;
    let cmdline = cli.probe.join(" ");

    if !cli.quiet
        && let Some(q) = &cli.question
    {
        println!("== {q} ==");
    }

    let state = PulseState::new();
    state.set("QUESTION", cli.question.as_deref().unwrap_or(""));
    state.set("CMD", &cmdline);
    let _pulse = cli.heartbeat.start("ct-await", state.clone())?;

    let started = Instant::now();
    let mut ticks = 0u64;
    let (verdict, reason) = loop {
        let remaining = limit.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break (
                Verdict::Error,
                format!("timed out after {} ({ticks} probe run(s))", pulse::limit_label(limit)),
            );
        }
        ticks += 1;
        state.set("TICKS", &ticks.to_string());
        let mut command =
            Command::new(supervise::resolve_program(&cli.probe[0], &name));
        command.args(&cli.probe[1..]);
        // A single probe run may never outlive the overall bound.
        let outcome = supervise::run_captured(command, None, Some(remaining))
            .map_err(|e| format!("probe '{}': {e}", cli.probe[0]))?;
        // Matcher precedence is exactly ct-test's: a failure signal is
        // decisive; a supplied ok-match is the required success proof.
        if let Some(re) = &err_re
            && (re.is_match(&outcome.stdout) || re.is_match(&outcome.stderr))
        {
            break (
                Verdict::Error,
                format!(
                    "--err-match '{}' matched on probe run {ticks}",
                    cli.err_match.as_deref().unwrap_or("")
                ),
            );
        }
        if outcome.timed_out {
            break (
                Verdict::Error,
                format!("timed out after {} (probe run {ticks} killed)", pulse::limit_label(limit)),
            );
        }
        let established = match &ok_re {
            Some(re) => re.is_match(&outcome.stdout) || re.is_match(&outcome.stderr),
            None => outcome.status.is_some_and(|s| s.success()),
        };
        if established {
            break (
                Verdict::Success,
                format!(
                    "condition established after {}s ({ticks} run(s))",
                    started.elapsed().as_secs()
                ),
            );
        }
        // Not yet: a non-zero exit, or a missing required ok-match, just
        // means the condition is not established (a file that does not exist
        // yet is the normal case).
        let sleep = every.min(limit.saturating_sub(started.elapsed()));
        if sleep.is_zero() {
            continue; // loop once more to produce the timeout verdict
        }
        std::thread::sleep(sleep);
    };

    if verdict == Verdict::Error {
        eprintln!("ct-await: {reason}");
    }
    if !cli.quiet {
        println!("{} ({reason})", verdict.label());
    }
    if cli.emit.is_some() || cli.emit_stderr.is_some() {
        let elapsed = started.elapsed().as_secs().to_string();
        let ticks_s = ticks.to_string();
        let tokens = [
            ("RESULT", verdict.label()),
            ("ELAPSED", elapsed.as_str()),
            ("TICKS", ticks_s.as_str()),
            ("REASON", reason.as_str()),
            ("QUESTION", cli.question.as_deref().unwrap_or("")),
            ("CMD", cmdline.as_str()),
        ];
        if let Some(t) = &cli.emit {
            println!("{}", template::render(t, &tokens));
        }
        if let Some(t) = &cli.emit_stderr {
            eprintln!("{}", template::render(t, &tokens));
        }
    }
    Ok(verdict.exit_code())
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
            eprintln!("ct-await: {msg}");
            ExitCode::from(2)
        }
    }
}
