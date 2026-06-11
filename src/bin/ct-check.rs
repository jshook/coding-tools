// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-check` — verify the project's recorded invariants.
//!
//! Loads the rule store (`.ct/rules.jsonc`, discovered upward git-style),
//! runs every selected rule's probe in store order, and reports each in one
//! of five lanes: `SUCCESS`, `ERROR`, `WARN`, `PENDING`, `BROKEN`. Purely
//! read-only — runs never write anything; the companion `ct-rules` is the
//! writing surface. Reachable directly or as `ct check`. The canonical
//! reference is `docs/explain/ct-check.md`; the full surface specification
//! is `docs/specs/rules.md`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use coding_tools::explain::Format;
use coding_tools::pulse::{self, HeartbeatOpts, PulseState};
use coding_tools::rules::{self, ProbeOutcome, Rule, Severity, Store};
use coding_tools::{pattern, template};
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-check.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-check.json");

#[derive(Parser, Debug)]
#[command(
    name = "ct-check",
    version,
    about = "Verify the project's recorded invariants from .ct/rules.jsonc (read-only).",
    long_about = "ct-check runs the rule store's probes in order and reports each rule as SUCCESS, \
                  ERROR, WARN, PENDING, or BROKEN (also reachable as `ct check`). It never writes \
                  anything; rules are recorded with ct-rules. See `ct-check --explain` for \
                  agent-oriented documentation."
)]
struct Cli {
    /// Rule store. Default: the nearest .ct/rules.jsonc walking upward from the current directory.
    #[arg(long)]
    file: Option<PathBuf>,

    /// Select rules whose id matches (substring->glob->regex promoted, anchored).
    #[arg(long)]
    id: Option<String>,

    /// Select rules carrying any of these tags (comma-separated).
    #[arg(long, value_delimiter = ',')]
    tag: Vec<String>,

    /// Stop after the first enforced violation; remaining rules are reported as skipped.
    #[arg(long)]
    fail_fast: bool,

    /// Print the selected rules (id, lanes, question, tags); run nothing.
    #[arg(long)]
    list: bool,

    /// Suppress per-rule lines and the default summary.
    #[arg(long)]
    quiet: bool,

    /// Emit a structured JSON result instead of text (overrides the emit templates).
    #[arg(long)]
    json: bool,

    /// Per-rule template written to stdout. Tokens: {RESULT} {ID} {QUESTION} {CODE} {WHY} {CMD}.
    #[arg(long, value_name = "TEMPLATE")]
    emit_each: Option<String>,

    /// Summary template written to stdout. Tokens: {RESULT} {OK} {ERRORS} {WARNED} {PENDING} {BROKEN} {SKIPPED} {TOTAL} {REASON}.
    #[arg(long, alias = "emit-stdout", value_name = "TEMPLATE")]
    emit: Option<String>,

    /// Summary template written to stderr (same tokens as --emit).
    #[arg(long, value_name = "TEMPLATE")]
    emit_stderr: Option<String>,

    /// Default per-rule bound in seconds (fractional allowed); a rule's own timeout field overrides it. A timed-out probe is BROKEN.
    #[arg(long, value_name = "SECS")]
    timeout: Option<f64>,

    #[command(flatten)]
    heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    explain: Option<Format>,
}

/// A rule's reported lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Holds,
    Violated,
    Warned,
    Pending,
    Broken,
    Skipped,
}

impl Lane {
    fn label(self) -> &'static str {
        match self {
            Lane::Holds => "SUCCESS",
            Lane::Violated => "ERROR",
            Lane::Warned => "WARN",
            Lane::Pending => "PENDING",
            Lane::Broken => "BROKEN",
            Lane::Skipped => "SKIPPED",
        }
    }
}

/// One executed (or skipped) rule, for reporting.
struct Report {
    id: String,
    question: String,
    lane: Lane,
    code: String,
    reason: String,
    why: Option<String>,
    cmd: String,
    detail: String, // probe stdout head on violation
}

/// Resolve the store path: explicit `--file`, else nearest `.ct` upward.
fn resolve_store(file: &Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(f) = file {
        return Ok(f.clone());
    }
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    match rules::discover_root(&cwd) {
        Some(root) => Ok(rules::store_path(&root)),
        None => Err(format!(
            "no .ct directory found from {} upward; create the store with `ct rules --init`",
            cwd.display()
        )),
    }
}

