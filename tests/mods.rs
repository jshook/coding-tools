// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on the built-in `mods` check: a fixture crate (`api -> svc -> db`)
//! written to a scratch directory and run in-process via the library —
//! acyclic/layers holding for a clean crate, `--forbid` catching a transitive
//! module edge, and the spec guards.

use std::path::{Path, PathBuf};
use std::time::Duration;

use coding_tools::modgraph;
use coding_tools::rules::ProbeOutcome;

/// Write the `api -> svc -> db` fixture into a per-test scratch dir (overwrites,
/// never needs cleaning) and return its path.
fn fixture(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/mods")
        .join(tag);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lib.rs"), "mod api;\nmod svc;\nmod db;\n").unwrap();
    std::fs::write(dir.join("api.rs"), "use crate::svc::S;\npub fn f() {}\n").unwrap();
    std::fs::write(dir.join("svc.rs"), "use crate::db::D;\npub struct S;\n").unwrap();
    std::fs::write(dir.join("db.rs"), "pub struct D;\n").unwrap();
    dir
}

/// Run the built-in `mods` check over the fixture (`--base .` so the fixture
/// dir itself is the crate root). Returns `(outcome, reason, violation report)`.
fn mods_check(dir: &Path, args: &[&str]) -> (ProbeOutcome, String, String) {
    let mut argv: Vec<String> = vec!["--base".into(), ".".into()];
    argv.extend(args.iter().map(|s| s.to_string()));
    modgraph::check(&argv, dir, None)
}

#[test]
fn mods_acyclic_and_layers_hold_for_a_clean_crate() {
    let dir = fixture("clean");
    // api -> svc -> db is acyclic and respects the declared (highest-first) order.
    assert_eq!(mods_check(&dir, &["--acyclic"]).0, ProbeOutcome::Holds);
    assert_eq!(
        mods_check(&dir, &["--layers", "api,svc,db"]).0,
        ProbeOutcome::Holds
    );
}

#[test]
fn mods_forbid_catches_a_transitive_module_edge() {
    let dir = fixture("forbid");
    // api reaches db only through svc: the evidence path proves the hop.
    let (o, _reason, report) = mods_check(&dir, &["--forbid", "api=>db"]);
    assert_eq!(o, ProbeOutcome::Violated);
    assert!(
        report.contains("forbid: api=>db: api -> svc -> db"),
        "{report:?}"
    );
    // The reverse does not hold.
    assert_eq!(
        mods_check(&dir, &["--forbid", "db=>api"]).0,
        ProbeOutcome::Holds
    );
}

#[test]
fn mods_guards_are_specific() {
    let dir = fixture("guards");
    // A layer that matches no module is a defective spec.
    let (o, reason, _) = mods_check(&dir, &["--layers", "ghost"]);
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(reason.contains("matches nothing"), "{reason:?}");
    // No assertion is a defective probe.
    let (o, reason, _) = mods_check(&dir, &[]);
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(reason.contains("nothing to assert"), "{reason:?}");
}

#[test]
fn mods_rejects_unknown_flag_with_a_valid_flags_hint() {
    let dir = fixture("badflag");
    // A bad argument is BROKEN and the message lists the real flags (sourced
    // from the clap grammar, so the hint cannot drift from the check).
    let (o, reason, _) = mods_check(&dir, &["--bogus"]);
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(reason.contains("valid flags"), "{reason:?}");
    assert!(
        reason.contains("--acyclic") && reason.contains("--layers"),
        "{reason:?}"
    );
}

#[test]
fn mods_honors_timeout() {
    let dir = fixture("timeout");
    // A zero bound trips on the first file the walk yields: --timeout is honored
    // (mods walks the tree itself, so the deadline is checked cooperatively),
    // not silently ignored as it once was.
    let argv = vec![
        "--base".to_string(),
        ".".to_string(),
        "--acyclic".to_string(),
    ];
    let (o, reason, _) = modgraph::check(&argv, &dir, Some(Duration::ZERO));
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(reason.contains("timed out"), "{reason:?}");
}
