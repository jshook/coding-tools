// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end guards on the framed-verdict surface that unifies the suite:
//! `ct-search` posing a search as a pass/fail test (`--expect`/`--emit`), and
//! `ct-test`'s read-only command allow-gate. The binaries are driven through the
//! paths Cargo exports (`CARGO_BIN_EXE_*`) — no classpath/PATH games.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

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

#[test]
fn ct_search_expect_none_inverts_the_verdict() {
    let dir = scratch("ct-search-expect");
    std::fs::write(dir.join("hit.txt"), "a NEEDLE lives here\n").unwrap();
    std::fs::write(dir.join("clean.txt"), "nothing to see\n").unwrap();

    let run = |grep: &str, expect: &str| -> Output {
        Command::new(env!("CARGO_BIN_EXE_ct-search"))
            .args(["--base", dir.to_str().unwrap()])
            .args(["--type", "f", "--grep", grep, "--expect", expect, "--quiet"])
            .output()
            .unwrap()
    };

    // `none` fails when the pattern IS present...
    assert_eq!(code(&run("NEEDLE", "none")), 1, "found one => ERROR");
    // ...and passes when it is absent.
    assert_eq!(
        code(&run("ABSENT_TOKEN", "none")),
        0,
        "found none => SUCCESS"
    );

    // Default `any` is unchanged: matched => 0.
    let emit = Command::new(env!("CARGO_BIN_EXE_ct-search"))
        .args(["--base", dir.to_str().unwrap()])
        .args(["--type", "f", "--grep", "NEEDLE"])
        .args(["--emit", "{RESULT} {COUNT}"])
        .output()
        .unwrap();
    assert_eq!(code(&emit), 0);
    assert!(
        stdout(&emit).contains("SUCCESS 1"),
        "got: {:?}",
        stdout(&emit)
    );
}

#[test]
fn ct_patch_yaml_set_and_delete_preserve_comments() {
    let dir = scratch("ct-patch-yaml");
    let file = dir.join("cfg.yaml");
    let original =
        "# top comment\nserver:\n  host: localhost   # inline\n  port: 8080\n  debug: true\n";
    std::fs::write(&file, original).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_ct-patch"))
        .args([
            "--base",
            file.to_str().unwrap(),
            "--set",
            ".server.port=9090",
            "--delete",
            ".server.debug",
            "--expect",
            "=2",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(
        after.contains("# top comment"),
        "leading comment kept: {after:?}"
    );
    assert!(
        after.contains("port: 9090"),
        "number set unquoted: {after:?}"
    );
    assert!(after.contains("# inline"), "inline comment kept: {after:?}");
    assert!(!after.contains("debug:"), "debug deleted: {after:?}");

    // --add is JSON-family only for now: it must error on YAML, not corrupt.
    let add = Command::new(env!("CARGO_BIN_EXE_ct-patch"))
        .args(["--base", file.to_str().unwrap(), "--add", ".server.tags=x"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 2, "YAML --add should error");
    assert!(stderr(&add).contains("not yet supported for YAML"));
}

#[test]
fn ct_patch_preserves_comments_and_is_expect_gated() {
    let dir = scratch("ct-patch");
    let file = dir.join("config.jsonc");
    let original = "{\n  // service config\n  \"port\": 8080,\n  \"debug\": true\n}\n";
    std::fs::write(&file, original).unwrap();

    let patch = |args: &[&str]| -> Output {
        let mut a = vec!["--base", file.to_str().unwrap()];
        a.extend_from_slice(args);
        Command::new(env!("CARGO_BIN_EXE_ct-patch"))
            .args(a)
            .output()
            .unwrap()
    };

    // Wrong --expect (one change, expecting two) => ERROR, nothing written.
    let denied = patch(&["--set", ".port=9090", "--expect", "=2"]);
    assert_eq!(code(&denied), 1, "expect mismatch => ERROR");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        original,
        "ERROR must not write"
    );

    // Dry-run never writes.
    let dry = patch(&["--set", ".port=9090", "--dry-run"]);
    assert_eq!(code(&dry), 0);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        original,
        "dry-run must not write"
    );

    // Apply: value changes, the comment and layout survive.
    let ok = patch(&["--set", ".port=9090"]);
    assert_eq!(code(&ok), 0);
    let after = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        after, "{\n  // service config\n  \"port\": 9090,\n  \"debug\": true\n}\n",
        "only the value should change; comment preserved"
    );
}

