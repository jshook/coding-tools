// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct` — umbrella launcher for the `coding_tools` suite.
//!
//! `ct <command> [args…]` runs the matching `ct-<command>` binary, the same
//! git-style external-subcommand convention `git`/`cargo` use: `ct search` runs
//! `ct-search`, `ct test` runs `ct-test`, and any other `ct-*` tool on `PATH`
//! (or installed beside `ct`) is reachable too. `ct` adds no behaviour of its
//! own beyond locating the tool and handing off — so a child's stdout, stderr,
//! and exit status pass straight through.
//!
//! For discovery it speaks the standard idioms — `ct --help`, `ct help
//! <command>` (→ `ct-<command> --help`), `ct --version` — plus the suite's
//! `--explain`: `ct --explain md` documents the umbrella, and `ct --explain
//! json` emits the whole suite as one tool-definition manifest an agent can
//! hoist in a single call. The canonical reference is `docs/explain/ct.md`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
/// `ct.json` is a manifest bundling every suite tool's definition.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct.json");

/// Built-in subcommands, for help and the usage banner. Dispatch itself is
/// generic — any `ct-<name>` resolves — so this is a curated index, not a limit.
const SUBCOMMANDS: &[(&str, &str)] = &[
    (
        "search",
        "Recursively find files by name, type, size, and content (ct-search)",
    ),
    (
        "view",
        "Show a file's lines by range, or regions around a pattern (ct-view)",
    ),
    (
        "tree",
        "Report a file tree with per-file line/word/char counts and filters (ct-tree)",
    ),
    (
        "edit",
        "Find/replace across files, gated by an --expect verdict and --dry-run (ct-edit)",
    ),
    (
        "patch",
        "Set/delete nodes by path in JSON/JSONC/JSONL, preserving formatting (ct-patch)",
    ),
    (
        "test",
        "Run a command as a framed experiment with a templated verdict (ct-test)",
    ),
    (
        "each",
        "Run a command template once per item, no shell, with an aggregate --expect verdict (ct-each)",
    ),
    (
        "outline",
        "Report the declarations in a file or tree: kind, name, start:end span (ct-outline)",
    ),
    (
        "okf",
        "Author and query Open Knowledge Format bundles: validate, list, links (ct-okf)",
    ),
    (
        "rules",
        "Record, promote, remove, and list the project's invariant rules (ct-rules)",
    ),
    (
        "check",
        "Verify the project's recorded invariants from .ct/rules.jsonc (ct-check)",
    ),
    (
        "await",
        "Poll a read-only probe until it succeeds, aborts, or times out (ct-await)",
    ),
];

/// The `ct --help` / usage text.
fn usage() -> String {
    let mut commands = String::new();
    for (name, blurb) in SUBCOMMANDS {
        commands.push_str(&format!("  {name:<9} {blurb}\n"));
    }
    format!(
        "ct — umbrella launcher for the coding_tools suite\n\
         \n\
         Usage:\n  \
         ct <command> [args...]    run the matching ct-<command> tool\n  \
         ct and <cmd...> ::: <cmd...>   run each in turn, stop at the first failure (shell-less &&)\n  \
         ct or  <cmd...> ::: <cmd...>   run each in turn, stop at the first success (shell-less ||)\n  \
         ct help [<command>]       show this help, or a command's own --help\n  \
         ct <command> --explain    print one tool's definition (md or json)\n  \
         ct --explain [md|json]    describe the whole suite (json = a manifest of every tool)\n  \
         ct completions [shell]    print the shell completion script (bash/zsh/fish; auto-detects if omitted)\n  \
         ct --version\n\
         \n\
         Commands:\n\
         {commands}\n\
         ct <command> runs ct-<command> — found beside ct or on PATH — the same way\n\
         git runs git-<command>, so any ct-* tool you install is reachable through ct.\n"
    )
}

/// Resolve `ct-<sub>` to launch: prefer a sibling of the running `ct`
/// executable (so a freshly built or bundled install works without PATH games),
/// else fall back to the bare name for `PATH` resolution at launch time.
fn resolve(child: &str) -> OsString {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate: PathBuf = dir.join(child);
        if candidate.is_file() {
            return candidate.into_os_string();
        }
    }
    OsString::from(child)
}

/// Hand off to `ct-<sub> [rest…]`, replacing this process on Unix so the child
/// owns the terminal and its exit status passes through unchanged.
fn dispatch(sub: &str, rest: &[String]) -> ExitCode {
    let program = resolve(&format!("ct-{sub}"));
    let mut command = Command::new(&program);
    command.args(rest);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec only returns if the hand-off failed.
        launch_error(sub, command.exec())
    }
    #[cfg(not(unix))]
    {
        match command.status() {
            Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(2)).unwrap_or(2)),
            Err(e) => launch_error(sub, e),
        }
    }
}

