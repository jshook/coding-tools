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
//! `--list-allowed` prints that set without running anything.
//!
//! Two **built-in** probes cover the one wait the allowlist could not express,
//! "a local service is listening now": `--tcp-port` and `--http`, both
//! loopback-only (see [`coding_tools::probe`]). Each probe run is bounded by
//! `--probe-timeout` when given, and never by more than what is left of
//! `--timeout` — a run stopped by the overall deadline reports how long it had
//! been running, so a too-slow probe diagnoses itself instead of reading as a
//! false negative.
//!
//! Reachable directly or as `ct await`. The canonical reference is
//! `docs/explain/ct-await.md`; `docs/explain/ct-await.json` is the MCP
//! tool-use definition. Both are embedded below.

use std::process::{Command, ExitCode};
use std::time::Instant;

use coding_tools::allowlist;
use coding_tools::cli::ct_await::Cli;
use coding_tools::explain::Format;
use coding_tools::pattern;
use coding_tools::probe;
use coding_tools::pulse::{self, PulseState};
use coding_tools::supervise;
use coding_tools::template;
use coding_tools::verdict::Verdict;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-await.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-await.json");

/// The refusal shown for a non-gated probe.
fn deny_message(name: &str) -> String {
    let base = allowlist::builtin().join(" ");
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

/// What one tick of the wait polls.
enum Probe {
    /// An allow-listed argv, launched directly (never through a shell).
    Argv { argv: Vec<String>, name: String },
    /// Built-in: connect once to a loopback TCP port.
    Tcp {
        addrs: Vec<std::net::SocketAddr>,
        port: u16,
    },
    /// Built-in: one `GET` to a loopback `http://` URL.
    Http {
        target: probe::HttpTarget,
        url: String,
        ok_status: u16,
    },
}

/// What one probe run produced, normalised across the argv and built-in forms
/// so the wait loop's matcher precedence is identical for both.
struct Run {
    /// Text the matchers search — a command's stdout, or a built-in's detail.
    out: String,
    /// A command's stderr; empty for a built-in probe.
    err: String,
    /// The probe's own verdict: exit 0, or the endpoint answering.
    established: bool,
    /// Whether the run was cut short by its bound rather than finishing.
    cut_short: bool,
}

impl Probe {
    /// How the probe is named in messages and the `{CMD}` token.
    fn label(&self) -> String {
        match self {
            Probe::Argv { argv, .. } => argv.join(" "),
            Probe::Tcp { port, .. } => format!("--tcp-port {port}"),
            Probe::Http { url, ok_status, .. } => format!("--http {url} --ok-status {ok_status}"),
        }
    }

    /// Run once, bounded by `bound`.
    fn run_once(&self, bound: std::time::Duration) -> Result<Run, String> {
        match self {
            Probe::Argv { argv, name } => {
                let mut command = Command::new(supervise::resolve_program(&argv[0], name));
                command.args(&argv[1..]);
                let outcome = supervise::run_captured(command, None, Some(bound))
                    .map_err(|e| format!("probe '{}': {e}", argv[0]))?;
                Ok(Run {
                    established: outcome.status.is_some_and(|s| s.success()),
                    cut_short: outcome.timed_out,
                    out: outcome.stdout,
                    err: outcome.stderr,
                })
            }
            // The endpoint probes bound themselves via the socket's own connect
            // and read timeouts, so they never need killing.
            Probe::Tcp { addrs, .. } => {
                let r = probe::probe_tcp(addrs, bound);
                Ok(Run {
                    out: r.detail,
                    err: String::new(),
                    established: r.established,
                    cut_short: false,
                })
            }
            Probe::Http {
                target, ok_status, ..
            } => {
                let r = probe::probe_http(target, *ok_status, bound);
                Ok(Run {
                    out: r.detail,
                    err: String::new(),
                    established: r.established,
                    cut_short: false,
                })
            }
        }
    }
}

/// Resolve the requested probe, refusing a non-gated argv and a non-loopback
/// endpoint. Exactly one probe form must be given.
fn select_probe(cli: &Cli) -> Result<Result<Probe, ExitCode>, String> {
    let builtin = cli.tcp_port.is_some() || cli.http.is_some();
    if builtin && !cli.probe.is_empty() {
        return Err(
            "give one probe: either a built-in (--tcp-port/--http) or an argv after `--`"
                .to_string(),
        );
    }
    if let Some(port) = cli.tcp_port {
        return Ok(Ok(Probe::Tcp {
            addrs: probe::loopback_addrs("127.0.0.1", port)?,
            port,
        }));
    }
    if let Some(url) = &cli.http {
        return Ok(Ok(Probe::Http {
            target: probe::parse_http_url(url)?,
            url: url.clone(),
            ok_status: cli.ok_status,
        }));
    }
    if cli.probe.is_empty() {
        return Err("missing probe: supply one after `--`, e.g. `ct-await --timeout 600 -- ct-search --base target/build.log --grep 'BUILD SUCCESS' --quiet`, or use a built-in (--tcp-port 8080). `--list-allowed` prints every option.".to_string());
    }
    let name = allowlist::gated_name(&cli.probe[0]);
    // `ct-await` is on the read-only allowlist (so other tools can poll it), but
    // it must not poll *itself* — no self-nesting, the same guard ct-each has.
    let gated_ok = allowlist::is_allowed_for_each(&name, false)
        && name != "ct-await"
        && !(name == "ct-each" && cli.probe.iter().any(|a| a == "--mutating"));
    if !gated_ok {
        eprint!("{}", deny_message(&name));
        return Ok(Err(ExitCode::from(2)));
    }
    Ok(Ok(Probe::Argv {
        argv: cli.probe.clone(),
        name,
    }))
}

/// Whether the overall bound has arrived.
fn limit_reached(started: Instant, limit: std::time::Duration) -> bool {
    started.elapsed() >= limit
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    if cli.list_allowed {
        print!("{}", allowlist::listing(allowlist::Scope::Await));
        return Ok(ExitCode::SUCCESS);
    }
    let probe = match select_probe(&cli)? {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    let every = pulse::secs("--every", cli.every)?;
    // Bounded by design: `--timeout` is mandatory for a wait. It is not a clap
    // `required` only so the flags that run no wait — `--list-allowed`,
    // `--explain` — can be reached without inventing a bound for them.
    let Some(timeout) = cli.timeout else {
        return Err(
            "missing --timeout: every wait is bounded, so name the bound in seconds \
             (e.g. --timeout 600)"
                .to_string(),
        );
    };
    let limit = pulse::secs("--timeout", timeout)?;
    let per_run = cli
        .probe_timeout
        .map(|v| pulse::secs("--probe-timeout", v))
        .transpose()?;
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
    let cmdline = probe.label();

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
    // Runs that hit `--probe-timeout` and were retried rather than being fatal.
    // Reported at the end: a wait that expired while *every* run was being cut
    // short is a probe-too-slow problem, and the outcome line has to say so.
    let mut cut_runs = 0u64;
    // What the most recent run reported, so an expiry can say what was actually
    // observed instead of only that the condition never held.
    let mut last_seen = String::new();
    let (verdict, reason) = loop {
        let remaining = limit.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break (
                Verdict::Error,
                probe::expired_reason(limit, ticks, cut_runs, per_run, &last_seen),
            );
        }
        ticks += 1;
        state.set("TICKS", &ticks.to_string());
        // A run is bounded by --probe-timeout when given, and never by more than
        // what is left of --timeout.
        let bound = per_run.map_or(remaining, |p| p.min(remaining));
        let run_started = Instant::now();
        let outcome = probe.run_once(bound)?;
        let run_took = run_started.elapsed();
        last_seen = if outcome.err.trim().is_empty() {
            outcome.out.clone()
        } else {
            outcome.err.clone()
        };
        // Matcher precedence is exactly ct-test's: a failure signal is
        // decisive; a supplied ok-match is the required success proof.
        if let Some(re) = &err_re
            && (re.is_match(&outcome.out) || re.is_match(&outcome.err))
        {
            break (
                Verdict::Error,
                format!(
                    "--err-match '{}' matched on probe run {ticks}",
                    cli.err_match.as_deref().unwrap_or("")
                ),
            );
        }
        if outcome.cut_short {
            // Two very different things wear the same "killed" hat, and the old
            // message conflated them. A run stopped by its own --probe-timeout
            // is just "not yet" and the wait continues; a run stopped because
            // the overall --timeout arrived mid-run ends the wait — and says how
            // long that run had been going, so "narrow the probe" is visibly the
            // fix rather than looking like a false negative.
            if per_run.is_some_and(|p| run_took >= p) && !limit_reached(started, limit) {
                cut_runs += 1;
            } else {
                break (
                    Verdict::Error,
                    probe::deadline_reason(limit, ticks, run_took),
                );
            }
        } else if match &ok_re {
            Some(re) => re.is_match(&outcome.out) || re.is_match(&outcome.err),
            None => outcome.established,
        } {
            break (
                Verdict::Success,
                format!(
                    "condition established after {}s ({ticks} run(s))",
                    started.elapsed().as_secs()
                ),
            );
        }
        // Not yet: a non-zero exit, a missing required ok-match, or a run cut
        // short by --probe-timeout. All are ordinary (a file that does not exist
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
            eprintln!("ct-await: {msg}");
            ExitCode::from(2)
        }
    }
}
