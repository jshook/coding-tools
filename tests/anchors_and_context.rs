// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on `ct-edit`'s pattern rules, where the suite's promotion rule used to
//! fight the code it was pointed at.
//!
//! 1. `--find` is **literal by default** — an anchor is a piece of code, and
//!    code is made of the characters promotion would read as syntax. `--mode
//!    regex`/`glob` ask for the other readings; `--mode auto` restores the
//!    promotion rule the rest of the suite still applies to *queries*.
//! 2. `--nth`/`--at` pick **one** site when an anchor genuinely is not unique,
//!    without giving up the `--expect` gate.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CT_EDIT: &str = env!("CARGO_BIN_EXE_ct-edit");

/// A clean scratch directory for one test.
fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/anchors")
        .join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(bin: &str, cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("spawn")
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

/// Three identical call sites plus one metacharacter-heavy anchor.
fn sample(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::write(
        dir.join("a.rs"),
        "fn main() {\n    reg::<Foo[T]>();\n    let v = compute(1);\n    let v = compute(2);\n    \
         let v = compute(3);\n}\n",
    )
    .unwrap();
    dir
}

// ----- ct-edit: the anchor is code ----------------------------------------------

#[test]
fn mode_regex_still_promotes_and_expands_captures() {
    let dir = sample("regex-mode");
    let out = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--mode",
            "regex",
            "--find",
            r"compute\((\d)\)",
            "--replace",
            "calc(${1}0)",
            "--expect",
            "=3",
        ],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let after = std::fs::read_to_string(dir.join("a.rs")).unwrap();
    assert!(
        after.contains("calc(10)") && after.contains("calc(30)"),
        "{after}"
    );
}

#[test]
fn literal_default_does_not_expand_dollar_in_the_replacement() {
    let dir = scratch("literal-replace");
    std::fs::write(dir.join("a.rs"), "const KEY: &str = \"k\";\n").unwrap();
    let out = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--find",
            "KEY",
            "--replace",
            "$1_KEY",
            "--expect",
            "=1",
        ],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    // A literal find takes the replacement literally: `$1` is two characters.
    let after = std::fs::read_to_string(dir.join("a.rs")).unwrap();
    assert!(after.starts_with("const $1_KEY:"), "{after}");
}

// ----- ct-edit: picking one site ------------------------------------------------

#[test]
fn nth_and_at_each_take_exactly_one_site() {
    let dir = sample("pick");
    let nth = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--find",
            "let v = compute",
            "--replace",
            "let w = compute",
            "--nth",
            "2",
        ],
    );
    assert_eq!(code(&nth), 0, "{}", stderr(&nth));
    let after = std::fs::read_to_string(dir.join("a.rs")).unwrap();
    assert!(after.contains("let w = compute(2);"), "{after}");
    assert!(after.contains("let v = compute(1);"), "{after}");
    assert!(after.contains("let v = compute(3);"), "{after}");

    // `--at` names the site the way `ct search --detail` reports it.
    let at = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--find",
            "let v = compute",
            "--replace",
            "let u = compute",
            "--at",
            "a.rs:5",
        ],
    );
    assert_eq!(code(&at), 0, "{}", stderr(&at));
    let after = std::fs::read_to_string(dir.join("a.rs")).unwrap();
    assert!(after.contains("let u = compute(3);"), "{after}");
}

#[test]
fn a_pick_carries_the_strict_expectation_by_default() {
    let dir = sample("pick-expect");
    // Three matches, no pick, no --expect: the default `any` passes and all
    // three change. The pick's implied `=1` is what makes it a claim.
    let all = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--find",
            "let v = compute",
            "--replace",
            "X",
            "--dry-run",
        ],
    );
    assert!(
        stdout(&all).contains("3 replacement(s)"),
        "{}",
        stdout(&all)
    );

    let picked = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--find",
            "let v = compute",
            "--replace",
            "X",
            "--nth",
            "3",
            "--dry-run",
        ],
    );
    assert!(
        stdout(&picked).contains("1 replacement(s)"),
        "{}",
        stdout(&picked)
    );
    assert_eq!(code(&picked), 0, "{}", stderr(&picked));
}

#[test]
fn a_pick_that_lands_on_nothing_says_where_the_anchor_did_match() {
    let dir = sample("pick-miss");
    let over = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--find",
            "let v = compute",
            "--replace",
            "X",
            "--nth",
            "9",
        ],
    );
    assert_eq!(code(&over), 1, "a pick that took nothing is an ERROR");
    let err = stderr(&over);
    assert!(err.contains("--nth 9"), "names the pick: {err}");
    assert!(
        err.contains("holds 3 match(es)"),
        "counts the anchor: {err}"
    );
    assert!(err.contains("a.rs:3"), "positions them: {err}");
    // The anchor was fine, so the pattern diagnosis must NOT fire.
    assert!(!err.contains("nearest miss"), "{err}");
    // And nothing was written.
    let after = std::fs::read_to_string(dir.join("a.rs")).unwrap();
    assert!(after.contains("let v = compute(1);"), "{after}");

    let wrong_line = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--find",
            "let v = compute",
            "--replace",
            "X",
            "--at",
            "a.rs:99",
        ],
    );
    assert_eq!(code(&wrong_line), 1);
    assert!(
        stderr(&wrong_line).contains("--at a.rs:99 matched no site"),
        "{}",
        stderr(&wrong_line)
    );
}

#[test]
fn nth_counts_across_the_whole_selection_not_per_file() {
    let dir = scratch("pick-across");
    std::fs::write(dir.join("a.rs"), "hit\n").unwrap();
    std::fs::write(dir.join("b.rs"), "hit\n").unwrap();
    let out = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--name",
            "*.rs",
            "--find",
            "hit",
            "--replace",
            "done",
            "--nth",
            "2",
        ],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let a = std::fs::read_to_string(dir.join("a.rs")).unwrap();
    let b = std::fs::read_to_string(dir.join("b.rs")).unwrap();
    // Exactly one of the two files changed — the second in walk order.
    assert_ne!(a.trim() == "done", b.trim() == "done", "a={a:?} b={b:?}");
}

#[test]
fn a_pick_is_a_single_edit_concept_and_a_script_refuses_it() {
    let dir = sample("pick-script");
    for flag in [["--nth", "1"], ["--at", "a.rs:3"]] {
        let out = run(CT_EDIT, &dir, &["--script", "batch.ctb", flag[0], flag[1]]);
        assert_eq!(code(&out), 2, "{}", stderr(&out));
        assert!(
            stderr(&out).contains("cannot be used with"),
            "{}",
            stderr(&out)
        );
    }
    // The two picks are alternatives, not a pair.
    let both = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--find",
            "x",
            "--replace",
            "y",
            "--nth",
            "1",
            "--at",
            "a.rs:3",
        ],
    );
    assert_eq!(code(&both), 2, "{}", stderr(&both));
}
