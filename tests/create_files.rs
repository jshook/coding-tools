// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end guards on `ct-new` — the suite's create. The properties that
//! matter are the ones the ad-hoc `cat > f <<EOF` does not have:
//!
//! 1. an existing target is **never** clobbered by default (and saying so is a
//!    clean negative, exit `1`, not a silent success);
//! 2. a mistyped path fails loudly instead of scattering directories;
//! 3. `--dry-run` writes nothing and previews boundedly;
//! 4. a `--script` batch is atomic — one blocked target means **zero** writes,
//!    including the targets that would have succeeded on their own;
//! 5. the body is exactly the bytes given (plus the final newline a text file
//!    should end with, unless refused).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const CT_NEW: &str = env!("CARGO_BIN_EXE_ct-new");

/// A clean scratch directory for one test.
fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/new")
        .join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(CT_NEW)
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("spawn")
}

/// Run with `text` on standard input, for the `stdin:` payload scheme.
fn run_stdin(cwd: &Path, args: &[&str], text: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(CT_NEW)
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait")
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
fn a_create_writes_exactly_the_bytes_given() {
    let dir = scratch("bytes");
    let out = run(&dir, &["a.rs", "--content", "fn a() {}", "--expect", "=1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    // The terminating newline a text file should have is added, never doubled.
    assert_eq!(
        std::fs::read_to_string(dir.join("a.rs")).unwrap(),
        "fn a() {}\n"
    );

    // ...and refusing it means the bytes are verbatim.
    let out = run(
        &dir,
        &["b.rs", "--content", "fn b() {}", "--no-final-newline"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(dir.join("b.rs")).unwrap(),
        "fn b() {}"
    );

    // A body already ending in a newline is untouched.
    let out = run_stdin(&dir, &["c.rs", "--content", "stdin:"], "fn c() {}\n");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(dir.join("c.rs")).unwrap(),
        "fn c() {}\n"
    );

    // --empty is the explicit zero-byte case; a missing body is a usage error.
    assert_eq!(code(&run(&dir, &["d.keep", "--empty"])), 0);
    assert_eq!(std::fs::read_to_string(dir.join("d.keep")).unwrap(), "");
    let out = run(&dir, &["e.rs"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("--content"), "{}", stderr(&out));
}

#[test]
fn an_existing_file_is_never_clobbered_by_default() {
    let dir = scratch("clobber");
    std::fs::write(dir.join("a.rs"), "original\n").unwrap();

    // Default policy: nothing written, a clean negative, and the reason names
    // both ways out.
    let out = run(&dir, &["a.rs", "--content", "replacement"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(dir.join("a.rs")).unwrap(),
        "original\n"
    );
    assert!(stdout(&out).contains("blocked"), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("--on-exists skip") && stdout(&out).contains("overwrite"),
        "the refusal must name both ways out: {}",
        stdout(&out)
    );

    // Even `--expect none` — which the count alone would satisfy — cannot turn
    // a refusal into a success.
    let out = run(&dir, &["a.rs", "--content", "x", "--expect", "none"]);
    assert_eq!(code(&out), 1, "{}", stdout(&out));

    // skip: idempotent, and the file is still untouched.
    let out = run(&dir, &["a.rs", "--content", "x", "--on-exists", "skip"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(dir.join("a.rs")).unwrap(),
        "original\n"
    );
    assert!(
        stdout(&out).contains("nothing to write"),
        "{}",
        stdout(&out)
    );

    // overwrite: the only way the bytes change.
    let out = run(
        &dir,
        &[
            "a.rs",
            "--content",
            "replacement",
            "--on-exists",
            "overwrite",
        ],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(dir.join("a.rs")).unwrap(),
        "replacement\n"
    );
}

#[test]
fn a_missing_parent_directory_is_an_error_until_parents_is_given() {
    let dir = scratch("parents");
    let out = run(&dir, &["src/deep/a.rs", "--content", "x"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("--parents"), "{}", stderr(&out));
    assert!(
        !dir.join("src").exists(),
        "a refused create creates nothing"
    );

    let out = run(&dir, &["src/deep/a.rs", "--content", "x", "--parents"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(dir.join("src/deep/a.rs").is_file());

    // A path that is already a directory is a mistake, not an outcome.
    let out = run(&dir, &["src", "--content", "x"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("directory"), "{}", stderr(&out));
}

#[test]
fn dry_run_writes_nothing_and_previews_boundedly() {
    let dir = scratch("dry-run");
    let body: String = (1..=30).map(|n| format!("line{n}\n")).collect();
    let out = run_stdin(
        &dir,
        &["big.txt", "--content", "stdin:", "--dry-run"],
        &body,
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        !dir.join("big.txt").exists(),
        "--dry-run must write nothing"
    );

    let text = stdout(&out);
    assert!(text.contains("big.txt:1:+ line1"), "{text}");
    assert!(text.contains("dry-run, not written"), "{text}");
    // Bounded: the preview stops well short of the body and says how much it held back.
    assert!(
        !text.contains("line30"),
        "the preview must not dump the file: {text}"
    );
    assert!(text.contains("more line(s)"), "{text}");
}

#[test]
fn a_script_batch_is_atomic() {
    let dir = scratch("script");
    std::fs::write(dir.join("taken.rs"), "original\n").unwrap();
    let script = "#% new file=fresh.rs\n#% content\nfn fresh() {}\n#% end\n\
                  #% new file=taken.rs\n#% content\nfn taken() {}\n#% end\n";
    std::fs::write(dir.join("batch.ctb"), script).unwrap();

    // One blocked target means zero writes — including the one that would have
    // succeeded on its own.
    let out = run(&dir, &["--script", "batch.ctb"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(
        !dir.join("fresh.rs").exists(),
        "a blocked batch must not half-apply: {}",
        stdout(&out)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("taken.rs")).unwrap(),
        "original\n"
    );

    // With the collision accepted, the whole batch lands.
    let out = run(
        &dir,
        &["--script", "batch.ctb", "--on-exists", "skip", "--json"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(dir.join("fresh.rs")).unwrap(),
        "fn fresh() {}\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("taken.rs")).unwrap(),
        "original\n",
        "a skipped target is never rewritten"
    );
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    assert_eq!(v["ensured"], 2);
    assert_eq!(v["created"], 1);
    assert_eq!(v["skipped"], 1);
    assert_eq!(v["script"], "batch.ctb");

    // A per-item policy overrides the invocation's.
    let script = "#% new file=taken.rs on-exists=overwrite\n#% content\nreplaced\n#% end\n";
    std::fs::write(dir.join("one.ctb"), script).unwrap();
    let out = run(&dir, &["--script", "one.ctb"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(dir.join("taken.rs")).unwrap(),
        "replaced\n"
    );
}

#[test]
fn a_script_naming_one_target_twice_is_refused_before_anything_is_written() {
    let dir = scratch("script-dupe");
    let script = "#% new file=a.rs\n#% content\nfirst\n#% end\n\
                  #% new file=a.rs\n#% content\nsecond\n#% end\n";
    std::fs::write(dir.join("dupe.ctb"), script).unwrap();
    let out = run(&dir, &["--script", "dupe.ctb"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("both target"), "{}", stderr(&out));
    assert!(!dir.join("a.rs").exists());

    // A bad item names its own script line.
    std::fs::write(dir.join("bad.ctb"), "#% new file=a.rs mode=regex\n#% end\n").unwrap();
    let out = run(&dir, &["--script", "bad.ctb"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("script line 1"), "{}", stderr(&out));
}

#[test]
fn the_json_result_reports_every_action() {
    let dir = scratch("json");
    std::fs::write(dir.join("there.txt"), "x\n").unwrap();

    let out = run(&dir, &["there.txt", "--content", "y", "--json"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    assert_eq!(v["tool"], "ct-new");
    assert_eq!(v["verdict"], "ERROR");
    assert_eq!(v["applied"], false);
    assert_eq!(v["blocked"], 1);
    assert_eq!(v["ensured"], 0);
    assert_eq!(v["files"][0]["action"], "blocked");

    let out = run(&dir, &["made.txt", "--content", "hello", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    assert_eq!(v["files"][0]["action"], "created");
    assert_eq!(v["files"][0]["lines"], 1);
    assert_eq!(v["files"][0]["bytes"], 6);
}
