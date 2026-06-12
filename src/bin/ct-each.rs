// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-each` — declarative per-item dispatch.
//!
//! Runs one command template once per item — `{ITEM}`/`{INDEX}` expand inside
//! the argv elements, which are passed directly to the program (never through a
//! shell) — classifies each run by the suite's exit contract, and frames the
//! whole sweep with an aggregate `--expect` verdict. Reachable directly or as
//! `ct each`. Dispatch targets are gated: the read-only allowlist plus
//! `ct-test` by default, the suite's mutating tools only behind `--mutating`.
//! The canonical reference is `docs/explain/ct-each.md` — the text this tool
//! emits for `--explain md`; `docs/explain/ct-each.json` is the MCP tool-use
//! definition emitted for `--explain json`. Both are embedded below.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use clap::Parser;
use coding_tools::allowlist;
use coding_tools::explain::Format;
use coding_tools::pulse::{self, HeartbeatOpts, PulseState};
use coding_tools::supervise::{self, Outcome};
use coding_tools::walk::{self, EntryType};
use coding_tools::verdict::{Expect, Verdict};
use coding_tools::{pattern, payload, template};
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-each.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-each.json");

#[derive(Parser, Debug)]
#[command(
    name = "ct-each",
    version,
    about = "Run a command template once per item (no shell), with per-item verdicts and an aggregate --expect.",
    long_about = "ct-each dispatches one command over a set of distinct items: {ITEM} and {INDEX} \
                  expand inside the argv elements after `--`, each expansion is launched directly \
                  (never through a shell), each run is classified by exit status, and the SUCCESS \
                  count is judged against --expect (also reachable as `ct each`). See \
                  `ct-each --explain` for agent-oriented documentation."
)]
struct Cli {
    /// Items to dispatch over, in order (repeatable; one run per item). file:PATH expands to the file's non-empty lines; text:VALUE is one literal item.
    #[arg(long, num_args = 1.., value_name = "ITEM")]
    items: Vec<String>,

    /// Pin how --name/--ext walker patterns are interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    mode: Option<pattern::Mode>,

    /// Also read items from standard input, one per line (blank lines skipped), after any walker items.
    #[arg(long)]
    stdin: bool,

    /// Walker item source: files under this root become items (paths). A file yields itself; a directory is descended.
    #[arg(long)]
    base: Option<PathBuf>,

    /// Walker item source: limit to files whose name matches; '|'-separated alternatives, each substring->glob->regex promoted and anchored. Implies --base . when --base is absent.
    #[arg(long)]
    name: Option<String>,

    /// Walker item source: restrict to these extensions (comma-separated, no dots). Combined with --name as alternatives. Implies --base . when --base is absent.
    #[arg(long, value_delimiter = ',')]
    ext: Vec<String>,

    /// Include dot-entries while walking; default skips them.
    #[arg(long)]
    hidden: bool,

    /// Follow symlinks while walking.
    #[arg(long)]
    follow: bool,

    /// Question this sweep answers; printed as a "== ... ==" banner.
    #[arg(long)]
    question: Option<String>,

    /// Expectation over the per-item SUCCESS count: all|any|none|N|=N|+N|-N (default: all).
    #[arg(long)]
    expect: Option<String>,

    /// Stop after the first per-item ERROR; remaining items are reported as skipped.
    #[arg(long)]
    fail_fast: bool,

    /// Permit the suite's mutating tools (ct-edit, ct-patch) as the command.
    #[arg(long)]
    mutating: bool,

    /// Print each expanded command without running anything.
    #[arg(long)]
    dry_run: bool,

    /// Per item: kill the run and classify that item ERROR after SECS seconds (fractional allowed); its {CODE} becomes "timeout".
    #[arg(long, value_name = "SECS")]
    timeout: Option<f64>,

    #[command(flatten)]
    heartbeat: HeartbeatOpts,

