// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on the suite's *self-diagnosing refusals* and the discovery surface
//! around them — the behaviours that decide whether a failed call teaches the
//! fix or reads as a false negative.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A clean scratch directory for one test, so a stale file cannot make a
/// diagnosis pass for the wrong reason.
fn scratch(tag: &str) -> PathBuf {
    let dir = repo().join("target/test-tmp/diag").join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("child exited via a signal")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn run(exe: &str, dir: &Path, args: &[&str]) -> Output {
    Command::new(exe)
        .current_dir(dir)
        .args(args)
        .output()
        .expect("launch")
}

const CT_VIEW: &str = env!("CARGO_BIN_EXE_ct-view");
const CT_SEARCH: &str = env!("CARGO_BIN_EXE_ct-search");

// ----- flag synonyms ------------------------------------------------------------

#[test]
fn a_guessable_wrong_flag_names_the_right_one() {
    let dir = scratch("flags");
    // The two misses a field report actually hit.
    let out = run(CT_SEARCH, &dir, &["--content", "foo"]);
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("did you mean --grep?"),
        "{}",
        stderr(&out)
    );

    let out = run(CT_VIEW, &dir, &["--base", "x"]);
    assert_eq!(code(&out), 2);
    let err = stderr(&out);
    assert!(err.contains("positional"), "{err}");
    assert!(err.contains("<PATH>"), "{err}");

    // A flag with no known synonym gets clap's message and nothing invented.
    let out = run(CT_SEARCH, &dir, &["--frobnicate"]);
    assert_eq!(code(&out), 2);
    assert!(!stderr(&out).contains("did you mean"), "{}", stderr(&out));
}