#[test]
fn ct_test_wraps_read_only_suite_tools_by_sibling_resolution() {
    let dir = scratch("ct-compose");
    std::fs::write(dir.join("big.rs"), "x\n".repeat(50)).unwrap();
    std::fs::write(dir.join("small.rs"), "x\n".repeat(3)).unwrap();

    // ct-test wraps ct-tree (a sibling binary, not on PATH) as a condition.
    let wrap = |args: &[&str]| -> Output {
        let mut a = vec!["--quiet", "--emit", "{RESULT}", "--cmd", "ct-tree", "--"];
        a.extend_from_slice(args);
        Command::new(env!("CARGO_BIN_EXE_ct-test"))
            .args(a)
            .output()
            .unwrap()
    };

    // A file over 40 lines exists -> ct-tree exits 0 -> ct-test SUCCESS.
    let yes = wrap(&[
        "--base",
        dir.to_str().unwrap(),
        "--ext",
        "rs",
        "--min-lines",
        "40",
        "--flat",
    ]);
    assert_eq!(
        code(&yes),
        0,
        "sibling ct-tree should run; got {:?}",
        stderr(&yes)
    );
    assert!(stdout(&yes).contains("SUCCESS"));

    // None over 500 lines -> ct-tree exits 1 -> ct-test ERROR.
    let no = wrap(&[
        "--base",
        dir.to_str().unwrap(),
        "--ext",
        "rs",
        "--min-lines",
        "500",
        "--flat",
    ]);
    assert_eq!(code(&no), 1);
    assert!(stdout(&no).contains("ERROR"));
}

#[test]
fn ct_test_allow_gate_is_fixed_and_immutable() {
    let ct_test = || Command::new(env!("CARGO_BIN_EXE_ct-test"));

    // A command not on the fixed list is refused (exit 2), nothing runs, and the
    // message states the list is immutable (no opt-in path).
    let denied = ct_test()
        .args(["--quiet", "--cmd", "seq", "--", "1", "2"])
        .output()
        .unwrap();
    assert_eq!(code(&denied), 2, "non-allowlisted command must be refused");
    assert!(
        stderr(&denied).contains("not on the allowlist"),
        "deny message missing; got: {:?}",
        stderr(&denied)
    );
    assert!(
        stderr(&denied).contains("immutable"),
        "deny message must state the list is fixed; got: {:?}",
        stderr(&denied)
    );

    // There is no `--allow` flag any more: passing it is a usage error.
    let no_allow = ct_test().args(["--allow", "seq"]).output().unwrap();
    assert_eq!(code(&no_allow), 2, "--allow must no longer exist");

    // A built-in read-only command runs (a suite ct-* tool, allowed on every OS).
    let dir = scratch("ct-test-gate");
    let ok = common::exit_ok(&dir);
    let builtin = ct_test()
        .args(["--quiet", "--cmd", &ok[0], "--emit", "{RESULT}"])
        .arg("--")
        .args(&ok[1..])
        .output()
        .unwrap();
    assert_eq!(code(&builtin), 0, "built-in command should run");
    assert!(stdout(&builtin).contains("SUCCESS"));
}

#[test]
fn ct_test_diagnoses_and_lets_caller_set_inconclusive_outcome() {
    // ct-view writes the file's contents to stdout and exits 0; --ok-match-stderr
    // searches only stderr, so the required success proof is "not found" — an
    // inconclusive run. (ct-view stands in for a plain echo, cross-platform.)
    let dir = scratch("ct-test-inconclusive");
    let hi = dir.join("hi.txt");
    std::fs::write(&hi, "hi\n").unwrap();
    let hi = hi.to_string_lossy().into_owned();
    let run = |extra: &[&str]| -> Output {
        let mut args = vec![
            "--ok-match-stderr",
            "hi",
            "--cmd",
            "ct-view",
            "--emit",
            "{RESULT}",
        ];
        args.extend_from_slice(extra);
        args.push("--");
        args.push(hi.as_str());
        Command::new(env!("CARGO_BIN_EXE_ct-test"))
            .args(args)
            .output()
            .unwrap()
    };

    // Default: a required --ok-match that did not appear is ERROR, even on exit 0,
    // and the reason names the stream so the mismatch is diagnosable.
    let default = run(&[]);
    assert_eq!(
        code(&default),
        1,
        "absent required ok-match => ERROR on exit 0"
    );
    assert!(stdout(&default).contains("ERROR"));
    let why = stderr(&default);
    assert!(
        why.contains("not found in stderr"),
        "reason names the stream: {why:?}"
    );
    assert!(why.contains("exit=0"), "reason includes exit code: {why:?}");

    // The caller can override the inconclusive outcome.
    assert_eq!(
        code(&run(&["--otherwise", "exit"])),
        0,
        "exit policy => SUCCESS on exit 0"
    );
    assert_eq!(code(&run(&["--otherwise", "success"])), 0, "success policy");
    assert_eq!(code(&run(&["--otherwise", "error"])), 1, "error policy");

    // A failure signal stays decisive regardless of --otherwise.
    let bad = dir.join("bad.txt");
    std::fs::write(&bad, "BAD news\n").unwrap();
    let err = Command::new(env!("CARGO_BIN_EXE_ct-test"))
        .args([
            "--err-match",
            "BAD",
            "--otherwise",
            "success",
            "--cmd",
            "ct-view",
            "--emit",
            "{RESULT}",
            "--",
            bad.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        code(&err),
        1,
        "err-match must not be overridden by --otherwise"
    );
}
