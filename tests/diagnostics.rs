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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
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

// ----- ct-edit: a single-line find that matched nothing -------------------------

#[test]
fn single_line_find_promoted_past_its_own_text_is_diagnosed() {
    let dir = scratch("promoted");
    // `[T]` makes this a *valid* regex whose character class matches `T`, so the
    // promoted pattern does not match the text it was copied from. This is the
    // reported failure, and it used to produce a bare ERROR with no hint.
    std::fs::write(dir.join("a.rs"), "fn main() {\n    reg::<Foo[T]>();\n}\n").unwrap();

    let out = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--name",
            "*.rs",
            "--find",
            "reg::<Foo[T]>();",
            "--replace",
            "",
            "--expect",
            "1",
        ],
    );
    assert_eq!(code(&out), 1, "a zero-match edit is a verdict ERROR");
    let err = stderr(&out);
    assert!(err.contains("nearest miss: "), "positioned: {err}");
    assert!(err.contains("a.rs:2"), "names the line: {err}");
    assert!(err.contains("--mode literal"), "names the fix: {err}");
    assert!(err.contains("stdin:"), "names the scoped fix too: {err}");
    // And nothing was written.
    let after = std::fs::read_to_string(dir.join("a.rs")).unwrap();
    assert!(after.contains("reg::<Foo[T]>();"));

    // Taking the *scoped* fix the note names first makes the same edit succeed.
    // (`--mode literal` would also pin `--name '*.rs'`, which is exactly why the
    // note offers `file:`/`stdin:` ahead of it.)
    let anchor = dir.join("anchor.txt");
    std::fs::write(&anchor, "reg::<Foo[T]>();").unwrap();
    let fixed = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--name",
            "*.rs",
            "--find",
            &format!("file:{}", anchor.display()),
            "--replace",
            "text:",
            "--expect",
            "1",
        ],
    );
    assert_eq!(code(&fixed), 0, "{}", stderr(&fixed));
}

#[test]
fn mode_literal_silently_narrowing_the_selection_is_called_out() {
    let dir = scratch("mode-name");
    std::fs::write(dir.join("a.rs"), "let x = 1;\n").unwrap();
    // `--mode` pins EVERY pattern in the call, so `--name '*.rs'` stops being a
    // glob and matches nothing. That reads as "the anchor is wrong" unless the
    // empty-selection note says what actually happened.
    let out = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--name",
            "*.rs",
            "--mode",
            "literal",
            "--find",
            "let x = 1;",
            "--replace",
            "y",
            "--expect",
            "1",
        ],
    );
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.contains("read 0 file(s)"), "{err}");
    assert!(err.contains("--mode pins every pattern"), "{err}");
    assert!(
        err.contains("matched as literal rather than as a glob"),
        "{err}"
    );
}

#[test]
fn single_line_find_reports_whitespace_drift_and_an_empty_selection() {
    let dir = scratch("drift");
    std::fs::write(dir.join("b.rs"), "let y = 2;\n").unwrap();

    // Leading/trailing whitespace on the anchor: both rows are quoted, because
    // the difference is invisible otherwise.
    let drift = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--name",
            "b.rs",
            "--find",
            "  let y = 2;  ",
            "--replace",
            "z",
            "--expect",
            "1",
        ],
    );
    let err = stderr(&drift);
    assert!(err.contains("surrounding whitespace"), "{err}");
    assert!(
        err.contains("'  let y = 2;  '"),
        "quotes the pattern: {err}"
    );
    assert!(err.contains("'let y = 2;'"), "quotes the line: {err}");

    // Nothing close at all: report the scan's shape, not a made-up cause.
    let cold = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--name",
            "*.rs",
            "--find",
            "zzz_absent_marker",
            "--replace",
            "x",
            "--expect",
            "1",
        ],
    );
    let err = stderr(&cold);
    assert!(err.contains("scanned file(s)"), "{err}");
    assert!(!err.contains("nearest miss"), "no invented miss: {err}");

    // Nothing read at all: the fault is the selection, and saying so stops the
    // caller re-checking an anchor that was never compared to anything.
    let empty = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--name",
            "*.zzz",
            "--find",
            "x",
            "--replace",
            "y",
            "--expect",
            "1",
        ],
    );
    let err = stderr(&empty);
    assert!(err.contains("read 0 file(s)"), "{err}");
    assert!(err.contains("--name '*.zzz'"), "names the filter: {err}");
}

#[test]
fn the_zero_match_diagnosis_is_also_structured_json() {
    let dir = scratch("miss-json");
    std::fs::write(dir.join("a.rs"), "  reg::<Foo[T]>();\n").unwrap();
    let out = run(
        CT_EDIT,
        &dir,
        &[
            "--base",
            ".",
            "--name",
            "*.rs",
            "--find",
            "reg::<Foo[T]>();",
            "--replace",
            "",
            "--expect",
            "1",
            "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    assert_eq!(v["verdict"], "ERROR");
    assert_eq!(v["files_scanned"], 1);
    assert_eq!(v["nearest_miss"]["kind"], "line");
    assert_eq!(v["nearest_miss"]["cause"], "promoted");
    assert_eq!(v["nearest_miss"]["promoted_as"], "regex");
    assert_eq!(v["nearest_miss"]["literal_hits"], 1);
    assert_eq!(v["nearest_miss"]["line"], 1);
    // The text path stays silent under --json: one machine-readable answer.
    assert!(!stderr(&out).contains("nearest miss"));
}

// ----- ct-view: repeatable --match ----------------------------------------------

#[test]
fn several_match_patterns_merge_into_one_window_set() {
    let dir = scratch("multimatch");
    let file = dir.join("src.txt");
    std::fs::write(&file, "alpha\nfiller\nbeta\nfiller\nfiller\ngamma\n").unwrap();
    let path = file.to_string_lossy().into_owned();

    let out = run(
        CT_VIEW,
        &dir,
        &[&path, "--match", "alpha", "--match", "gamma", "-C", "0"],
    );
    assert_eq!(code(&out), 0);
    let text = stdout(&out);
    assert!(text.contains("alpha") && text.contains("gamma"), "{text}");
    assert!(!text.contains("beta"), "only the matched windows: {text}");

    // A pattern that contributed nothing is named, so a partial answer is not
    // mistaken for a complete one — but the union still succeeds.
    let out = run(
        CT_VIEW,
        &dir,
        &[&path, "--match", "alpha", "--match", "zzz", "-C", "0"],
    );
    assert_eq!(code(&out), 0);
    assert!(
        stderr(&out).contains("--match 'zzz' matched no lines"),
        "{}",
        stderr(&out)
    );

    // Exit follows the union: 1 only when nothing matched at all.
    let out = run(CT_VIEW, &dir, &[&path, "--match", "zzz", "--match", "qqq"]);
    assert_eq!(code(&out), 1);

    // --json reports each pattern's contribution.
    let out = run(
        CT_VIEW,
        &dir,
        &[
            &path, "--match", "alpha", "--match", "zzz", "-C", "0", "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    let matches = v["matches"].as_array().expect("matches array");
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0]["hits"], 1);
    assert_eq!(matches[1]["hits"], 0);
}
