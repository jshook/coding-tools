// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on the built-in `deps` check (real `cargo metadata` over this very
//! workspace: deny/forbid evidence paths, duplicates, defective assertions),
//! run in-process via the library, and end-to-end on `ct-await` (success on a
//! condition that becomes true, immediate abort-on, hard timeout, and the
//! immutable probe gate) driven through `CARGO_BIN_EXE_ct-await`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use coding_tools::deps;
use coding_tools::rules::ProbeOutcome;

mod common;

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

/// Run the built-in `deps` check over this very workspace, in-process.
/// Returns `(outcome, reason, violation report)`.
fn deps_check(args: &[&str]) -> (ProbeOutcome, String, String) {
    let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    deps::check(&argv, repo(), None)
}

fn ct_await(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ct-await"));
    c.current_dir(dir);
    c
}

#[test]
fn deps_deny_reports_an_evidence_path_for_a_real_dependency() {
    // clap IS a dependency of this crate: the check violates with proof.
    let (o, _reason, report) = deps_check(&["--deny", "clap"]);
    assert_eq!(o, ProbeOutcome::Violated);
    assert!(
        report.contains("deny: clap: coding-tools v"),
        "evidence: {report:?}"
    );
    assert!(
        report.contains("-> clap v"),
        "reaches the crate: {report:?}"
    );

    // An absent crate holds.
    assert_eq!(deps_check(&["--deny", "openssl"]).0, ProbeOutcome::Holds);
}

#[test]
fn deps_forbid_duplicates_and_defective_assertions() {
    // A real reachable pair: this crate depends on walkdir.
    let (o, _, report) = deps_check(&["--forbid", "coding-tools=>walkdir"]);
    assert_eq!(o, ProbeOutcome::Violated);
    assert!(
        report.contains("forbid: coding-tools=>walkdir:"),
        "{report:?}"
    );

    // No path in the reverse direction.
    assert_eq!(
        deps_check(&["--forbid", "walkdir=>coding-tools"]).0,
        ProbeOutcome::Holds
    );

    // A source package that does not exist is a defective assertion (Broken).
    let (o, reason, _) = deps_check(&["--forbid", "ghost=>walkdir"]);
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(reason.contains("no package named 'ghost'"), "{reason:?}");

    // Duplicates: this repo's own invariant says there are none.
    assert_eq!(deps_check(&["--duplicates"]).0, ProbeOutcome::Holds);

    // No assertions is a defective probe.
    let (o, reason, _) = deps_check(&[]);
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(reason.contains("nothing to assert"), "{reason:?}");
}

#[test]
fn deps_rejects_unknown_flag_with_a_valid_flags_hint() {
    // A bad argument is BROKEN and the message lists the real flags, sourced
    // from the clap grammar so the hint cannot drift from the check.
    let (o, reason, _) = deps_check(&["--bogus"]);
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(reason.contains("valid flags"), "{reason:?}");
    assert!(
        reason.contains("--deny") && reason.contains("--acyclic"),
        "{reason:?}"
    );
}

#[test]
fn deps_acyclic_and_layers_over_this_workspace() {
    assert_eq!(
        deps_check(&["--acyclic", "--edges", "normal"]).0,
        ProbeOutcome::Holds
    );
    // Member-scoped: this single-crate workspace has no member-to-member cycle.
    assert_eq!(
        deps_check(&["--acyclic", "--members"]).0,
        ProbeOutcome::Holds
    );
    // A single matching layer is trivially clean (no pair to violate).
    assert_eq!(
        deps_check(&["--layers", "coding-tools"]).0,
        ProbeOutcome::Holds
    );

    // A layer matching no member is a defective spec (the typo guard).
    let (o, reason, _) = deps_check(&["--layers", "coding-tools,ghost"]);
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(
        reason.contains("layer 'ghost' matches nothing"),
        "{reason:?}"
    );

    // Spec errors are Broken with a specific reason.
    let (o, reason, _) = deps_check(&["--members"]);
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(
        reason.contains("--members applies to --acyclic"),
        "{reason:?}"
    );

    let (o, reason, _) = deps_check(&["--layers-closed"]);
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(
        reason.contains("--layers-closed requires --layers"),
        "{reason:?}"
    );
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
        .args([
            "--",
            "ct-search",
            "--base",
            "build.log",
            "--grep",
            "BUILD SUCCESS",
            "--quiet",
        ])
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

    // The probe surfaces the log content (ct-view is read-only and
    // cross-platform); the failure marker ends the wait on the first tick,
    // exit 1 — decisive over the missing success marker.
    let started = Instant::now();
    let out = ct_await(&dir)
        .args(["--every", "0.2", "--timeout", "30", "--quiet"])
        .args([
            "--ok-match",
            "BUILD SUCCESS",
            "--err-match",
            "BUILD FAILURE",
        ])
        .args(["--", "ct-view", "ci.log"])
        .output()
        .unwrap();
    assert_eq!(
        code(&out),
        1,
        "err-match => ERROR; stderr: {:?}",
        stderr(&out)
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "abort is immediate"
    );
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
        .args([
            "--ok-match",
            "BUILD SUCCESS",
            "--emit",
            "{RESULT} ticks={TICKS}",
        ])
        .args(["--", "ct-view", "slow.log"])
        .output()
        .unwrap();
    writer.join().unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
    let ticks: u64 = stdout(&out)
        .split("ticks=")
        .nth(1)
        .and_then(|s| s.trim().parse().ok())
        .unwrap();
    assert!(
        ticks > 1,
        "exit 0 alone must not satisfy a required ok-match"
    );

    // A condition that never comes true ends at the bound, exit 1, reasoned.
    let started = Instant::now();
    let out = ct_await(&dir)
        .args(["--every", "0.1", "--timeout", "0.5", "--quiet"])
        .arg("--")
        .args(common::exit_err(&dir))
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "timeout => ERROR");
    assert!(started.elapsed() < Duration::from_secs(10), "bound is hard");
    assert!(
        stderr(&out).contains("timed out after 0.5s"),
        "got {:?}",
        stderr(&out)
    );
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
        .args([
            "--every",
            "0.1",
            "--timeout",
            "5",
            "--quiet",
            "--",
            "ct-check",
            "--quiet",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
}