    /// Per-item template written to stdout. Tokens: {RESULT} {ITEM} {INDEX} {CODE} {CMD} {STDOUT} {STDERR}. Default (unless --quiet): "{RESULT} {ITEM}".
    #[arg(long, value_name = "TEMPLATE")]
    emit_each: Option<String>,

    /// Summary template written to stdout after the sweep. Tokens: {RESULT} {OK} {ERRORS} {SKIPPED} {TOTAL} {QUESTION} {EXPECT} {REASON}.
    #[arg(long, alias = "emit-stdout", value_name = "TEMPLATE")]
    emit: Option<String>,

    /// Summary template written to stderr (same tokens as --emit).
    #[arg(long, value_name = "TEMPLATE")]
    emit_stderr: Option<String>,

    /// Also pass each child's stdout/stderr through verbatim.
    #[arg(long)]
    show_output: bool,

    /// Suppress the question banner, the default per-item lines, and the default summary.
    #[arg(long)]
    quiet: bool,

    /// Emit a structured JSON result instead of text (overrides the emit templates).
    #[arg(long)]
    json: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    explain: Option<Format>,

    /// Command and arguments run per item (after `--`); {ITEM} and {INDEX} expand in every element.
    #[arg(last = true, value_name = "CMD [ARGS...]")]
    command: Vec<String>,
}

/// The aggregate expectation: the suite's [`Expect`] grammar plus `all`,
/// which requires every item to succeed and is the default.
enum EachExpect {
    All,
    Std(Expect),
}

impl EachExpect {
    fn parse(spec: Option<&str>) -> Result<EachExpect, String> {
        match spec {
            None | Some("all") => Ok(EachExpect::All),
            Some(s) => Ok(EachExpect::Std(
                Expect::parse(s).map_err(|e| format!("invalid --expect: {e}"))?,
            )),
        }
    }

    fn eval(&self, ok: u64, total: u64) -> Verdict {
        match self {
            EachExpect::All => Expect::Eq(total).eval(ok),
            EachExpect::Std(e) => e.eval(ok),
        }
    }

    fn label(&self, spec: Option<&str>) -> String {
        spec.unwrap_or("all").to_string()
    }
}

/// One item's planned, fully-expanded invocation.
struct Planned {
    index: usize, // 1-based
    item: String,
    argv: Vec<String>,
}

impl Planned {
    fn display(&self) -> String {
        self.argv.join(" ")
    }
}

/// What one executed item produced, for reporting.
struct ItemResult {
    index: usize,
    item: String,
    cmd: String,
    code: String,
    verdict: Verdict,
}

/// The refusal shown when a dispatch target is not permitted: what was blocked
/// and the full, fixed set of runnable commands.
fn deny_message(name: &str, mutating: bool) -> String {
    let base = allowlist::BUILTIN.join(" ");
    let extra = allowlist::MUTATING_SUITE.join(" ");
    let hint = if mutating {
        String::new()
    } else {
        format!("\nWith --mutating it can also run: {extra}\n")
    };
    format!(
        "ct-each: '{name}' is not an allowed dispatch target, so nothing was run.\n\
         \n\
         ct-each runs this fixed set of commands:\n  \
         {base} ct-test\n\
         {hint}\
         \n\
         The list is immutable; ct-each does not run other commands, and there \
         is no shell mode.\n"
    )
}

/// Expand the command template for every item and gate each expanded argv,
/// before anything runs — so a refusal can never strike mid-sweep.
fn plan(cli: &Cli, items: &[String]) -> Result<Vec<Planned>, String> {
    let mut planned = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let index = (i + 1).to_string();
        let tokens = [("ITEM", item.as_str()), ("INDEX", index.as_str())];
        let argv: Vec<String> = cli
            .command
            .iter()
            .map(|part| template::render(part, &tokens))
            .collect();
        let name = allowlist::gated_name(&argv[0]);
        if !allowlist::is_allowed_for_each(&name, cli.mutating) {
            eprint!("{}", deny_message(&name, cli.mutating));
            return Err(format!("refused dispatch target '{name}'"));
        }
        planned.push(Planned {
            index: i + 1,
            item: item.clone(),
            argv,
        });
    }
    Ok(planned)
}

