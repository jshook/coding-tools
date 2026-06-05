// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `cttest` — framed experiment runner.
//!
//! Runs a command, classifies the outcome from stdout/stderr pattern matches,
//! and emits a templated verdict. The canonical, self-contained reference is
//! `docs/explain/cttest.md` — the same text this tool emits for `--explain md`;
//! `docs/explain/cttest.json` is the MCP tool-use definition emitted for
//! `--explain json`. Both are embedded below.

use std::io::Write;
use std::process::{Command, ExitCode, ExitStatus, Stdio};

use clap::Parser;
use coding_tools::explain::Format;
use coding_tools::pattern;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/cttest.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/cttest.json");

#[derive(Parser, Debug)]
#[command(
    name = "cttest",
    version,
    about = "Run a command as a framed experiment and emit a templated SUCCESS/ERROR verdict.",
    long_about = "cttest frames a command with the question it answers, classifies the result from \
                  what the command prints (not only its exit code), and emits a templated verdict. \
                  See `cttest --explain` for agent-oriented documentation."
)]
struct Cli {
    /// Question this experiment answers; printed as a "== ... ==" banner.
    #[arg(long)]
    question: Option<String>,

    /// Program to run (or, with --shell, a shell command line).
    #[arg(long)]
    cmd: Option<String>,

    /// Interpret --cmd as a shell line via `sh -c` (enables pipes/redirection).
    #[arg(long)]
    shell: bool,

    /// Literal text written to the child's standard input.
    #[arg(long)]
    stdin: Option<String>,

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

    /// Template written to stdout after running. Tokens: {RESULT} {CODE} {QUESTION} {CMD} {STDOUT} {STDERR}.
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

    /// Arguments passed through to --cmd (after `--`); ignored with --shell.
    #[arg(last = true)]
    args: Vec<String>,
}

/// Render an exit status as a token for `{CODE}`.
fn code_token(status: &ExitStatus) -> String {
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

/// Resolve `{RESULT}` from the matchers and the child's exit status.
fn classify_result(
    cli: &Cli,
    stdout: &str,
    stderr: &str,
    status: &ExitStatus,
) -> Result<&'static str, String> {
    let hit = |pat: &str, hay: &str| -> Result<bool, String> {
        Ok(pattern::compile(pat)
            .map_err(|e| format!("invalid pattern '{pat}': {e}"))?
            .is_match(hay))
    };

    let mut err_hit = false;
    if let Some(p) = &cli.err_match {
        err_hit |= hit(p, stdout)? || hit(p, stderr)?;
    }
    if let Some(p) = &cli.err_match_stdout {
        err_hit |= hit(p, stdout)?;
    }
    if let Some(p) = &cli.err_match_stderr {
        err_hit |= hit(p, stderr)?;
    }

    let ok_specified =
        cli.ok_match.is_some() || cli.ok_match_stdout.is_some() || cli.ok_match_stderr.is_some();
    let mut ok_hit = false;
    if let Some(p) = &cli.ok_match {
        ok_hit |= hit(p, stdout)? || hit(p, stderr)?;
    }
    if let Some(p) = &cli.ok_match_stdout {
        ok_hit |= hit(p, stdout)?;
    }
    if let Some(p) = &cli.ok_match_stderr {
        ok_hit |= hit(p, stderr)?;
    }

    Ok(if err_hit {
        "ERROR"
    } else if ok_specified {
        if ok_hit { "SUCCESS" } else { "ERROR" }
    } else if status.success() {
        "SUCCESS"
    } else {
        "ERROR"
    })
}

/// The command line as a single display string for the `{CMD}` token.
fn cmd_display(cli: &Cli) -> String {
    let mut parts = vec![cli.cmd.clone().unwrap_or_default()];
    parts.extend(cli.args.iter().cloned());
    parts.join(" ")
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let cmd_str = cli
        .cmd
        .as_deref()
        .ok_or("missing required option --cmd")?
        .to_string();

    if !cli.quiet
        && let Some(q) = &cli.question
    {
        println!("== {q} ==");
    }

    let mut command = if cli.shell {
        let mut c = Command::new("sh");
        c.arg("-c").arg(&cmd_str);
        if !cli.args.is_empty() {
            // Provide $0 then the positional parameters for the shell snippet.
            c.arg("sh").args(&cli.args);
        }
        c
    } else {
        let mut c = Command::new(&cmd_str);
        c.args(&cli.args);
        c
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to launch '{cmd_str}': {e}"))?;

    if let Some(input) = &cli.stdin {
        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(input.as_bytes())
            .map_err(|e| format!("writing to child stdin: {e}"))?;
    } else {
        drop(child.stdin.take());
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("waiting for command: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if cli.show_output {
        std::io::stdout().write_all(&output.stdout).ok();
        std::io::stderr().write_all(&output.stderr).ok();
    }

    let result = classify_result(&cli, &stdout, &stderr, &output.status)?;
    let code = code_token(&output.status);
    let cmdline = cmd_display(&cli);
    let render = |tpl: &str| -> String {
        tpl.replace("{RESULT}", result)
            .replace("{CODE}", &code)
            .replace("{QUESTION}", cli.question.as_deref().unwrap_or(""))
            .replace("{CMD}", &cmdline)
            .replace("{STDOUT}", stdout.trim_end_matches('\n'))
            .replace("{STDERR}", stderr.trim_end_matches('\n'))
    };

    if let Some(t) = &cli.emit {
        println!("{}", render(t));
    }
    if let Some(t) = &cli.emit_stderr {
        eprintln!("{}", render(t));
    }

    Ok(if result == "SUCCESS" {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
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
            eprintln!("cttest: {msg}");
            ExitCode::from(2)
        }
    }
}