/// Print the friendly message for a failed hand-off (no exit).
fn print_launch_error(sub: &str, err: &std::io::Error) {
    if err.kind() == std::io::ErrorKind::NotFound {
        eprintln!("ct: unknown command '{sub}' — no 'ct-{sub}' found beside ct or on PATH");
        eprint!("\n{}", usage());
    } else {
        eprintln!("ct: could not run 'ct-{sub}': {err}");
    }
}

/// Turn a failed hand-off into a friendly message and exit `2`.
fn launch_error(sub: &str, err: std::io::Error) -> ExitCode {
    print_launch_error(sub, &err);
    ExitCode::from(2)
}

// ----- Boolean chains: `ct and` / `ct or` -------------------------------------

/// The separator between sub-command segments in a `ct and` / `ct or` chain.
/// Distinctive enough (GNU-parallel style) to not collide with ordinary flag
/// values; `--` is avoided because leaf tools consume it for trailing argv.
const CHAIN_SEP: &str = ":::";

/// Short-circuit boolean mode for a chain.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChainMode {
    /// Run left to right; stop at the first failure (non-zero) and return it.
    And,
    /// Run left to right; stop at the first success (zero) and return `0`.
    Or,
}

/// Split a chain's argv into segments on [`CHAIN_SEP`], rejecting blank ones
/// (a leading/trailing/doubled separator, or no commands at all).
fn split_segments(rest: &[String]) -> Result<Vec<&[String]>, String> {
    let segs: Vec<&[String]> = rest.split(|a| a == CHAIN_SEP).collect();
    if segs.iter().any(|s| s.is_empty()) {
        return Err(format!(
            "empty segment — separate sub-commands with '{CHAIN_SEP}' and don't leave one blank \
             (e.g. `ct and search … {CHAIN_SEP} edit …`)"
        ));
    }
    Ok(segs)
}

/// Run the segments under the short-circuit `mode`, deferring each segment's
/// execution to `run` (which returns its exit code). Pure control flow — the
/// real `run` spawns a child; tests inject codes — so the short-circuit logic
/// is verifiable without launching processes.
fn chain_code<R: FnMut(&str, &[String]) -> i32>(
    mode: ChainMode,
    segs: &[&[String]],
    mut run: R,
) -> i32 {
    let mut last = 0;
    for seg in segs {
        let code = run(seg[0].as_str(), &seg[1..]);
        match mode {
            ChainMode::And if code != 0 => return code,
            ChainMode::Or if code == 0 => return 0,
            ChainMode::Or => last = code,
            ChainMode::And => {}
        }
    }
    match mode {
        ChainMode::And => 0,   // every segment succeeded
        ChainMode::Or => last, // none succeeded; mirror `a || b`'s last code
    }
}

/// Spawn one `ct-<sub> [args…]` segment and wait, returning its exit code; a
/// launch failure prints the usual message and counts as `2`. Always spawns
/// (never `exec`s) so the chain can continue to later segments.
fn run_segment(sub: &str, args: &[String]) -> i32 {
    let program = resolve(&format!("ct-{sub}"));
    match Command::new(&program).args(args).status() {
        Ok(status) => status.code().unwrap_or(2),
        Err(e) => {
            print_launch_error(sub, &e);
            2
        }
    }
}

/// Map a raw exit code to a process `ExitCode` (clamping to a byte).
fn code_to_exit(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(2))
}

/// Entry point for `ct and …` / `ct or …`: split into segments and run them
/// under the chosen short-circuit mode.
fn run_chain(mode: ChainMode, kw: &str, rest: &[String]) -> ExitCode {
    match split_segments(rest) {
        Ok(segs) => code_to_exit(chain_code(mode, &segs, run_segment)),
        Err(msg) => {
            eprintln!("ct {kw}: {msg}");
            ExitCode::from(2)
        }
    }
}