/// Run one planned item to completion under the per-item timeout.
fn run_item(p: &Planned, timeout: Option<std::time::Duration>) -> Result<Outcome, String> {
    let name = allowlist::gated_name(&p.argv[0]);
    let mut command = Command::new(supervise::resolve_program(&p.argv[0], &name));
    command.args(&p.argv[1..]);
    supervise::run_captured(command, None, timeout)
        .map_err(|e| format!("item {} ('{}'): {e}", p.index, p.item))
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    if cli.command.is_empty() {
        return Err("missing command: supply one after `--`, e.g. `ct-each --items a b -- ct-view {ITEM}`".to_string());
    }
    // Resolve the payload schemes per item: file:PATH expands to the file's
    // non-empty lines, text:VALUE stays one literal item.
    let mut items: Vec<String> = Vec::with_capacity(cli.items.len());
    for raw in &cli.items {
        let resolved = payload::resolve(raw)?;
        if resolved.from_file {
            items.extend(
                resolved
                    .text
                    .lines()
                    .map(str::trim_end)
                    .filter(|l| !l.is_empty())
                    .map(String::from),
            );
        } else {
            items.push(resolved.text);
        }
    }
    // Walker item source: matched file paths become items, in walk order.
    if cli.base.is_some() || cli.name.is_some() || !cli.ext.is_empty() {
        let mut name_spec = cli.name.clone().unwrap_or_default();
        for e in &cli.ext {
            let e = e.trim().trim_start_matches('.');
            if e.is_empty() {
                continue;
            }
            if !name_spec.is_empty() {
                name_spec.push('|');
            }
            name_spec.push_str(&format!("*.{e}"));
        }
        let names = if name_spec.is_empty() {
            None
        } else {
            Some(
                pattern::compile_name_set_with(&name_spec, cli.mode)
                    .map_err(|e| format!("invalid --name/--ext pattern: {e}"))?,
            )
        };
        let selector = walk::Selector {
            base: cli.base.clone().unwrap_or_else(|| PathBuf::from(".")),
            names,
            types: vec![EntryType::F],
            size: None,
            hidden: cli.hidden,
            follow: cli.follow,
        };
        for entry in selector.walk() {
            let entry = entry?;
            if entry.file_type().is_file() {
                items.push(entry.path().display().to_string());
            }
        }
    }
    if cli.stdin {
        let mut text = String::new();
        use std::io::Read;
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| format!("reading items from stdin: {e}"))?;
        items.extend(
            text.lines()
                .map(str::trim_end)
                .filter(|l| !l.is_empty())
                .map(String::from),
        );
    }
    if items.is_empty() {
        return Err("no items: supply --items and/or --stdin".to_string());
    }

    let expect = EachExpect::parse(cli.expect.as_deref())?;
    let timeout = cli.timeout.map(|v| pulse::secs("--timeout", v)).transpose()?;
    let planned = plan(&cli, &items)?;
    let total = planned.len();

    if !cli.json
        && !cli.quiet
        && let Some(q) = &cli.question
    {
        println!("== {q} ==");
    }

    if cli.dry_run {
        if cli.json {
            let cmds: Vec<_> = planned
                .iter()
                .map(|p| json!({ "index": p.index, "item": p.item, "cmd": p.display() }))
                .collect();
            println!(
                "{}",
                json!({ "tool": "ct-each", "dry_run": true, "total": total, "items": cmds })
            );
        } else {
            for p in &planned {
                println!("would run: {}", p.display());
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    let state = PulseState::new();
    state.set("QUESTION", cli.question.as_deref().unwrap_or(""));
    state.set("TOTAL", &total.to_string());
    let pulse_guard = cli.heartbeat.start("ct-each", state.clone())?;

    let mut results: Vec<ItemResult> = Vec::with_capacity(total);
    let mut ok = 0u64;
    let mut skipped = 0usize;
    for p in &planned {
        if cli.fail_fast && results.iter().any(|r| r.verdict == Verdict::Error) {
            skipped = total - results.len();
            break;
        }
        state.set("ITEM", &p.item);
        state.set("INDEX", &p.index.to_string());
        state.set("DONE", &results.len().to_string());

        let outcome = run_item(p, timeout)?;
        let code = if outcome.timed_out {
            "timeout".to_string()
        } else {
            match outcome.status.and_then(|s| s.code()) {
                Some(c) => c.to_string(),
                None => "signal".to_string(),
            }
        };
        let verdict = if !outcome.timed_out && code == "0" {
            Verdict::Success
        } else {
            Verdict::Error
        };
        if verdict == Verdict::Success {
            ok += 1;
        }

        if cli.show_output {
            use std::io::Write;
            std::io::stdout().write_all(outcome.stdout.as_bytes()).ok();
            std::io::stderr().write_all(outcome.stderr.as_bytes()).ok();
        }
        // A red item is never unexplained: name it, with its code, on stderr.
        if verdict == Verdict::Error && !cli.json {
            eprintln!(
                "ct-each: [{}/{}] '{}' -> ERROR (exit={code})",
                p.index, total, p.item
            );
        }

        let index_s = p.index.to_string();
        let cmd_s = p.display();
        let item_tokens = [
            ("RESULT", verdict.label()),
            ("ITEM", p.item.as_str()),
            ("INDEX", index_s.as_str()),
            ("CODE", code.as_str()),
            ("CMD", cmd_s.as_str()),
            ("STDOUT", outcome.stdout.trim_end_matches('\n')),
            ("STDERR", outcome.stderr.trim_end_matches('\n')),
        ];
        if !cli.json {
            if let Some(t) = &cli.emit_each {
                println!("{}", template::render(t, &item_tokens));
            } else if !cli.quiet {
                println!("{} {}", verdict.label(), p.item);
            }
        }

        results.push(ItemResult {
            index: p.index,
            item: p.item.clone(),
            cmd: p.display(),
            code,
            verdict,
        });
    }
    drop(pulse_guard);

    let errors = results.len() as u64 - ok;
    let verdict = expect.eval(ok, total as u64);
    let expect_label = expect.label(cli.expect.as_deref());
    let reason = format!(
        "--expect {expect_label}: {ok}/{total} succeeded ({errors} error(s), {skipped} skipped)"
    );

    if cli.json {
        let item_objs: Vec<_> = results
            .iter()
            .map(|r| {
                json!({
                    "index": r.index,
                    "item": r.item,
                    "cmd": r.cmd,
                    "code": r.code,
                    "result": r.verdict.label(),
                })
            })
            .collect();
        let obj = json!({
            "tool": "ct-each",
            "verdict": verdict.label(),
            "expect": expect_label,
            "ok": ok,
            "errors": errors,
            "skipped": skipped,
            "total": total,
            "items": item_objs,
        });
        println!("{obj}");
    } else {
        if verdict == Verdict::Error {
            eprintln!("ct-each: {reason}");
        }
        if !cli.quiet && cli.emit.is_none() {
            println!("{ok}/{total} item(s) succeeded -> {}", verdict.label());
        }
        let ok_s = ok.to_string();
        let errors_s = errors.to_string();
        let skipped_s = skipped.to_string();
        let total_s = total.to_string();
        let tokens = [
            ("RESULT", verdict.label()),
            ("OK", ok_s.as_str()),
            ("ERRORS", errors_s.as_str()),
            ("SKIPPED", skipped_s.as_str()),
            ("TOTAL", total_s.as_str()),
            ("QUESTION", cli.question.as_deref().unwrap_or("")),
            ("EXPECT", expect_label.as_str()),
            ("REASON", reason.as_str()),
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
            eprintln!("ct-each: {msg}");
            ExitCode::from(2)
        }
    }
}
