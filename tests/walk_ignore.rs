// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards the gitignore-aware walk shared by every selection tool: by default
//! it skips what git would (paths matched by a `.gitignore`, and the `.git`
//! directory always), and `--no-ignore` opts back into the ignored paths while
//! `.git` stays skipped. Regression test for `ct-tree --base .` descending into
//! a 3 GB `target/`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch crate with one kept file, one gitignored file, and a `.git`
/// directory (overwrites in place, never needs cleaning).
fn fixture(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/walk-ignore")
        .join(tag);
    std::fs::create_dir_all(dir.join("ignored")).unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".gitignore"), "ignored/\n").unwrap();
    std::fs::write(dir.join("keep.rs"), "fn keep() {}\n").unwrap();
    std::fs::write(dir.join("ignored/skip.rs"), "fn skip() {}\n").unwrap();
    std::fs::write(dir.join(".git/config.rs"), "fn vcs() {}\n").unwrap();
    dir
}

/// `ct-search --list` over the fixture's `*.rs`, returning the matched paths.
fn search(dir: &Path, extra: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_ct-search"))
        .args(["--base", dir.to_str().unwrap(), "--name", "*.rs", "--list"])
        .args(extra)
        .output()
        .expect("run ct-search");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn walk_skips_gitignored_and_dotgit_by_default() {
    let out = search(&fixture("default"), &[]);
    assert!(out.contains("keep.rs"), "tracked file should be found: {out:?}");
    assert!(!out.contains("skip.rs"), "gitignored file must be skipped: {out:?}");
    assert!(!out.contains("config.rs"), ".git contents must be skipped: {out:?}");
}

#[test]
fn no_ignore_reaches_gitignored_but_never_dotgit() {
    let out = search(&fixture("no-ignore"), &["--no-ignore"]);
    assert!(out.contains("keep.rs"), "{out:?}");
    assert!(out.contains("skip.rs"), "--no-ignore should reach gitignored files: {out:?}");
    assert!(
        !out.contains("config.rs"),
        ".git stays skipped even under --no-ignore: {out:?}"
    );
}
