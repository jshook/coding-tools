// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on the three answers to "the pattern rules fought the code":
//!
//! 1. `ct-edit --find` is **literal by default** — an anchor is a piece of code,
//!    and code is made of the characters promotion would read as syntax.
//!    `--mode regex`/`glob` ask for the other readings; `--mode auto` restores
//!    the promotion rule the rest of the suite still applies to *queries*.
//! 2. `--nth`/`--at` pick **one** site when an anchor genuinely is not unique,
//!    without giving up the `--expect` gate.
//! 3. `ct-search --context` prints hits with surrounding lines in grep's own
//!    shape, so "show me the matching lines with context" — the most common
//!    read there is — no longer has to leave the suite.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CT_EDIT: &str = env!("CARGO_BIN_EXE_ct-edit");
const CT_SEARCH: &str = env!("CARGO_BIN_EXE_ct-search");

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

// ----- ct-search: hits with context ---------------------------------------------

#[test]
fn context_prints_grep_shaped_rows_and_separates_windows() {
    let dir = scratch("context");
    std::fs::write(
        dir.join("a.rs"),
        "one\ntwo\nMARK first\nfour\nfive\nsix\nseven\nMARK second\nnine\n",
    )
    .unwrap();

    let out = run(
        CT_SEARCH,
        &dir,
        &["--base", "a.rs", "--grep", "MARK", "--context", "1"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out).replace('\\', "/");
    // A hit keeps grep's `:` separators; a context line uses `-`.
    assert!(text.contains("a.rs:3:MARK first"), "{text}");
    assert!(text.contains("a.rs-2-two"), "{text}");
    assert!(text.contains("a.rs-4-four"), "{text}");
    // Non-adjacent windows are separated, exactly as grep does it.
    assert!(text.contains("\n--\n"), "{text}");
    assert!(text.contains("a.rs:8:MARK second"), "{text}");
    // Lines outside both windows are not printed.
    assert!(!text.contains("six"), "{text}");

    // Overlapping windows merge into one, with no separator inside them.
    let wide = run(
        CT_SEARCH,
        &dir,
        &["--base", "a.rs", "--grep", "MARK", "--context", "4"],
    );
    assert!(!stdout(&wide).contains("\n--\n"), "{}", stdout(&wide));
}

#[test]
fn context_implies_detail_and_zero_context_is_the_plain_form() {
    let dir = scratch("context-modes");
    std::fs::write(dir.join("a.rs"), "alpha\nMARK\nbeta\n").unwrap();

    // No --detail passed: --context turns it on.
    let ctx = run(
        CT_SEARCH,
        &dir,
        &["--base", "a.rs", "--grep", "MARK", "-C", "1"],
    );
    assert!(stdout(&ctx).contains("a.rs-1-alpha"), "{}", stdout(&ctx));

    // `--context 0` is `--detail`: hits only, no context rows.
    let zero = run(
        CT_SEARCH,
        &dir,
        &["--base", "a.rs", "--grep", "MARK", "--context", "0"],
    );
    let text = stdout(&zero);
    assert!(text.contains(":2:MARK"), "{text}");
    assert!(!text.contains("alpha"), "{text}");

    // The default list mode is untouched by any of this.
    let list = run(CT_SEARCH, &dir, &["--base", "a.rs", "--grep", "MARK"]);
    assert_eq!(stdout(&list).lines().count(), 1, "{}", stdout(&list));
}

#[test]
fn detail_json_carries_the_hit_lines() {
    let dir = scratch("context-json");
    std::fs::write(dir.join("a.rs"), "alpha\nMARK\nbeta\nMARK\n").unwrap();
    let out = run(
        CT_SEARCH,
        &dir,
        &["--base", "a.rs", "--grep", "MARK", "--detail", "--json"],
    );
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    let hits = v["hits"].as_array().expect("hits array");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0]["line"], 2);
    assert_eq!(hits[1]["line"], 4);
    assert_eq!(hits[0]["text"], "MARK");
}
