// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end guards on the invariant surface (`ct-rules` + `ct-check`):
//! verify-on-add (strict and `--pending`), promotion, the immutable probe
//! gate (mutating tools and self-recursion refused; `{def:}` expansion),
//! lanes and their exit-status mapping (`WARN` soft, `BROKEN` ⇒ exit 2),
//! upward store discovery with root-relative probes, comment preservation
//! across store edits, and the cargo hook's loud-degradation shim. The
//! binaries are driven through the paths Cargo exports (`CARGO_BIN_EXE_*`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A unique, overwrite-friendly scratch project under `target/`. The rule
/// store is rewritten from scratch each run (overwrite preferred to removal).
fn project(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/it")
        .join(tag);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join(".ct")).unwrap();
    dir
}

/// Reset a project's store to the empty scaffold.
fn fresh_store(dir: &Path) {
    std::fs::write(
        dir.join(".ct/rules.jsonc"),
        "{\n  // test store\n  \"defs\": {\n  },\n  \"rules\": [\n  ]\n}\n",
    )
    .unwrap();
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

fn ct_rules(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ct-rules"));
    c.current_dir(dir);
    c
}

fn ct_check(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ct-check"));
    c.current_dir(dir);
    c
}

#[test]
fn add_verifies_now_strict_refuses_failing_candidates() {
    let dir = project("rules-add");
    fresh_store(&dir);
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();

    // A rule that holds records, with provenance.
    let ok = ct_rules(&dir)
        .args(["--add", "no-dbg", "--question", "No dbg! in src?", "--why", "hygiene"])
        .args(["--", "ct-search", "--base", "src", "--grep", "dbg!", "--expect", "none", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&ok), 0, "stderr: {:?}", stderr(&ok));
    let store = std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap();
    assert!(store.contains("// test store"), "comments preserved: {store:?}");
    assert!(store.contains("\"added\""), "provenance recorded");

    // A failing candidate is refused (exit 1) and NOT recorded.
    std::fs::write(dir.join("src/main.rs"), "fn main() { dbg!(1); }\n").unwrap();
    let refused = ct_rules(&dir)
        .args(["--add", "no-dbg-2", "--question", "q"])
        .args(["--", "ct-search", "--base", "src", "--grep", "dbg!", "--expect", "none", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&refused), 1, "failing candidate refused");
    assert!(stderr(&refused).contains("not recorded"));
    assert!(!std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap().contains("no-dbg-2"));

    // Duplicate ids are refused.
    let dup = ct_rules(&dir)
        .args(["--add", "no-dbg", "--question", "q", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(code(&dup), 2, "duplicate id refused");
}

#[test]
fn pending_lane_and_promotion() {
    let dir = project("rules-pending");
    fresh_store(&dir);
    std::fs::write(dir.join("src/lib.rs"), "fn f() { x.unwrap(); }\n").unwrap();

    // Record an aspiration that does not yet hold.
    let add = ct_rules(&dir)
        .args(["--add", "no-unwrap", "--pending", "--question", "No unwrap?"])
        .args(["--", "ct-search", "--base", "src", "--grep", "unwrap", "--expect", "none", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0, "stderr: {:?}", stderr(&add));
    assert!(stdout(&add).contains("pending"));

    // PENDING never reddens the run.
    let check = ct_check(&dir).output().unwrap();
    assert_eq!(code(&check), 0, "pending must not fail: {:?}", stderr(&check));
    assert!(stdout(&check).contains("PENDING"));
    assert!(stdout(&check).contains("not yet held"));

    // Promotion is refused while it still fails...
    let early = ct_rules(&dir).args(["--promote", "no-unwrap"]).output().unwrap();
    assert_eq!(code(&early), 1, "premature promotion refused");

    // ...and succeeds (clearing the flag) once the code is clean.
    std::fs::write(dir.join("src/lib.rs"), "fn f() {}\n").unwrap();
    let promote = ct_rules(&dir).args(["--promote", "no-unwrap"]).output().unwrap();
    assert_eq!(code(&promote), 0, "stderr: {:?}", stderr(&promote));
    let check = ct_check(&dir).output().unwrap();
    assert!(stdout(&check).contains("SUCCESS  no-unwrap"), "now enforced: {:?}", stdout(&check));
    assert!(!stdout(&check).contains("PENDING"));
}

#[test]
fn lanes_map_to_exit_status_warn_soft_broken_hard() {
    let dir = project("rules-lanes");
    fresh_store(&dir);
    std::fs::write(dir.join("src/lib.rs"), "fn f() { x.unwrap(); }\n").unwrap();

    // severity warn: violated but soft.
    let add = ct_rules(&dir)
        .args(["--add", "no-unwrap", "--severity", "warn", "--question", "No unwrap?", "--pending"])
        .args(["--", "ct-search", "--base", "src", "--grep", "unwrap", "--expect", "none", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0);
    // Promote-by-hand for the test: rewrite store without the pending flag.
    let store = std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap();
    assert!(store.contains("\n      \"pending\": true,\n"), "pretty store: {store:?}");
    std::fs::write(
        dir.join(".ct/rules.jsonc"),
        store.replace("\n      \"pending\": true,", ""),
    )
    .unwrap();

    let check = ct_check(&dir).output().unwrap();
    assert_eq!(code(&check), 0, "warn never reddens: {:?}", stderr(&check));
    assert!(stdout(&check).contains("WARN"), "got {:?}", stdout(&check));
    assert!(stderr(&check).contains("'no-unwrap' WARN"), "explained on stderr");

    // A broken probe (missing file named directly) => BROKEN, exit 2.
    let store = std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap();
    let with_broken = store.replace(
        "\"rules\": [",
        "\"rules\": [{\"id\":\"stale\",\"question\":\"q\",\"probe\":[\"ct-view\",\"src/gone.rs\"]},",
    );
    std::fs::write(dir.join(".ct/rules.jsonc"), with_broken).unwrap();
    let check = ct_check(&dir).output().unwrap();
    assert_eq!(code(&check), 2, "broken rule => exit 2");
    assert!(stdout(&check).contains("BROKEN"));
    assert!(stderr(&check).contains("fix or remove with ct-rules"));
}

#[test]
fn probe_gate_is_immutable_and_def_expansion_works() {
    let dir = project("rules-gate");
    fresh_store(&dir);
    std::fs::write(dir.join("src/lib.rs"), "struct Parser; struct Lexer;\n").unwrap();

    // Mutating tools are never probes, with or without flags.
    for probe in [
        vec!["ct-edit", "--find", "a", "--replace", "b"],
        vec!["ct-each", "--items", "x", "--mutating", "--", "ct-edit"],
        vec!["rm", "-rf", "src"],
        vec!["sh", "-c", "true"],
        vec!["ct-check"], // no self-recursion
        vec!["cargo", "publish"],
    ] {
        let mut args = vec!["--add", "bad", "--question", "q", "--"];
        args.extend(probe.iter().copied());
        let out = ct_rules(&dir).args(&args).output().unwrap();
        assert_eq!(code(&out), 2, "probe {probe:?} must be refused");
    }

    // Defs: list splice through ct-each, validated and run at add time.
    let def = ct_rules(&dir)
        .args(["--def", r#"core-types=["Parser","Lexer"]"#])
        .output()
        .unwrap();
    assert_eq!(code(&def), 0, "stderr: {:?}", stderr(&def));
    let add = ct_rules(&dir)
        .args(["--add", "types-used", "--question", "Core types referenced?"])
        .args(["--", "ct-each", "--items", "{def:core-types}", "--quiet", "--"])
        .args(["ct-search", "--base", "src", "--grep", "{ITEM}", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0, "stderr: {:?}", stderr(&add));
    let check = ct_check(&dir).output().unwrap();
    assert_eq!(code(&check), 0, "stderr: {:?}", stderr(&check));
    assert!(stdout(&check).contains("SUCCESS  types-used"));

    // An unknown def is a load-time error naming the rule (exit 2).
    let store = std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap();
    std::fs::write(
        dir.join(".ct/rules.jsonc"),
        store.replace("{def:core-types}", "{def:nope}"),
    )
    .unwrap();
    let check = ct_check(&dir).output().unwrap();
    assert_eq!(code(&check), 2);
    assert!(stderr(&check).contains("unknown def"), "got {:?}", stderr(&check));
}

#[test]
fn store_discovery_is_upward_and_probes_run_from_the_root() {
    let dir = project("rules-discovery");
    fresh_store(&dir);
    std::fs::write(dir.join("src/lib.rs"), "fn clean() {}\n").unwrap();
    let add = ct_rules(&dir)
        .args(["--add", "no-dbg", "--question", "q"])
        .args(["--", "ct-search", "--base", "src", "--grep", "dbg!", "--expect", "none", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0, "stderr: {:?}", stderr(&add));

    // From a subdirectory: the store is found upward and the root-relative
    // probe path still resolves (probes run from the project root).
    let sub = dir.join("src");
    let check = ct_check(&sub).output().unwrap();
    assert_eq!(code(&check), 0, "stderr: {:?}", stderr(&check));
    assert!(stdout(&check).contains("SUCCESS  no-dbg"));

    // With no .ct anywhere upward: a clear exit-2 error.
    let orphan = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/it");
    let lost = ct_check(&orphan).output().unwrap();
    // (target/test-tmp/it sits under this repo, which may itself grow a .ct
    // one day — accept either the not-found error or a successful discovery.)
    if code(&lost) == 2 {
        assert!(
            stderr(&lost).contains("no .ct directory") || stderr(&lost).contains("read"),
            "got {:?}",
            stderr(&lost)
        );
    }
}

#[test]
fn ct_check_is_allowlisted_and_composes() {
    let dir = project("rules-compose");
    fresh_store(&dir);
    std::fs::write(dir.join("src/lib.rs"), "fn clean() {}\n").unwrap();
    let add = ct_rules(&dir)
        .args(["--add", "no-dbg", "--question", "q"])
        .args(["--", "ct-search", "--base", "src", "--grep", "dbg!", "--expect", "none", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0);

    // ct-test frames the whole invariant surface as one experiment.
    let mut wrap = Command::new(env!("CARGO_BIN_EXE_ct-test"));
    wrap.current_dir(&dir)
        .args(["--question", "Do all invariants hold?", "--quiet"])
        .args(["--emit", "{RESULT}"])
        .args(["--cmd", "ct-check", "--", "--quiet"]);
    let out = wrap.output().unwrap();
    assert_eq!(code(&out), 0, "stderr: {:?}", stderr(&out));
    assert!(stdout(&out).contains("SUCCESS"));

    // ct-rules is NOT a permitted ct-test command (it writes).
    let mut deny = Command::new(env!("CARGO_BIN_EXE_ct-test"));
    deny.current_dir(&dir).args(["--cmd", "ct-rules", "--", "--list"]);
    let out = deny.output().unwrap();
    assert_eq!(code(&out), 2, "ct-rules must stay off the allowlist");
}

#[test]
fn store_is_human_friendly_jsonc_with_prompt_retention_and_flatten() {
    let dir = project("rules-pretty");
    fresh_store(&dir);
    std::fs::write(dir.join("src/lib.rs"), "fn clean() {}\n").unwrap();

    // Recording with --prompt retains the verbatim request and says so.
    let add = ct_rules(&dir)
        .args(["--add", "no-dbg", "--question", "No dbg! in src?", "--why", "hygiene"])
        .args(["--prompt", "please make sure we never ship debug prints again"])
        .args(["--", "ct-search", "--base", "src", "--grep", "dbg!", "--expect", "none", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0, "stderr: {:?}", stderr(&add));
    assert!(
        stdout(&add).contains("retained in the rule's \"prompt\" field"),
        "user is told about retention: {:?}",
        stdout(&add)
    );

    let second = ct_rules(&dir)
        .args(["--add", "second", "--question", "q2"])
        .args(["--prompt", "second request"])
        .args(["--", "true"])
        .output()
        .unwrap();
    assert_eq!(code(&second), 0);

    // The store reads like JSONC for humans: header comment on top, one
    // field per line in stable order, blank line between rules, no
    // trailing whitespace, and the prompt verbatim.
    let store = std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap();
    assert!(store.starts_with("// ct rule store"), "header first: {store:?}");
    assert!(store.contains("// Managed by `ct rules`"));
    assert!(store.contains("\n      \"id\": \"no-dbg\",\n      \"question\""), "one field per line");
    assert!(
        store.contains("\"prompt\": \"please make sure we never ship debug prints again\""),
        "prompt verbatim"
    );
    assert!(store.contains("    },\n\n    {"), "blank line between rules: {store:?}");
    assert!(!store.lines().any(|l| l.ends_with(' ')), "no trailing whitespace");

    // A hand-vandalised store (header removed) gets it re-established on
    // the next write.
    let headerless = store.lines().skip(4).collect::<Vec<_>>().join("\n");
    std::fs::write(dir.join(".ct/rules.jsonc"), headerless).unwrap();
    let def = ct_rules(&dir).args(["--def", "x=src"]).output().unwrap();
    assert_eq!(code(&def), 0, "stderr: {:?}", stderr(&def));
    let store = std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap();
    assert!(store.starts_with("// ct rule store"), "header re-established");

    // --flatten strips every prompt, naming what it removed; definitions stay.
    let flat = ct_rules(&dir).args(["--flatten"]).output().unwrap();
    assert_eq!(code(&flat), 0, "stderr: {:?}", stderr(&flat));
    assert!(stdout(&flat).contains("flattened 2 prompt(s)"), "got {:?}", stdout(&flat));
    let store = std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap();
    assert!(!store.contains("\"prompt\""), "prompts gone: {store:?}");
    assert!(store.contains("\"why\": \"hygiene\""), "mechanical definition intact");
    let check = ct_check(&dir).output().unwrap();
    assert_eq!(code(&check), 0, "flattened store still verifies: {:?}", stderr(&check));

    // Flattening twice is a clean no-op.
    let again = ct_rules(&dir).args(["--flatten"]).output().unwrap();
    assert_eq!(code(&again), 0);
    assert!(stdout(&again).contains("nothing to flatten"));
}

#[test]
fn cargo_hook_is_generated_and_refuses_foreign_files() {
    let dir = project("rules-hook");
    fresh_store(&dir);
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"t\"\nversion=\"0.0.0\"\n").unwrap();

    // Overwrite any prior generated shim: regeneration is fine.
    let _ = std::fs::remove_file(dir.join("tests/ct_invariants.rs"));
    let hook = ct_rules(&dir).args(["--hook", "cargo"]).output().unwrap();
    assert_eq!(code(&hook), 0, "stderr: {:?}", stderr(&hook));
    let shim = std::fs::read_to_string(dir.join("tests/ct_invariants.rs")).unwrap();
    assert!(shim.starts_with("// Generated by `ct rules --hook cargo`."));
    assert!(shim.contains("ct check"), "shim runs the surface");
    assert!(shim.contains("could not run `ct`"), "degrades loudly");

    // Regenerating over our own shim is fine; a foreign file is refused.
    assert_eq!(code(&ct_rules(&dir).args(["--hook", "cargo"]).output().unwrap()), 0);
    std::fs::write(dir.join("tests/ct_invariants.rs"), "// hand-written\n").unwrap();
    let refused = ct_rules(&dir).args(["--hook", "cargo"]).output().unwrap();
    assert_eq!(code(&refused), 2);
    assert!(stderr(&refused).contains("not overwriting"));
}

#[test]
fn bridge_probes_run_real_cargo_with_hermetic_flags() {
    // A real cargo workspace for the bridge to interrogate: no dependencies,
    // so the lockfile generates offline and `cargo tree -d` is empty.
    let dir = project("rules-bridge");
    fresh_store(&dir);
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"bridge-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "").unwrap();
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(lock.status.success(), "lockfile: {:?}", stderr(&lock));

    // Recording runs the probe through the bridge NOW — a real `cargo tree`
    // execution with --locked/--offline enforced by the compiled-in entry.
    let add = ct_rules(&dir)
        .args(["--add", "no-duplicate-deps", "--question", "No duplicate crate versions?"])
        .args(["--expect", "empty"])
        .args(["--", "cargo", "tree", "-d"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0, "stderr: {:?}", stderr(&add));

    // And `cargo metadata` through its bridge entry, read by the exit adapter.
    let add = ct_rules(&dir)
        .args(["--add", "metadata-resolves", "--question", "Does the crate graph resolve?"])
        .args(["--", "cargo", "metadata"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0, "stderr: {:?}", stderr(&add));

    let check = ct_check(&dir).output().unwrap();
    assert_eq!(code(&check), 0, "stderr: {:?}", stderr(&check));
    assert!(stdout(&check).contains("SUCCESS  no-duplicate-deps"));
    assert!(stdout(&check).contains("SUCCESS  metadata-resolves"));
}

#[test]
fn builtin_mods_check_records_runs_and_prototypes() {
    let dir = project("builtin-mods");
    fresh_store(&dir);
    // A tiny acyclic module graph: a -> b.
    std::fs::write(dir.join("src/lib.rs"), "mod a;\nmod b;\n").unwrap();
    std::fs::write(dir.join("src/a.rs"), "use crate::b::X;\npub fn f() {}\n").unwrap();
    std::fs::write(dir.join("src/b.rs"), "pub struct X;\n").unwrap();

    // --add runs the built-in `mods` check IN-PROCESS (gate -> run_probe Builtin)
    // and records it because it holds — exercising the consolidation glue.
    let add = ct_rules(&dir)
        .args(["--add", "mods-acyclic", "--question", "Is the module graph acyclic?"])
        .args(["--", "mods", "--acyclic"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0, "stderr: {:?}", stderr(&add));

    // A violating built-in check is refused (the glue detects the real edge).
    let bad = ct_rules(&dir)
        .args(["--add", "no-a-to-b", "--question", "Does a stay off b?"])
        .args(["--", "mods", "--forbid", "a=>b"])
        .output()
        .unwrap();
    assert_eq!(code(&bad), 1, "stderr: {:?}", stderr(&bad));

    // ct check runs the stored built-in rule via the same in-process dispatch.
    let check = ct_check(&dir).output().unwrap();
    assert_eq!(code(&check), 0, "stderr: {:?}", stderr(&check));
    assert!(stdout(&check).contains("SUCCESS  mods-acyclic"), "{:?}", stdout(&check));

    // The stored probe is the bare built-in head — no binary involved.
    let store = std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap();
    assert!(store.contains("\"mods\""), "stored probe head: {store}");
    assert!(!store.contains("ct-mods"), "no binary reference: {store}");

    // Prototype mode: a bare probe runs + reports without saving.
    let proto = ct_rules(&dir).args(["--", "mods", "--acyclic"]).output().unwrap();
    assert_eq!(code(&proto), 0, "stderr: {:?}", stderr(&proto));
    assert!(stdout(&proto).contains("not saved"), "{:?}", stdout(&proto));
    let proto_bad = ct_rules(&dir).args(["--", "mods", "--forbid", "a=>b"]).output().unwrap();
    assert_eq!(code(&proto_bad), 1, "stderr: {:?}", stderr(&proto_bad));

    // Neither prototype wrote: still exactly the one recorded rule.
    let store2 = std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap();
    assert_eq!(store2.matches("\"id\"").count(), 1, "prototypes must not write: {store2}");

    // An --expect adapter on a built-in check is refused, not silently dropped.
    let guard = ct_rules(&dir)
        .args(["--add", "x", "--question", "q", "--expect-ok", "foo"])
        .args(["--", "mods", "--acyclic"])
        .output()
        .unwrap();
    assert_eq!(code(&guard), 2, "stderr: {:?}", stderr(&guard));
    assert!(
        stderr(&guard).contains("classifies its own outcome"),
        "stderr: {:?}",
        stderr(&guard)
    );
}

#[test]
fn prototyping_a_non_builtin_probe_runs_without_saving() {
    let dir = project("prototype-search");
    fresh_store(&dir);
    std::fs::write(dir.join("src/lib.rs"), "pub fn clean() {}\n").unwrap();

    // A bare gated probe with no verb prototypes any allowed tool, not just the
    // built-in checks: it runs and reports, without recording. Holds here —
    // ZZZNOTHERE is absent, so --expect none is SUCCESS (exit 0).
    let hold = ct_rules(&dir)
        .args(["--", "ct-search", "--base", "src", "--grep", "ZZZNOTHERE", "--expect", "none", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&hold), 0, "stderr: {:?}", stderr(&hold));
    assert!(stdout(&hold).contains("not saved"), "{:?}", stdout(&hold));

    // Violated: `clean` is present, so --expect none fails (exit 1) — and the
    // exit status follows the outcome so a prototype composes in &&/||.
    let viol = ct_rules(&dir)
        .args(["--", "ct-search", "--base", "src", "--grep", "clean", "--expect", "none", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&viol), 1, "stderr: {:?}", stderr(&viol));

    // Neither prototype wrote to the store.
    let store = std::fs::read_to_string(dir.join(".ct/rules.jsonc")).unwrap();
    assert_eq!(store.matches("\"id\"").count(), 0, "prototypes must not write: {store}");
}

#[test]
fn ct_each_walker_source_feeds_per_file_rules() {
    let dir = project("rules-walker");
    fresh_store(&dir);
    std::fs::write(dir.join("src/a.rs"), "// SPDX-License-Identifier: Apache-2.0\n").unwrap();
    std::fs::write(dir.join("src/b.rs"), "// SPDX-License-Identifier: Apache-2.0\n").unwrap();

    let add = ct_rules(&dir)
        .args(["--add", "license-headers", "--question", "Every file carries the header?"])
        .args(["--", "ct-each", "--base", "src", "--name", "*.rs", "--quiet", "--"])
        .args(["ct-search", "--base", "{ITEM}", "--grep", "SPDX-License-Identifier", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0, "stderr: {:?}", stderr(&add));

    // Drop the header from one file: the rule reports the violation.
    std::fs::write(dir.join("src/b.rs"), "fn nope() {}\n").unwrap();
    let check = ct_check(&dir).output().unwrap();
    assert_eq!(code(&check), 1, "violation => exit 1");
    assert!(stdout(&check).contains("ERROR    license-headers"));
}

#[test]
fn ct_check_id_mode_pins_interpretation() {
    let dir = project("check-id-mode");
    fresh_store(&dir);
    std::fs::write(dir.join("src/lib.rs"), "fn clean() {}\n").unwrap();
    // One holding rule with id "abc".
    let add = ct_rules(&dir)
        .args(["--add", "abc", "--question", "q"])
        .args(["--", "ct-search", "--base", "src", "--grep", "ZZZ", "--expect", "none", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0, "stderr: {:?}", stderr(&add));

    // --mode pins how --id is read: 'a.c' as a regex matches "abc"; as a literal
    // it does not — so the same pattern selects the rule under one mode, not the other.
    let re = ct_check(&dir).args(["--id", "a.c", "--mode", "regex", "--list"]).output().unwrap();
    assert!(stdout(&re).contains("abc"), "regex --id should match: {:?}", stdout(&re));
    let lit = ct_check(&dir).args(["--id", "a.c", "--mode", "literal", "--list"]).output().unwrap();
    assert!(!stdout(&lit).contains("abc"), "literal --id must not match: {:?}", stdout(&lit));
}

#[test]
fn ct_each_refuses_builtin_check_with_guidance() {
    let dir = project("each-builtin");
    // A built-in check is not a per-item dispatch target; the refusal teaches the
    // right path (the check's own repeatable flags) rather than dead-ending.
    let out = Command::new(env!("CARGO_BIN_EXE_ct-each"))
        .current_dir(&dir)
        .args(["--items", "openssl", "--", "deps", "--deny", "{ITEM}"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 2, "refused dispatch => exit 2");
    let err = stderr(&out);
    assert!(err.contains("built-in check"), "{err:?}");
    assert!(err.contains("deps --deny"), "should point to repeatable flags: {err:?}");
}
