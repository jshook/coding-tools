// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-test` — framed experiment runner.
//!
//! Runs a command, classifies the outcome from stdout/stderr pattern matches,
//! and emits a templated verdict; reachable directly or as `ct test`. The run
//! is bounded by `--timeout` (the child's process group is killed and the
//! verdict is `ERROR`) and observable via `--heartbeat`. There is no shell
//! mode: the command is always launched directly. The canonical,
//! self-contained reference is `docs/explain/ct-test.md` — the same text this
//! tool emits for `--explain md`; `docs/explain/ct-test.json` is the MCP
//! tool-use definition emitted for `--explain json`. Both are embedded below.

use std::process::{Command, ExitCode, ExitStatus};

use clap::Parser;
use coding_tools::allowlist;
use coding_tools::explain::Format;
use coding_tools::pattern;
use coding_tools::payload;
use coding_tools::pulse::{self, HeartbeatOpts, PulseState};
use coding_tools::supervise::{self, Outcome};
use coding_tools::template;
use coding_tools::testrun::focus_block;
use coding_tools::verdict::Verdict;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-test.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-test.json");

#[derive(Parser, Debug)]
#[command(
    name = "ct-test",
    version,
    about = "Run a command as a framed experiment and emit a templated SUCCESS/ERROR verdict.",
    long_about = "ct-test frames a command with the question it answers, classifies the result from \
                  what the command prints (not only its exit code), and emits a templated verdict \
                  (also reachable as `ct test`). The command is always launched directly — there is \
                  no shell mode. See `ct-test --explain` for agent-oriented documentation."
)]
struct Cli {
    /// Question this experiment answers; printed as a "== ... ==" banner.
    #[arg(long)]
    question: Option<String>,

    /// Program to run (must be on the fixed read-only allowlist).
    #[arg(long)]
    cmd: Option<String>,

    /// Text written to the child's standard input. Accepts file:PATH / text:VALUE payloads.
    #[arg(long)]
    stdin: Option<String>,

    /// Pin how matcher patterns are interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    mode: Option<pattern::Mode>,

    /// Kill the command and classify ERROR if it runs longer than SECS seconds (fractional allowed); {CODE} becomes "timeout".
    #[arg(long, value_name = "SECS")]
    timeout: Option<f64>,

    #[command(flatten)]
    heartbeat: HeartbeatOpts,

    /// Match in stdout OR stderr forces ERROR (synonym for the -stdout/-stderr pair).
    #[arg(long)]
    err_match: Option<String>,

    /// Match in stdout forces ERROR.
    #[arg(long)]
    err_match_stdout: Option<String>,

    /// Match in stderr forces ERROR.
    #[arg(long)]
    err_match_stderr: Option<String>,

    /// Match in stdout OR stderr indicates SUCCESS (synonym for the -stdout/-stderr pair).
    #[arg(long)]
    ok_match: Option<String>,

    /// Match in stdout indicates SUCCESS.
    #[arg(long)]
    ok_match_stdout: Option<String>,

    /// Match in stderr indicates SUCCESS.
    #[arg(long)]
    ok_match_stderr: Option<String>,

    /// Verdict when neither an --ok-match nor an --err-match matched: success, error, or exit (follow the exit code). Default: error if any --ok-match was given, else exit.
    #[arg(long, value_enum)]
    otherwise: Option<Otherwise>,

    /// Distil captured output to lines matching this pattern (with --context around each), printed to stderr and available as {FOCUS}.
    #[arg(long)]
    focus: Option<String>,

    /// Lines of context shown around each --focus match.
    #[arg(long, default_value_t = 2)]
    context: usize,

    /// Keep only the last N lines of each captured stream in the {STDOUT}/{STDERR} emit tokens (matchers and --focus still see everything).
    #[arg(long, value_name = "N")]
    capture_tail: Option<usize>,

    /// Template written to stdout after running. Tokens: {RESULT} {CODE} {QUESTION} {CMD} {STDOUT} {STDERR} {REASON} {FOCUS}.
    #[arg(long, alias = "emit-stdout")]
    emit: Option<String>,

