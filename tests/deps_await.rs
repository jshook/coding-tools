// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end guards on `ct-deps` (real `cargo metadata` over this very
//! workspace: deny/forbid evidence paths, duplicates, defective assertions)
//! and `ct-await` (success on a condition that becomes true, immediate
//! abort-on, hard timeout, and the immutable probe gate). The binaries are
//! driven through the paths Cargo exports (`CARGO_BIN_EXE_*`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo().join("target/test-tmp/it").join(tag);
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

fn ct_deps() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ct-deps"));
    c.current_dir(repo());
    c
}

fn ct_await(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ct-await"));
    c.current_dir(dir);
    c
}

#[test]
fn deps_deny_reports_an_evidence_path_for_a_real_dependency() {
    // clap IS a dependency of this crate: the assertion must fail with proof.
    let out = ct_deps().args(["--deny", "clap"]).output().unwrap();
    assert_eq!(code(&out), 1, "stderr: {:?}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("deny: clap: coding-tools v"), "evidence path: {text:?}");
    assert!(text.contains("-> clap v"), "path reaches the crate: {text:?}");

    // An absent crate holds, quietly composable.
    let out = ct_deps()
        .args(["--deny", "openssl", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
}

#[test]
fn deps_forbid_and_duplicates_and_defective_assertions() {
    // A real reachable pair: this crate depends on walkdir.
    let out = ct_deps()
        .args(["--forbid", "coding-tools=>walkdir"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 1);
    assert!(stdout(&out).contains("forbid: coding-tools=>walkdir:"));

    // No path in the reverse direction.
    let out = ct_deps()
        .args(["--forbid", "walkdir=>coding-tools", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));

    // A source package that does not exist is a defective assertion (exit 2).
    let out = ct_deps().args(["--forbid", "ghost=>walkdir"]).output().unwrap();
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("no package named 'ghost'"));

    // Duplicates: this repo's own invariant says there are none.
    let out = ct_deps()
        .args(["--duplicates", "--emit", "{RESULT} {COUNT}"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
    assert!(stdout(&out).contains("SUCCESS 0"));

    // No assertions is a usage error.
    let out = ct_deps().output().unwrap();
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("nothing to assert"));
}

#[test]
fn await_succeeds_when_the_condition_becomes_true() {
    let dir = scratch("await-appears");
    let marker = dir.join("build.log");
    let _ = std::fs::remove_file(&marker);

    // The "external work": a thread that produces the marker shortly.
    let marker2 = marker.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        std::fs::write(&marker2, "BUILD SUCCESS\n").unwrap();
    });

    // The probe errors while the file is missing (the normal "not yet" case),
    // then succeeds once it appears.
    let out = ct_await(&dir)
        .args(["--every", "0.1", "--timeout", "10", "--quiet"])
        .args(["--emit", "{RESULT} after {TICKS} tick(s)"])
        .args(["--", "ct-search", "--base", "build.log", "--grep", "BUILD SUCCESS", "--quiet"])
        .output()
        .unwrap();
    writer.join().unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("SUCCESS after"), "got {text:?}");
}

#[test]
fn await_matchers_are_decisive_and_timeout_is_hard() {
    let dir = scratch("await-abort");
    std::fs::write(dir.join("ci.log"), "step 1 ok\nBUILD FAILURE\n").unwrap();

    // The probe surfaces the log content (cat is allowlisted); the failure
    // marker ends the wait on the first tick, exit 1 — decisive over the
    // missing success marker.
    let started = Instant::now();
    let out = ct_await(&dir)
        .args(["--every", "0.2", "--timeout", "30", "--quiet"])
        .args(["--ok-match", "BUILD SUCCESS", "--err-match", "BUILD FAILURE"])
        .args(["--", "cat", "ci.log"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "err-match => ERROR; stderr: {:?}", stderr(&out));
    assert!(started.elapsed() < Duration::from_secs(10), "abort is immediate");
    assert!(stderr(&out).contains("--err-match 'BUILD FAILURE' matched"));

    // A required ok-match is fail-closed: the probe exiting 0 without the
    // marker is "not yet" — until the external work appends it.
    std::fs::write(dir.join("slow.log"), "step 1 ok\n").unwrap();
    let slow = dir.join("slow.log");
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        std::fs::write(&slow, "step 1 ok\nBUILD SUCCESS\n").unwrap();
    });
    let out = ct_await(&dir)
        .args(["--every", "0.1", "--timeout", "10", "--quiet"])
        .args(["--ok-match", "BUILD SUCCESS", "--emit", "{RESULT} ticks={TICKS}"])
        .args(["--", "cat", "slow.log"])
        .output()
        .unwrap();
    writer.join().unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
    let ticks: u64 = stdout(&out)
        .split("ticks=")
        .nth(1)
        .and_then(|s| s.trim().parse().ok())
        .unwrap();
    assert!(ticks > 1, "exit 0 alone must not satisfy a required ok-match");

    // A condition that never comes true ends at the bound, exit 1, reasoned.
    let started = Instant::now();
    let out = ct_await(&dir)
        .args(["--every", "0.1", "--timeout", "0.5", "--quiet"])
        .args(["--", "false"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "timeout => ERROR");
    assert!(started.elapsed() < Duration::from_secs(10), "bound is hard");
    assert!(stderr(&out).contains("timed out after 0.5s"), "got {:?}", stderr(&out));
}

#[test]
fn await_probe_gate_is_the_read_only_set() {
    let dir = scratch("await-gate");
    for probe in [
        vec!["rm", "-rf", "x"],
        vec!["sh", "-c", "true"],
        vec!["ct-edit", "--find", "a", "--replace", "b"],
        vec!["ct-each", "--items", "x", "--mutating", "--", "ct-edit"],
        vec!["ct-await", "--timeout", "1", "--", "true"], // no self-nesting
    ] {
        let mut args = vec!["--timeout", "1", "--"];
        args.extend(probe.iter().copied());
        let out = ct_await(&dir).args(&args).output().unwrap();
        assert_eq!(code(&out), 2, "probe {probe:?} must be refused");
        assert!(stderr(&out).contains("not an allowed probe"));
    }

    // ct-check is gated read-only, so awaiting the invariants is one command.
    std::fs::create_dir_all(dir.join(".ct")).unwrap();
    std::fs::write(
        dir.join(".ct/rules.jsonc"),
        "{\n  \"defs\": {\n  },\n  \"rules\": [\n  ]\n}\n",
    )
    .unwrap();
    let out = ct_await(&dir)
        .args(["--every", "0.1", "--timeout", "5", "--quiet", "--", "ct-check", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
}
