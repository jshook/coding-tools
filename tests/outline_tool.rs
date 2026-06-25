// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end guards on `ct-outline`: the count/context contract (only
//! matched entries count; ancestors render as `(context)`), the `start:?`
//! honesty rule for underivable ends, language gating at the `--base`
//! boundary, and composition through `ct-test`/`ct-each` (it is on the
//! read-only allowlist). The binaries are driven through the paths Cargo
//! exports (`CARGO_BIN_EXE_*`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn ct_outline() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ct-outline"))
}

const RUST_SAMPLE: &str = "pub struct Point {\n    x: i32,\n}\n\nimpl Point {\n    pub fn norm(&self) -> i32 {\n        self.x\n    }\n}\n";

#[test]
fn matched_only_counting_with_context_ancestors() {
    let dir = scratch("outline-context");
    std::fs::write(dir.join("point.rs"), RUST_SAMPLE).unwrap();

    let out = ct_outline()
        .args(["--base", dir.to_str().unwrap()])
        .args(["--match", "norm", "--emit", "count={COUNT} {RESULT}"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
    let text = stdout(&out);
    // The enclosing impl is visible but marked, and does not count.
    assert!(
        text.contains("impl    Point      (context)"),
        "got {text:?}"
    );
    assert!(text.contains("fn      norm"), "got {text:?}");
    assert!(
        text.contains("count=1 SUCCESS"),
        "ancestors must not count: {text:?}"
    );

    // --flat and --json carry only the matched entry, agreeing on the count.
    let flat = ct_outline()
        .args(["--base", dir.to_str().unwrap(), "--match", "norm", "--flat"])
        .output()
        .unwrap();
    let rows: Vec<String> = stdout(&flat).lines().map(String::from).collect();
    assert_eq!(rows.len(), 1, "flat carries matched entries only: {rows:?}");
    assert!(rows[0].ends_with(":6:8:fn:norm"), "got {rows:?}");

    let json = ct_outline()
        .args(["--base", dir.to_str().unwrap(), "--match", "norm", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout(&json).trim()).unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["files"][0]["entries"][0]["name"], "norm");
}

#[test]
fn anchored_match_keeps_expect_counts_predictable() {
    let dir = scratch("outline-anchored");
    std::fs::write(
        dir.join("v.rs"),
        "enum Verdict { A }\ntrait VerdictExt {}\n",
    )
    .unwrap();

    // Anchored: the bare name matches only the exact symbol.
    let exact = ct_outline()
        .args(["--base", dir.to_str().unwrap(), "--match", "Verdict"])
        .args(["--expect", "=1", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&exact), 0, "stderr: {:?}", stderr(&exact));

    // Prefix intent is said explicitly with a glob.
    let prefix = ct_outline()
        .args(["--base", dir.to_str().unwrap(), "--match", "Verdict*"])
        .args(["--expect", "=2", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&prefix), 0, "stderr: {:?}", stderr(&prefix));
}

#[test]
fn underivable_end_renders_as_question_mark_never_a_guess() {
    let dir = scratch("outline-unknown-end");
    std::fs::write(dir.join("broken.rs"), "fn broken() {\n    let x = 1;\n").unwrap();

    let out = ct_outline()
        .args(["--base", dir.to_str().unwrap(), "--flat"])
        .output()
        .unwrap();
    assert!(
        stdout(&out).contains(":1:?:fn:broken"),
        "got {:?}",
        stdout(&out)
    );

    let json = ct_outline()
        .args(["--base", dir.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout(&json).trim()).unwrap();
    assert!(v["files"][0]["entries"][0]["end"].is_null());
}

#[test]
fn unrecognised_language_errors_directly_but_skips_in_walks() {
    let dir = scratch("outline-langs");
    std::fs::write(dir.join("known.py"), "def f():\n    pass\n").unwrap();
    std::fs::write(dir.join("unknown.zig"), "fn f() void {}\n").unwrap();

    // Named directly: a clear error, exit 2.
    let direct = ct_outline()
        .args(["--base", dir.join("unknown.zig").to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&direct), 2);
    assert!(
        stderr(&direct).contains("no outline rules"),
        "got {:?}",
        stderr(&direct)
    );

    // In a walk: silently skipped; the recognised file still outlines.
    let walked = ct_outline()
        .args(["--base", dir.to_str().unwrap(), "--flat"])
        .output()
        .unwrap();
    assert_eq!(code(&walked), 0);
    assert!(
        stdout(&walked).contains(":1:2:def:f"),
        "got {:?}",
        stdout(&walked)
    );
    assert!(
        !stdout(&walked).contains("zig"),
        "unrecognised file skipped"
    );
}

#[test]
fn composes_through_ct_test_and_ct_each() {
    let dir = scratch("outline-compose");
    std::fs::write(dir.join("point.rs"), RUST_SAMPLE).unwrap();
    let base = dir.to_str().unwrap();

    // ct-outline is on the read-only allowlist, so ct-test wraps it.
    let wrapped = Command::new(env!("CARGO_BIN_EXE_ct-test"))
        .args(["--quiet", "--emit", "{RESULT}"])
        .args(["--cmd", "ct-outline", "--"])
        .args([
            "--base", base, "--match", "Point", "--kind", "struct", "--expect", "=1", "--quiet",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&wrapped), 0, "stderr: {:?}", stderr(&wrapped));
    assert!(stdout(&wrapped).contains("SUCCESS"));

    // ...and ct-each dispatches it per item without --mutating.
    let swept = Command::new(env!("CARGO_BIN_EXE_ct-each"))
        .args([
            "--items",
            "Point",
            "norm",
            "--quiet",
            "--emit",
            "{OK}/{TOTAL}",
        ])
        .args([
            "--",
            "ct-outline",
            "--base",
            base,
            "--match",
            "{ITEM}",
            "--quiet",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&swept), 0, "stderr: {:?}", stderr(&swept));
    assert!(stdout(&swept).contains("2/2"), "got {:?}", stdout(&swept));
}