    /// Template written to stderr after running (same tokens as --emit).
    #[arg(long)]
    emit_stderr: Option<String>,

    /// Also pass the child's stdout/stderr through verbatim.
    #[arg(long)]
    show_output: bool,

    /// Suppress the question banner.
    #[arg(long)]
    quiet: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    explain: Option<Format>,

    /// Arguments passed through to --cmd (after `--`).
    #[arg(last = true)]
    args: Vec<String>,
}

/// Render an exit status as a token for `{CODE}` (`timeout` when the run was
/// killed by `--timeout`).
fn code_token(status: Option<&ExitStatus>) -> String {
    let Some(status) = status else {
        return "timeout".to_string();
    };
    if let Some(code) = status.code() {
        return code.to_string();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("signal:{sig}");
        }
    }
    "unknown".to_string()
}

/// What an *inconclusive* run resolves to — neither an `--ok-match` nor an
/// `--err-match` fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Otherwise {
    /// Treat an inconclusive run as `SUCCESS`.
    Success,
    /// Treat an inconclusive run as `ERROR` (fail-closed).
    Error,
    /// Follow the child's exit status (`0` ⇒ `SUCCESS`).
    Exit,
}

impl Otherwise {
    fn label(self) -> &'static str {
        match self {
            Otherwise::Success => "success",
            Otherwise::Error => "error",
            Otherwise::Exit => "exit",
        }
    }
}

