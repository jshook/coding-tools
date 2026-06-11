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
         ct help [<command>]       show this help, or a command's own --help\n  \
         ct <command> --explain    print one tool's definition (md or json)\n  \
         ct --explain [md|json]    describe the whole suite (json = a manifest of every tool)\n  \
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

/// Turn a failed hand-off into a friendly message and exit `2`.
fn launch_error(sub: &str, err: std::io::Error) -> ExitCode {
    if err.kind() == std::io::ErrorKind::NotFound {
        eprintln!("ct: unknown command '{sub}' — no 'ct-{sub}' found beside ct or on PATH");
        eprint!("\n{}", usage());
    } else {
        eprintln!("ct: could not run 'ct-{sub}': {err}");
    }
    ExitCode::from(2)
}

fn main() -> ExitCode {
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
        flag if flag.starts_with('-') => {
            eprintln!("ct: unknown option '{flag}'");
            eprint!("\n{}", usage());
            ExitCode::from(2)
        }
        sub => dispatch(sub, &args[1..]),
    }
}
