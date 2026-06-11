// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-deps` — crate-graph invariants.
//!
//! Asserts properties of the resolved dependency graph — `--deny NAME`
//! (crate must not appear anywhere), `--forbid 'A=>B'` (package A must not
//! reach package B), `--duplicates` (no crate at more than one version) —
//! with every violation carrying its evidence path. The graph comes from
//! `cargo metadata --format-version 1 --locked --offline` (hermetic by
//! construction: no network, no lockfile writes), run from the current
//! directory. Read-only, so it is on the `ct-test` allowlist and usable in
//! rule probes; reachable directly or as `ct deps`. The canonical reference
//! is `docs/explain/ct-deps.md`; `docs/explain/ct-deps.json` is the MCP
//! tool-use definition. Both are embedded below.

use std::collections::HashSet;
use std::process::{Command, ExitCode};

use clap::Parser;
use coding_tools::deps::{self, EdgeKind, Violation};
use coding_tools::explain::Format;
use coding_tools::pulse::{self, HeartbeatOpts, PulseState};
use coding_tools::supervise;
use coding_tools::template;
use coding_tools::verdict::Verdict;
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-deps.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-deps.json");

#[derive(Parser, Debug)]
#[command(
    name = "ct-deps",
    version,
    about = "Assert crate-graph invariants: --deny crates, --forbid A=>B paths, no --duplicates.",
    long_about = "ct-deps interrogates the resolved dependency graph from `cargo metadata` \
                  (--locked --offline enforced: hermetic, read-only) and reports each violated \
                  assertion with an evidence path (also reachable as `ct deps`). Exit 0 when every \
                  assertion holds, 1 with violations, 2 on errors. See `ct-deps --explain` for \
                  agent-oriented documentation."
)]
struct Cli {
    /// Violation if this crate appears anywhere reachable from the workspace (repeatable).
    #[arg(long, value_name = "NAME")]
    deny: Vec<String>,

    /// Violation if package A reaches package B: 'A=>B' (repeatable). A must exist in the graph.
    #[arg(long, value_name = "A=>B")]
    forbid: Vec<String>,

    /// Violation for every crate that resolves at more than one version.
    #[arg(long)]
    duplicates: bool,

    /// Edge kinds traversed: normal, build, dev (comma-separated). Default: all three.
    #[arg(long, value_enum, value_delimiter = ',')]
    edges: Vec<EdgeKind>,

    /// Question this check answers; printed as a "== ... ==" banner.
    #[arg(long)]
    question: Option<String>,

    /// Template written to stdout after the check. Tokens: {RESULT} {COUNT} {VIOLATIONS} {QUESTION}.
    #[arg(long, alias = "emit-stdout")]
    emit: Option<String>,

    /// Template written to stderr (same tokens as --emit).
    #[arg(long)]
    emit_stderr: Option<String>,

    /// Print nothing; report via exit status (and --emit, which still fires).
    #[arg(long)]
    quiet: bool,

    /// Emit a structured JSON result instead of text (overrides --emit).
    #[arg(long)]
    json: bool,

    /// Kill the underlying cargo invocation and abort (exit 2) after SECS seconds (fractional allowed).
    #[arg(long, value_name = "SECS")]
    timeout: Option<f64>,

    #[command(flatten)]
    heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    explain: Option<Format>,
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    if cli.deny.is_empty() && cli.forbid.is_empty() && !cli.duplicates {
        return Err(
            "nothing to assert: supply --deny NAME, --forbid 'A=>B', and/or --duplicates"
                .to_string(),
        );
    }
    let allowed: HashSet<EdgeKind> = if cli.edges.is_empty() {
        [EdgeKind::Normal, EdgeKind::Build, EdgeKind::Dev]
            .into_iter()
            .collect()
    } else {
        cli.edges.iter().copied().collect()
    };
    let forbids: Vec<(String, String)> = cli
        .forbid
        .iter()
        .map(|spec| {
            spec.split_once("=>")
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                .filter(|(a, b)| !a.is_empty() && !b.is_empty())
                .ok_or_else(|| format!("--forbid needs 'A=>B', got '{spec}'"))
        })
        .collect::<Result<_, _>>()?;

    let _pulse = cli.heartbeat.start("ct-deps", PulseState::new())?;
    let timeout = cli.timeout.map(|v| pulse::secs("--timeout", v)).transpose()?;

    // The graph source: hermetic by construction.
    let mut command = Command::new("cargo");
    command.args(["metadata", "--format-version", "1", "--locked", "--offline"]);
    let outcome = supervise::run_captured(command, None, timeout)
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if outcome.timed_out {
        return Err(format!(
            "cargo metadata timed out after {}",
            pulse::limit_label(timeout.expect("timed out implies a bound"))
        ));
    }
    if !outcome.status.is_some_and(|s| s.success()) {
        return Err(format!(
            "cargo metadata failed: {}",
            outcome.stderr.lines().last().unwrap_or("(no output)")
        ));
    }
    let graph = deps::parse_metadata(&outcome.stdout)?;

    if !cli.json
        && !cli.quiet
        && let Some(q) = &cli.question
    {
        println!("== {q} ==");
    }

    let mut violations: Vec<Violation> = Vec::new();
    for name in &cli.deny {
        violations.extend(deps::deny_paths(&graph, name, &allowed));
    }
    for (from, to) in &forbids {
        violations.extend(deps::forbid_path(&graph, from, to, &allowed)?);
    }
    if cli.duplicates {
        for (name, versions) in graph.duplicates() {
            violations.push(Violation {
                check: "duplicates".to_string(),
                subject: name,
                evidence: versions.join(", "),
            });
        }
    }

    let verdict = if violations.is_empty() {
        Verdict::Success
    } else {
        Verdict::Error
    };

    if cli.json {
        let objs: Vec<_> = violations
            .iter()
            .map(|v| json!({ "check": v.check, "subject": v.subject, "evidence": v.evidence }))
            .collect();
        println!(
            "{}",
            json!({
                "tool": "ct-deps",
                "verdict": verdict.label(),
                "count": violations.len(),
                "violations": objs,
            })
        );
        return Ok(verdict.exit_code());
    }

    if !cli.quiet {
        for v in &violations {
            println!("{}: {}: {}", v.check, v.subject, v.evidence);
        }
        println!("{} violation(s) -> {}", violations.len(), verdict.label());
    }
    if cli.emit.is_some() || cli.emit_stderr.is_some() {
        let count = violations.len().to_string();
        let lines = violations
            .iter()
            .map(|v| format!("{}: {}: {}", v.check, v.subject, v.evidence))
            .collect::<Vec<_>>()
            .join("\n");
        let tokens = [
            ("RESULT", verdict.label()),
            ("COUNT", count.as_str()),
            ("VIOLATIONS", lines.as_str()),
            ("QUESTION", cli.question.as_deref().unwrap_or("")),
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
            eprintln!("ct-deps: {msg}");
            ExitCode::from(2)
        }
    }
}