/// Resolve the [`Verdict`] **and a one-line reason** from the matchers and the
/// child's exit status.
///
/// `ct-test` is *fail-closed*: it reports `SUCCESS` only when success is
/// positively established. The precedence is
///
/// 0. the run timed out → `ERROR` (decisive — partial output proves nothing);
/// 1. any `--err-match*` hits → `ERROR` (a failure signal is decisive);
/// 2. else any `--ok-match*` hits → `SUCCESS` (positive proof);
/// 3. else *inconclusive* → the [`Otherwise`] policy from `--otherwise`, whose
///    default is `error` when an `--ok-match` was required (so an absent proof is
///    a failure even on a clean exit) and `exit` otherwise.
///
/// The reason names which rule fired and, for an unmet `--ok-match`, which stream
/// was searched — so a stream mismatch (e.g. success on stdout, `--ok-match-stderr`)
/// is diagnosable rather than a silent red.
fn classify_result(cli: &Cli, outcome: &Outcome) -> Result<(Verdict, String), String> {
    // 0. A timeout is decisive: the experiment did not complete, so no match in
    //    its partial output can establish success.
    if outcome.timed_out {
        let label = pulse::limit_label(pulse::secs("--timeout", cli.timeout.unwrap_or(0.0))?);
        return Ok((
            Verdict::Error,
            format!("timed out after {label}; the command's process group was killed"),
        ));
    }
    let status = outcome.status.as_ref().expect("not timed out");
    let (stdout, stderr) = (outcome.stdout.as_str(), outcome.stderr.as_str());

    let hit = |pat: &str, hay: &str| -> Result<bool, String> {
        Ok(pattern::compile_with(pat, cli.mode)
            .map_err(|e| format!("invalid pattern '{pat}': {e}"))?
            .is_match(hay))
    };
    let check = |p: &str, in_out: bool, in_err: bool| -> Result<bool, String> {
        Ok((in_out && hit(p, stdout)?) || (in_err && hit(p, stderr)?))
    };
    let stream = |in_out: bool, in_err: bool| match (in_out, in_err) {
        (true, true) => "stdout/stderr",
        (true, false) => "stdout",
        (false, true) => "stderr",
        _ => "nothing",
    };

    // (pattern, search stdout, search stderr, option name)
    let err_specs = [
        (cli.err_match.as_deref(), true, true, "--err-match"),
        (
            cli.err_match_stdout.as_deref(),
            true,
            false,
            "--err-match-stdout",
        ),
        (
            cli.err_match_stderr.as_deref(),
            false,
            true,
            "--err-match-stderr",
        ),
    ];
    let ok_specs = [
        (cli.ok_match.as_deref(), true, true, "--ok-match"),
        (
            cli.ok_match_stdout.as_deref(),
            true,
            false,
            "--ok-match-stdout",
        ),
        (
            cli.ok_match_stderr.as_deref(),
            false,
            true,
            "--ok-match-stderr",
        ),
    ];
    let err_specified = err_specs.iter().any(|(p, ..)| p.is_some());
    let ok_specified = ok_specs.iter().any(|(p, ..)| p.is_some());

    // 1. A failure signal is decisive.
    for (pat, in_out, in_err, name) in err_specs {
        if let Some(p) = pat
            && check(p, in_out, in_err)?
        {
            return Ok((
                Verdict::Error,
                format!("{name} '{p}' matched {}", stream(in_out, in_err)),
            ));
        }
    }

    // 2. A positive proof is decisive.
    let mut ok_misses: Vec<String> = Vec::new();
    for (pat, in_out, in_err, name) in ok_specs {
        if let Some(p) = pat {
            if check(p, in_out, in_err)? {
                return Ok((
                    Verdict::Success,
                    format!("{name} '{p}' matched {}", stream(in_out, in_err)),
                ));
            }
            ok_misses.push(format!(
                "{name} '{p}' not found in {}",
                stream(in_out, in_err)
            ));
        }
    }

    // 3. Inconclusive: the caller's --otherwise policy decides. The default is
    //    fail-closed when a success proof was required, else follow the exit code.
    let policy = cli.otherwise.unwrap_or(if ok_specified {
        Otherwise::Error
    } else {
        Otherwise::Exit
    });
    let basis = if !ok_misses.is_empty() {
        ok_misses.join("; ")
    } else if err_specified {
        "no --err-match matched".to_string()
    } else {
        "no match assertions".to_string()
    };
    let note = match cli.otherwise {
        Some(_) => format!(" (--otherwise={})", policy.label()),
        None => String::new(),
    };
    let reason = format!("{basis}; exit={}{note}", code_token(Some(status)));
    let verdict = match policy {
        Otherwise::Success => Verdict::Success,
        Otherwise::Error => Verdict::Error,
        Otherwise::Exit => {
            if status.success() {
                Verdict::Success
            } else {
                Verdict::Error
            }
        }
    };
    Ok((verdict, reason))
}

/// The command line as a single display string for the `{CMD}` token.
fn cmd_display(cli: &Cli) -> String {
    let mut parts = vec![cli.cmd.clone().unwrap_or_default()];
    parts.extend(cli.args.iter().cloned());
    parts.join(" ")
}

/// The refusal shown when a command is not on the fixed allowlist: what was
/// blocked and the full set of commands `ct-test` is permitted to run.
fn deny_message(name: &str) -> String {
    let allowed = allowlist::BUILTIN.join(" ");
    format!(
        "ct-test: '{name}' is not on the allowlist, so nothing was run.\n\
         \n\
         ct-test runs only this fixed set of read-only commands:\n  \
         {allowed}\n\
         \n\
         The list is immutable; ct-test does not run other commands, and there \
         is no shell mode.\n"
    )
}

