// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end guards on the dispatching surface added with `ct-each` and the
//! suite-wide run bounds: per-item dispatch without a shell, the immutable
//! dispatch gate (and its `--mutating` extension), `--timeout` as a verdict on
//! the child-running tools, `--capture-tail` bounding of emit tokens, the
//! `--heartbeat` pulse, and the complete absence of any `--shell` mode. The
//! binaries are driven through the paths Cargo exports (`CARGO_BIN_EXE_*`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// A unique, overwrite-friendly scratch dir under `target/` (never removed).
fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/it")
        .join(tag);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("child exited via a signal")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ct_each() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ct-each"))
}

fn ct_test() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ct-test"))
}

#[test]
fn ct_each_dispatches_per_item_and_aggregates() {
    let dir = scratch("ct-each-sweep");
    let file = dir.join("haystack.txt");
    std::fs::write(&file, "alpha lives here\n").unwrap();

    let sweep = |expect: &str| -> Output {
        ct_each()
            .args(["--items", "alpha", "beta", "--expect", expect, "--quiet"])
            .args(["--emit", "{OK}/{TOTAL} -> {RESULT}"])
            .args(["--", "grep", "-q", "{ITEM}", file.to_str().unwrap()])
            .output()
            .unwrap()
    };

    // One of two items matches: --expect all fails, --expect 1 passes.
    let all = sweep("all");
    assert_eq!(code(&all), 1, "one miss must fail --expect all");
    assert!(stdout(&all).contains("1/2 -> ERROR"), "got {:?}", stdout(&all));
    assert!(
        stderr(&all).contains("'beta' -> ERROR"),
        "the red item is named: {:?}",
        stderr(&all)
    );

    let one = sweep("1");
    assert_eq!(code(&one), 0, "one success satisfies --expect 1");
    assert!(stdout(&one).contains("1/2 -> SUCCESS"));

    // JSON carries the per-item classification.
    let json = ct_each()
        .args(["--items", "alpha", "beta", "--json"])
        .args(["--", "grep", "-q", "{ITEM}", file.to_str().unwrap()])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout(&json).trim()).unwrap();
    assert_eq!(v["tool"], "ct-each");
    assert_eq!(v["verdict"], "ERROR");
    assert_eq!(v["ok"], 1);
    assert_eq!(v["items"][0]["result"], "SUCCESS");
    assert_eq!(v["items"][1]["result"], "ERROR");
}

