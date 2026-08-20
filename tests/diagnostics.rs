// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on the suite's *self-diagnosing refusals* and the discovery surface
//! around them — the behaviours that decide whether a failed call teaches the
//! fix or reads as a false negative.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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
const CT_EDIT: &str = env!("CARGO_BIN_EXE_ct-edit");

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

// ----- stdin: payloads ----------------------------------------------------------

#[test]
fn a_stdin_payload_carries_shell_hostile_text_verbatim() {
    let dir = scratch("stdin-payload");
    // Backticks and `$` are exactly what a POSIX shell would have eaten out of
    // an inline text: payload before ct-edit ever saw it.
    let anchor = "// see `run($cfg)` for details";
    std::fs::write(dir.join("a.rs"), format!("fn main() {{}}\n{anchor}\n")).unwrap();

    let mut child = Command::new(CT_EDIT)
        .current_dir(&dir)
        .args([
            "--base",
            ".",
            "--name",
            "*.rs",
            "--find",
            "stdin:",
            "--replace",
            "text:// gone",
            "--expect",
            "=1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch");
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(anchor.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let after = std::fs::read_to_string(dir.join("a.rs")).unwrap();
    assert!(after.contains("// gone"), "{after}");
    assert!(!after.contains("run($cfg)"), "{after}");
}

#[test]
fn the_stdin_scheme_refuses_an_argument_and_names_the_escape_hatch() {
    let dir = scratch("stdin-arg");
    std::fs::write(dir.join("a.rs"), "x\n").unwrap();
    let out = run(
        CT_EDIT,
        &dir,
        &["--base", ".", "--find", "stdin:oops", "--replace", "y"],
    );
    assert_eq!(code(&out), 2);
    let err = stderr(&out);
    assert!(err.contains("takes no argument"), "{err}");
    assert!(err.contains("text:stdin:oops"), "{err}");
}
