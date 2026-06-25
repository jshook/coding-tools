// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook
#![allow(dead_code)]

//! Cross-platform dispatched-command helpers shared by the dispatch tests.
//!
//! The suite's tools (`ct-test`/`ct-each`/`ct-await`) launch programs directly
//! (no shell), and the read-only allowlist is platform-specific, so a test must
//! pick a *portable* command for each primitive it needs. These helpers prefer
//! the suite's own `ct-*` tools — which resolve and gate on every OS — and fall
//! back to the host's native utility only where a raw filter is essential
//! (stdin echo). Each returns a full argv (`[program, args…]`) ready to append
//! after a `--` (for `ct-each`/`ct-await`) or split into `--cmd` + trailing args
//! (for `ct-test`).

use std::path::Path;

/// Print a file's contents to stdout (matcher/emit fodder), via the read-only
/// `ct-view`. The line-number gutter doesn't affect substring matchers.
/// Cross-platform stand-in for `cat FILE`.
pub fn print_file(path: &Path) -> Vec<String> {
    vec!["ct-view".into(), path.to_string_lossy().into_owned()]
}

/// A command that exits `0` everywhere (views a file we ensure exists).
/// Cross-platform stand-in for `true`.
pub fn exit_ok(dir: &Path) -> Vec<String> {
    let f = dir.join(".ok");
    std::fs::write(&f, "ok\n").unwrap();
    vec!["ct-view".into(), f.to_string_lossy().into_owned()]
}

/// A command that exits non-zero everywhere (`ct-search` finds nothing → `1`).
/// Cross-platform stand-in for `false`.
pub fn exit_err(dir: &Path) -> Vec<String> {
    vec![
        "ct-search".into(),
        "--base".into(),
        dir.to_string_lossy().into_owned(),
        "--name".into(),
        "zzz-absent-marker".into(),
        "--quiet".into(),
    ]
}

/// Search `file` for the literal `pattern` (exit `0` if present, `1` if not),
/// via the suite's own `ct-search`. Cross-platform stand-in for `grep -q PAT FILE`.
pub fn grep(file: &Path, pattern: &str) -> Vec<String> {
    vec![
        "ct-search".into(),
        "--base".into(),
        file.to_string_lossy().into_owned(),
        "--grep".into(),
        pattern.into(),
        "--mode".into(),
        "literal".into(),
        "--quiet".into(),
    ]
}

/// A long-running command that blocks until its caller's `--timeout` fires — a
/// bounded `ct-await` poll of a never-true probe. Suite-native and
/// cross-platform stand-in for `tail -f FILE`.
pub fn block(dir: &Path) -> Vec<String> {
    vec![
        "ct-await".into(),
        "--quiet".into(),
        "--timeout".into(),
        "30".into(),
        "--every".into(),
        "5".into(),
        "--".into(),
        "ct-search".into(),
        "--base".into(),
        dir.to_string_lossy().into_owned(),
        "--name".into(),
        "zzz-never-matches".into(),
        "--quiet".into(),
    ]
}

/// Echo stdin to stdout — the one irreducible per-OS fork, since no `ct-*` tool
/// reads stdin: `cat` on Unix, `findstr /R "."` (matches every non-empty line)
/// on Windows. Cross-platform stand-in for `cat -`.
pub fn cat_stdin() -> Vec<String> {
    #[cfg(not(windows))]
    {
        vec!["cat".into()]
    }
    #[cfg(windows)]
    {
        vec!["findstr".into(), "/R".into(), ".".into()]
    }
}