#[test]
fn ct_each_substitutes_item_and_index_in_argv_without_a_shell() {
    // `; rm` inside an item must stay one literal argument to echo — if any
    // shell interpreted the expansion, the output shape would change.
    let out = ct_each()
        .args(["--items", "a; rm -rf /", "plain", "--quiet"])
        .args(["--emit-each", "[{INDEX}] {STDOUT}"])
        .args(["--", "echo", "got:{ITEM}"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("[1] got:a; rm -rf /"), "got {text:?}");
    assert!(text.contains("[2] got:plain"), "got {text:?}");
}

#[test]
fn ct_each_reads_items_from_stdin() {
    use std::io::Write;
    let mut child = ct_each()
        .args(["--stdin", "--quiet", "--emit", "{OK}/{TOTAL}"])
        .args(["--", "true"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"one\n\ntwo\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).contains("2/2"), "blank line skipped: {:?}", stdout(&out));
}

#[test]
fn ct_each_dry_run_previews_and_runs_nothing() {
    let dir = scratch("ct-each-dry");
    let file = dir.join("untouched.txt");
    std::fs::write(&file, "old\n").unwrap();

    let out = ct_each()
        .args(["--items", "x", "--dry-run", "--mutating"])
        .args([
            "--",
            "ct-edit",
            "--base",
            file.to_str().unwrap(),
            "--find",
            "old",
            "--replace",
            "{ITEM}",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).contains("would run: ct-edit"), "got {:?}", stdout(&out));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old\n", "dry-run must not run");
}

#[test]
fn ct_each_gate_is_fixed_with_mutating_opt_in_for_suite_tools_only() {
    // An external mutating command is refused outright...
    let denied = ct_each()
        .args(["--items", "x", "--", "rm", "{ITEM}"])
        .output()
        .unwrap();
    assert_eq!(code(&denied), 2, "rm must be refused");
    assert!(stderr(&denied).contains("not an allowed dispatch target"));
    assert!(stderr(&denied).contains("immutable"));

    // ...even with --mutating, which unlocks only the suite's own gated tools.
    let still_denied = ct_each()
        .args(["--items", "x", "--mutating", "--", "rm", "{ITEM}"])
        .output()
        .unwrap();
    assert_eq!(code(&still_denied), 2, "--mutating must not unlock externals");

    // ct-edit needs --mutating...
    let edit_denied = ct_each()
        .args(["--items", "x", "--", "ct-edit", "--find", "a", "--replace", "b"])
        .output()
        .unwrap();
    assert_eq!(code(&edit_denied), 2, "ct-edit without --mutating is refused");

    // ...and works with it (sibling resolution, real edit, per-item gating kept).
    let dir = scratch("ct-each-mutating");
    let file = dir.join("subject.txt");
    std::fs::write(&file, "alpha beta\n").unwrap();
    let edit_ok = ct_each()
        .args(["--items", "alpha", "--mutating", "--quiet"])
        .args([
            "--",
            "ct-edit",
            "--base",
            file.to_str().unwrap(),
            "--find",
            "{ITEM}",
            "--replace",
            "renamed_{ITEM}",
            "--expect",
            "=1",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&edit_ok), 0, "stderr: {:?}", stderr(&edit_ok));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "renamed_alpha beta\n"
    );
}

#[test]
fn ct_test_timeout_is_a_decisive_error_verdict() {
    let dir = scratch("ct-test-timeout");
    let file = dir.join("forever.txt");
    std::fs::write(&file, "line\n").unwrap();

    // `tail -f` never exits; the timeout must kill it promptly and classify
    // ERROR with {CODE}=timeout — even though tail printed matching output.
    let started = Instant::now();
    let out = ct_test()
        .args(["--quiet", "--timeout", "0.3", "--ok-match", "line"])
        .args(["--emit", "{RESULT} code={CODE}"])
        .args(["--cmd", "tail", "--", "-f", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "timeout must bound the run"
    );
    assert_eq!(code(&out), 1, "timeout => verdict ERROR");
    assert!(stdout(&out).contains("ERROR code=timeout"), "got {:?}", stdout(&out));
    assert!(
        stderr(&out).contains("timed out after 0.3s"),
        "reason names the bound: {:?}",
        stderr(&out)
    );
}

#[test]
fn ct_each_timeout_marks_the_item_not_the_tool() {
    let dir = scratch("ct-each-timeout");
    let file = dir.join("forever.txt");
    std::fs::write(&file, "line\n").unwrap();

    let out = ct_each()
        .args(["--items", "x", "--timeout", "0.3", "--quiet", "--json"])
        .args(["--", "tail", "-f", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "timed-out item fails --expect all");
    let v: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(v["items"][0]["code"], "timeout");
    assert_eq!(v["items"][0]["result"], "ERROR");
}

#[test]
fn ct_test_capture_tail_bounds_emit_tokens_but_not_matchers() {
    let out = ct_test()
        .args(["--quiet", "--cmd", "cat", "--capture-tail", "2"])
        .args(["--stdin", "first\nsecond\nthird\nfourth\n"])
        .args(["--ok-match", "first"]) // matcher sees the untruncated stream
        .args(["--emit", "{RESULT}|{STDOUT}"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("SUCCESS|"), "matcher saw full output: {text:?}");
    assert!(text.contains("2 earlier line(s) omitted"), "got {text:?}");
    assert!(text.contains("third\nfourth"), "got {text:?}");
    assert!(!text.contains("|first"), "token must be truncated: {text:?}");
}

#[test]
fn heartbeat_pulses_while_a_child_runs() {
    let dir = scratch("heartbeat");
    let file = dir.join("forever.txt");
    std::fs::write(&file, "line\n").unwrap();

    // Default pulse goes to stderr with the minimal [Ns] template.
    let out = ct_test()
        .args(["--quiet", "--timeout", "0.7", "--heartbeat", "0.2"])
        .args(["--cmd", "tail", "--", "-f", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(stderr(&out).contains("[0s]"), "default pulse: {:?}", stderr(&out));

    // Custom template and stdout routing, with ct-each's dynamic tokens.
    let out = ct_each()
        .args(["--items", "thing", "--timeout", "0.7", "--quiet"])
        .args(["--heartbeat", "0.2", "--heartbeat-to", "stdout"])
        .args(["--heartbeat-emit", "tick {ELAPSED}s {TOOL} {ITEM} {DONE}/{TOTAL}"])
        .args(["--", "tail", "-f", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        stdout(&out).contains("tick 0s ct-each thing 0/1"),
        "custom pulse on stdout: {:?}",
        stdout(&out)
    );
}

#[test]
fn there_is_no_shell_mode_anywhere() {
    // --shell is not a recognised option on the dispatching tools (exit 2,
    // clap usage error), and sh is not a permitted command name.
    let test_shell = ct_test().args(["--shell", "--cmd", "echo hi"]).output().unwrap();
    assert_eq!(code(&test_shell), 2, "--shell must not exist on ct-test");

    let each_shell = ct_each()
        .args(["--items", "x", "--shell", "--", "echo", "{ITEM}"])
        .output()
        .unwrap();
    assert_eq!(code(&each_shell), 2, "--shell must not exist on ct-each");

    let sh_denied = ct_test().args(["--cmd", "sh", "--", "-c", "true"]).output().unwrap();
    assert_eq!(code(&sh_denied), 2, "sh is never runnable");
    assert!(stderr(&sh_denied).contains("no shell mode"));
}

#[test]
fn self_bounded_tools_accept_timeout_and_finish_cleanly_under_it() {
    // A generous timeout on a fast run must not perturb the result (the
    // watchdog disarms on completion).
    let dir = scratch("watchdog-clean");
    std::fs::write(dir.join("a.txt"), "needle\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ct-search"))
        .args(["--base", dir.to_str().unwrap()])
        .args(["--type", "f", "--grep", "needle", "--timeout", "30", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));

    // An invalid bound is a usage error, uniformly.
    let bad = Command::new(env!("CARGO_BIN_EXE_ct-view"))
        .args(["--timeout", "0", "Cargo.toml"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert_eq!(code(&bad), 2);
    assert!(stderr(&bad).contains("positive number of seconds"));
}