/// The last `n` lines of `text`, with an elision marker when lines were cut.
/// Bounds the `{STDOUT}`/`{STDERR}` emit tokens under `--capture-tail`.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= n {
        return text.to_string();
    }
    let omitted = lines.len() - n;
    let mut out = format!("(... {omitted} earlier line(s) omitted)\n");
    out.push_str(&lines[omitted..].join("\n"));
    out
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let cmd_str = cli
        .cmd
        .as_deref()
        .ok_or("missing required option --cmd")?
        .to_string();

    let name = allowlist::gated_name(&cmd_str);
    if !allowlist::is_allowed(&name) {
        eprint!("{}", deny_message(&name));
        return Ok(ExitCode::from(2));
    }

    let timeout = cli.timeout.map(|v| pulse::secs("--timeout", v)).transpose()?;
    let cmdline = cmd_display(&cli);

    if !cli.quiet
        && let Some(q) = &cli.question
    {
        println!("== {q} ==");
    }

    let mut command = Command::new(supervise::resolve_program(&cmd_str, &name));
    command.args(&cli.args);

    // The pulse runs exactly as long as the child does, so no heartbeat line
    // can land after the verdict output.
    let state = PulseState::new();
    state.set("QUESTION", cli.question.as_deref().unwrap_or(""));
    state.set("CMD", &cmdline);
    let pulse_guard = cli.heartbeat.start("ct-test", state)?;

    let stdin_text = match &cli.stdin {
        Some(raw) => Some(payload::resolve(raw)?.text),
        None => None,
    };
    let outcome = supervise::run_captured(command, stdin_text.as_deref(), timeout)
        .map_err(|e| format!("'{cmd_str}': {e}"))?;
    drop(pulse_guard);

    if cli.show_output {
        use std::io::Write;
        std::io::stdout().write_all(outcome.stdout.as_bytes()).ok();
        std::io::stderr().write_all(outcome.stderr.as_bytes()).ok();
    }

    let (verdict, reason) = classify_result(&cli, &outcome)?;
    let code = code_token(outcome.status.as_ref());

    // Distil the captured output to the lines that matter, if asked.
    let focus = match &cli.focus {
        Some(pat) => {
            let re = pattern::compile_with(pat, cli.mode)
                .map_err(|e| format!("invalid --focus pattern: {e}"))?;
            let mut blocks = Vec::new();
            if let Some(b) = focus_block(&outcome.stdout, &re, cli.context) {
                blocks.push(format!("stdout (focus):\n{b}"));
            }
            if let Some(b) = focus_block(&outcome.stderr, &re, cli.context) {
                blocks.push(format!("stderr (focus):\n{b}"));
            }
            blocks.join("\n")
        }
        None => String::new(),
    };

    // Matchers and --focus saw the full streams; only the emit tokens are
    // bounded by --capture-tail.
    let stdout_token = match cli.capture_tail {
        Some(n) => tail_lines(outcome.stdout.trim_end_matches('\n'), n),
        None => outcome.stdout.trim_end_matches('\n').to_string(),
    };
    let stderr_token = match cli.capture_tail {
        Some(n) => tail_lines(outcome.stderr.trim_end_matches('\n'), n),
        None => outcome.stderr.trim_end_matches('\n').to_string(),
    };

    let tokens = [
        ("RESULT", verdict.label()),
        ("CODE", code.as_str()),
        ("QUESTION", cli.question.as_deref().unwrap_or("")),
        ("CMD", cmdline.as_str()),
        ("STDOUT", stdout_token.as_str()),
        ("STDERR", stderr_token.as_str()),
        ("REASON", reason.as_str()),
        ("FOCUS", focus.as_str()),
    ];

    // On ERROR, always surface the reason so a verdict is never an unexplained
    // red — in particular, an unmet --ok-match on the wrong stream is diagnosable.
    // (`--quiet` governs only the question banner, not diagnostics.)
    if verdict == Verdict::Error {
        eprintln!("ct-test: {reason}");
    }
    // The focused slice goes to stderr so it never pollutes an --emit on stdout.
    if !focus.is_empty() {
        eprintln!("{focus}");
    }

    if let Some(t) = &cli.emit {
        println!("{}", template::render(t, &tokens));
    }
    if let Some(t) = &cli.emit_stderr {
        eprintln!("{}", template::render(t, &tokens));
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
            eprintln!("ct-test: {msg}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_lines_keeps_last_n_with_marker() {
        let text = "a\nb\nc\nd";
        assert_eq!(tail_lines(text, 4), text);
        assert_eq!(
            tail_lines(text, 2),
            "(... 2 earlier line(s) omitted)\nc\nd"
        );
    }
}