/// Print the shell completion registration script. With no shell, emit the
/// auto-detecting wrapper (which itself re-invokes `ct completions --shell
/// <detected>`); a shell may be named with `--shell SHELL`, `--shell=SHELL`, or
/// bare `SHELL`.
fn completions(rest: &[String]) -> ExitCode {
    let shell: Option<&str> = match rest.first().map(String::as_str) {
        None => None,
        Some("--shell") => match rest.get(1) {
            Some(s) => Some(s.as_str()),
            None => {
                eprintln!("ct: --shell needs a value (bash, zsh, fish)");
                return ExitCode::from(2);
            }
        },
        Some(s) if s.starts_with("--shell=") => s.strip_prefix("--shell="),
        Some(s) if !s.starts_with('-') => Some(s),
        Some(other) => {
            eprintln!("ct: unknown completions argument '{other}'");
            return ExitCode::from(2);
        }
    };
    match shell {
        None => veks_completion::print_indirect_wrapper("ct"),
        Some(name) => match veks_completion::Shell::from_name(name) {
            Some(sh) => veks_completion::print_completions("ct", sh),
            None => {
                eprintln!("ct: unknown shell '{name}' (bash, zsh, fish)");
                return ExitCode::from(2);
            }
        },
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    // Dynamic shell completion: when the registration script's callback sets
    // `_CT_COMPLETE=bash`, veks-completion answers the request and we exit
    // before any normal dispatch. A no-op on every ordinary invocation.
    let tree = coding_tools::completion::command_tree();
    if veks_completion::handle_complete_env("ct", &tree) {
        return ExitCode::SUCCESS;
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(first) = args.first() else {
        // No command: a usage error, but show the banner so it is actionable.
        eprint!("{}", usage());
        return ExitCode::from(2);
    };

    match first.as_str() {
        "-h" | "--help" => {
            print!("{}", usage());
            ExitCode::SUCCESS
        }
        "-V" | "--version" => {
            println!("ct {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        // `ct help` and `ct help <command>`, the git-style discovery idiom.
        "help" => match args.get(1) {
            None => {
                print!("{}", usage());
                ExitCode::SUCCESS
            }
            Some(sub) => dispatch(sub, &["--help".to_string()]),
        },
        explain if explain == "--explain" || explain.starts_with("--explain=") => {
            let fmt = explain
                .strip_prefix("--explain=")
                .or(args.get(1).map(String::as_str))
                .unwrap_or("md");
            match fmt {
                "md" => {
                    print!("{EXPLAIN_MD}");
                    ExitCode::SUCCESS
                }
                "json" => {
                    print!("{EXPLAIN_JSON}");
                    ExitCode::SUCCESS
                }
                other => {
                    eprintln!("ct: unknown --explain format '{other}' (use md or json)");
                    ExitCode::from(2)
                }
            }
        }
        "completions" => completions(&args[1..]),
        // Shell-less boolean chains: run several ct sub-commands in one argv,
        // short-circuiting like `&&` / `||` but without a shell to interpret them.
        "and" => run_chain(ChainMode::And, "and", &args[1..]),
        "or" => run_chain(ChainMode::Or, "or", &args[1..]),
        flag if flag.starts_with('-') => {
            eprintln!("ct: unknown option '{flag}'");
            eprint!("\n{}", usage());
            ExitCode::from(2)
        }
        sub => dispatch(sub, &args[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build owned segments and a borrowing view for `chain_code`.
    fn segs(parts: &[&[&str]]) -> Vec<Vec<String>> {
        parts
            .iter()
            .map(|seg| seg.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    /// Run `chain_code` over `codes` (one per segment), recording which
    /// sub-commands actually ran. Returns (final code, ran subs).
    fn run(mode: ChainMode, names: &[&str], codes: &[i32]) -> (i32, Vec<String>) {
        let storage = segs(&names.iter().map(std::slice::from_ref).collect::<Vec<_>>());
        let view: Vec<&[String]> = storage.iter().map(Vec::as_slice).collect();
        let mut ran = Vec::new();
        let mut i = 0;
        let code = chain_code(mode, &view, |sub, _| {
            ran.push(sub.to_string());
            let c = codes[i];
            i += 1;
            c
        });
        (code, ran)
    }

    #[test]
    fn and_stops_at_first_failure_and_returns_it() {
        let (code, ran) = run(ChainMode::And, &["a", "b", "c"], &[0, 1, 0]);
        assert_eq!(code, 1);
        assert_eq!(ran, ["a", "b"]); // c never runs
    }

    #[test]
    fn and_runs_all_when_every_segment_succeeds() {
        let (code, ran) = run(ChainMode::And, &["a", "b"], &[0, 0]);
        assert_eq!(code, 0);
        assert_eq!(ran, ["a", "b"]);
    }

    #[test]
    fn and_propagates_a_two_abort_and_halts() {
        let (code, ran) = run(ChainMode::And, &["a", "b", "c"], &[0, 2, 0]);
        assert_eq!(code, 2);
        assert_eq!(ran, ["a", "b"]);
    }

    #[test]
    fn or_stops_at_first_success() {
        let (code, ran) = run(ChainMode::Or, &["a", "b", "c"], &[1, 0, 1]);
        assert_eq!(code, 0);
        assert_eq!(ran, ["a", "b"]); // c never runs
    }

    #[test]
    fn or_returns_last_code_when_all_fail() {
        let (code, ran) = run(ChainMode::Or, &["a", "b"], &[1, 2]);
        assert_eq!(code, 2);
        assert_eq!(ran, ["a", "b"]);
    }

    #[test]
    fn split_segments_breaks_on_the_separator() {
        let argv: Vec<String> = ["search", "--quiet", ":::", "edit", "--mutating"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let segs = split_segments(&argv).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0], ["search", "--quiet"]);
        assert_eq!(segs[1], ["edit", "--mutating"]);
    }

    #[test]
    fn split_segments_rejects_blank_segments() {
        let blank = |parts: &[&str]| {
            let argv: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
            split_segments(&argv).is_err()
        };
        assert!(blank(&[])); // `ct and` with nothing
        assert!(blank(&[":::", "edit"])); // leading separator
        assert!(blank(&["search", ":::"])); // trailing separator
        assert!(blank(&["search", ":::", ":::", "edit"])); // doubled
    }
}
