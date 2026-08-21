// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on `ct-edit`'s pattern rules, where the suite's promotion rule used to
//! fight the code it was pointed at.
//!
//! `--find` is **literal by default** — an anchor is a piece of code, and code
//! is made of the characters promotion would read as syntax. `--mode
//! regex`/`glob` ask for the other readings; `--mode auto` restores the
//! promotion rule the rest of the suite still applies to *queries*.

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