/// Load, parse, and statically validate the store: every rule's probe must
/// def-expand and pass the gate before anything runs.
fn load_validated(path: &PathBuf) -> Result<(Store, Vec<Vec<String>>), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e} (create it with `ct rules --init`)", path.display()))?;
    let store = rules::parse_store(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut expanded = Vec::with_capacity(store.rules.len());
    for rule in &store.rules {
        let argv = rules::expand_defs(&rule.probe, &store.defs)
            .map_err(|e| format!("rule '{}': {e}", rule.id))?;
        rules::gate_probe(&argv).map_err(|e| format!("rule '{}': {e}", rule.id))?;
        expanded.push(argv);
    }
    Ok((store, expanded))
}

/// Whether `rule` is selected by `--id` / `--tag`.
fn selected(rule: &Rule, id_re: &Option<regex::Regex>, tags: &[String]) -> bool {
    if let Some(re) = id_re
        && !re.is_match(&rule.id)
    {
        return false;
    }
    if !tags.is_empty() && !tags.iter().any(|t| rule.tags.contains(t)) {
        return false;
    }
    true
}

/// The first `n` lines of `text`, with an elision marker.
fn head_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= n {
        return text.trim_end().to_string();
    }
    let mut out = lines[..n].join("\n");
    out.push_str(&format!("\n(... {} more line(s))", lines.len() - n));
    out
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let store_file = resolve_store(&cli.file)?;
    let (store, expanded) = load_validated(&store_file)?;

    let id_re = match &cli.id {
        Some(p) => Some(pattern::compile_anchored(p).map_err(|e| format!("invalid --id: {e}"))?),
        None => None,
    };
    let picked: Vec<usize> = (0..store.rules.len())
        .filter(|&i| selected(&store.rules[i], &id_re, &cli.tag))
        .collect();

    if cli.list {
        for &i in &picked {
            let r = &store.rules[i];
            let mut flags = Vec::new();
            if r.pending {
                flags.push("pending");
            }
            if r.severity == Severity::Warn {
                flags.push("warn");
            }
            let flags = if flags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", flags.join(","))
            };
            let tags = if r.tags.is_empty() {
                String::new()
            } else {
                format!("  ({})", r.tags.join(","))
            };
            println!("{}{flags}  {}{tags}", r.id, r.question);
        }
        return Ok(ExitCode::SUCCESS);
    }

    let total = picked.len();
    let state = PulseState::new();
    state.set("TOTAL", &total.to_string());
    let pulse_guard = cli.heartbeat.start("ct-check", state.clone())?;

    let mut reports: Vec<Report> = Vec::new();
    let mut stop = false;
    for (done, &i) in picked.iter().enumerate() {
        let rule = &store.rules[i];
        let argv = &expanded[i];
        if stop {
            reports.push(Report {
                id: rule.id.clone(),
                question: rule.question.clone(),
                lane: Lane::Skipped,
                code: String::new(),
                reason: "skipped by --fail-fast".to_string(),
                why: rule.why.clone(),
                cmd: argv.join(" "),
                detail: String::new(),
            });
            continue;
        }
        state.set("ID", &rule.id);
        state.set("DONE", &done.to_string());

        let gated = rules::gate_probe(argv).expect("validated at load");
        let timeout = rule
            .timeout
            .or(cli.timeout)
            .map(|v| pulse::secs("timeout", v))
            .transpose()?;
        let (outcome, reason, captured) = rules::run_probe(
            argv,
            &gated,
            &rules::probe_root(&store_file),
            rule.network,
            timeout,
            &rule.expect,
        );

        let lane = if rule.pending {
            Lane::Pending
        } else {
            match outcome {
                ProbeOutcome::Holds => Lane::Holds,
                ProbeOutcome::Violated => {
                    if rule.severity == Severity::Warn {
                        Lane::Warned
                    } else {
                        Lane::Violated
                    }
                }
                ProbeOutcome::Broken => Lane::Broken,
            }
        };
        if cli.fail_fast && lane == Lane::Violated {
            stop = true;
        }
        let pending_note = if rule.pending {
            match outcome {
                ProbeOutcome::Holds => " (now holds — promote?)",
                ProbeOutcome::Violated => " (not yet held)",
                ProbeOutcome::Broken => " (probe broken)",
            }
        } else {
            ""
        };
        reports.push(Report {
            id: rule.id.clone(),
            question: format!("{}{pending_note}", rule.question),
            lane,
            code: captured
                .status
                .and_then(|s| s.code())
                .map(|c| c.to_string())
                .unwrap_or_else(|| {
                    if captured.timed_out { "timeout" } else { "none" }.to_string()
                }),
            reason,
            why: rule.why.clone(),
            cmd: argv.join(" "),
            detail: if matches!(lane, Lane::Violated | Lane::Warned) {
                head_lines(&captured.stdout, 20)
            } else {
                String::new()
            },
        });
    }
    drop(pulse_guard);

    let count = |lane: Lane| reports.iter().filter(|r| r.lane == lane).count();
    let (holds, violated, warned) = (count(Lane::Holds), count(Lane::Violated), count(Lane::Warned));
    let (pending, broken, skipped) = (count(Lane::Pending), count(Lane::Broken), count(Lane::Skipped));
    let enforced = holds + violated + skipped; // fail-severity, non-pending rules
    let exit = if broken > 0 {
        ExitCode::from(2)
    } else if violated > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    };
    let result = if broken > 0 || violated > 0 { "ERROR" } else { "SUCCESS" };

    if cli.json {
        let rule_objs: Vec<_> = reports
            .iter()
            .map(|r| {
                json!({
                    "id": r.id, "question": r.question, "lane": r.lane.label(),
                    "code": r.code, "reason": r.reason, "why": r.why,
                })
            })
            .collect();
        println!(
            "{}",
            json!({
                "tool": "ct-check",
                "verdict": result,
                "store": store_file.display().to_string(),
                "ok": holds, "violated": violated, "warned": warned,
                "pending": pending, "broken": broken, "skipped": skipped,
                "total": total,
                "rules": rule_objs,
            })
        );
        return Ok(exit);
    }

    for r in &reports {
        if !cli.quiet {
            if let Some(t) = &cli.emit_each {
                let tokens = [
                    ("RESULT", r.lane.label()),
                    ("ID", r.id.as_str()),
                    ("QUESTION", r.question.as_str()),
                    ("CODE", r.code.as_str()),
                    ("WHY", r.why.as_deref().unwrap_or("")),
                    ("CMD", r.cmd.as_str()),
                ];
                println!("{}", template::render(t, &tokens));
            } else {
                println!("{:<8} {:<24} {}", r.lane.label(), r.id, r.question);
            }
        }
        // A red lane is never unexplained: reason, rationale, and the probe's
        // own violation output go to stderr.
        match r.lane {
            Lane::Violated | Lane::Warned | Lane::Broken => {
                let why = r
                    .why
                    .as_deref()
                    .map(|w| format!(" — why: {w}"))
                    .unwrap_or_default();
                eprintln!("ct-check: '{}' {} ({}){why}", r.id, r.lane.label(), r.reason);
                if !r.detail.is_empty() {
                    for line in r.detail.lines() {
                        eprintln!("  {line}");
                    }
                }
            }
            _ => {}
        }
    }

    let mut extras = Vec::new();
    if warned > 0 {
        extras.push(format!("{warned} warned"));
    }
    if pending > 0 {
        extras.push(format!("{pending} pending"));
    }
    if broken > 0 {
        extras.push(format!("{broken} broken"));
    }
    if skipped > 0 {
        extras.push(format!("{skipped} skipped"));
    }
    let extras = if extras.is_empty() {
        String::new()
    } else {
        format!(", {}", extras.join(", "))
    };
    if broken > 0 {
        eprintln!("ct-check: {broken} broken rule(s) — fix or remove with ct-rules");
    }
    if !cli.quiet && cli.emit.is_none() {
        println!("{holds}/{enforced} invariant(s) hold{extras} -> {result}");
    }
    if cli.emit.is_some() || cli.emit_stderr.is_some() {
        let strings = [
            holds.to_string(),
            violated.to_string(),
            warned.to_string(),
            pending.to_string(),
            broken.to_string(),
            skipped.to_string(),
            total.to_string(),
        ];
        let reason = format!("{holds}/{enforced} hold{extras}");
        let tokens = [
            ("RESULT", result),
            ("OK", strings[0].as_str()),
            ("ERRORS", strings[1].as_str()),
            ("WARNED", strings[2].as_str()),
            ("PENDING", strings[3].as_str()),
            ("BROKEN", strings[4].as_str()),
            ("SKIPPED", strings[5].as_str()),
            ("TOTAL", strings[6].as_str()),
            ("REASON", reason.as_str()),
        ];
        if let Some(t) = &cli.emit {
            println!("{}", template::render(t, &tokens));
        }
        if let Some(t) = &cli.emit_stderr {
            eprintln!("{}", template::render(t, &tokens));
        }
    }

    Ok(exit)
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
            eprintln!("ct-check: {msg}");
            ExitCode::from(2)
        }
    }
}
