// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end guards on `ct-deps`: real `cargo metadata` over this very
//! workspace — deny/forbid evidence paths, duplicates, defective
//! assertions. The binaries are driven through the paths Cargo exports
//! (`CARGO_BIN_EXE_*`).

use std::path::Path;
use std::process::{Command, Output};

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
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
