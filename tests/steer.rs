// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end tests for `ct-steer` driven through the binary Cargo exports
//! (`CARGO_BIN_EXE_ct-steer`). The classifier itself is unit-tested in
//! `coding_tools::steer`; these exercise the three surfaces an operator and the
//! Claude Code harness actually touch: the runtime `hook` (stdin → decision),
//! `check`, and the `install`/`uninstall` settings merge against a real file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A unique, overwrite-friendly scratch dir under `target/` (never removed).
fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/steer")
        .join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn steer() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ct-steer"))
}

/// A scratch project **outside** the repo tree (under the system temp dir), so
/// the default `.ct` discovery does not walk up into the repo's own `.ct`.
fn temp_scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("ct-steer-tests").join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `ct steer hook <extra…>` in `dir` with logging left at its default, and
/// with `CT_STEER_LOG` scrubbed so the environment can't redirect it.
fn run_hook_in(dir: &Path, envelope: &str, extra: &[&str]) -> Output {
    let mut child = steer()
        .arg("hook")
        .args(extra)
        .current_dir(dir)
        .env_remove("CT_STEER_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// The single daily-log file in a directory, asserting there is exactly one and
/// that it is named `yyyy-mm-dd.jsonl`.
fn only_daily_log(dir: &Path) -> PathBuf {
    let files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "exactly one daily log file in {dir:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name.len(), "yyyy-mm-dd.jsonl".len(), "name was {name}");
    assert!(
        name.ends_with(".jsonl") && name.as_bytes()[4] == b'-',
        "name was {name}"
    );
    files[0].clone()
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("child exited via a signal")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run `ct steer hook <extra…>` feeding `envelope` on stdin. Logging is disabled
/// (`--no-log`) so these decision-focused runs never touch the filesystem; the
/// default-on logging path has its own dedicated tests below.
fn run_hook(envelope: &str, extra: &[&str]) -> Output {
    let mut child = steer()
        .arg("hook")
        .arg("--no-log")
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn bash_envelope(command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string()
}

#[test]
fn hook_denies_a_find_grep_pipeline() {
    let out = run_hook(&bash_envelope("find . -name '*.rs' | xargs grep TODO"), &[]);
    assert_eq!(code(&out), 0, "the hook always exits 0");
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("decision JSON");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(
        v["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("ct search"),
        "reason names the ct tool: {}",
        stdout(&out)
    );
}

#[test]
fn hook_denies_a_harness_grep_call() {
    let envelope = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Grep",
        "tool_input": { "pattern": "TODO", "path": "src", "glob": "*.rs" },
    })
    .to_string();
    let out = run_hook(&envelope, &[]);
    assert_eq!(code(&out), 0);
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("decision JSON");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(
        v["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("ct search"),
        "reason names ct search: {}",
        stdout(&out)
    );
}

#[test]
fn hook_allows_a_read_of_an_image() {
    // Read of a non-renderable path passes through (Read stays the right tool).
    let envelope = serde_json::json!({
        "tool_name": "Read",
        "tool_input": { "file_path": "docs/diagram.png" },
    })
    .to_string();
    let out = run_hook(&envelope, &[]);
    assert_eq!(code(&out), 0);
    assert!(
        stdout(&out).trim().is_empty(),
        "image Read is allowed silently"
    );
}

#[test]
fn hook_modes_change_the_decision() {
    let env = bash_envelope("grep -r TODO src");
    let ask: serde_json::Value =
        serde_json::from_str(&stdout(&run_hook(&env, &["--mode", "ask"]))).unwrap();
    assert_eq!(ask["hookSpecificOutput"]["permissionDecision"], "ask");

    let warn: serde_json::Value =
        serde_json::from_str(&stdout(&run_hook(&env, &["--mode", "warn"]))).unwrap();
    assert!(warn["hookSpecificOutput"]["additionalContext"].is_string());
    assert!(
        warn["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none()
    );
}

#[test]
fn hook_is_silent_and_fails_open_on_misses() {
    // a command with no ct analogue
    let allow = run_hook(&bash_envelope("git status"), &[]);
    assert_eq!(code(&allow), 0);
    assert!(stdout(&allow).trim().is_empty(), "allow is silent");

    // a non-Bash tool
    let other = run_hook(r#"{"tool_name":"Read","tool_input":{}}"#, &[]);
    assert_eq!(code(&other), 0);
    assert!(stdout(&other).trim().is_empty());

    // malformed input
    let bad = run_hook("not json at all", &[]);
    assert_eq!(code(&bad), 0);
    assert!(stdout(&bad).trim().is_empty());
}

#[test]
fn check_exit_codes_mirror_the_decision() {
    let steered = steer()
        .args(["check", "grep -r TODO src"])
        .output()
        .unwrap();
    assert_eq!(code(&steered), 1, "a steered command exits 1");
    assert!(stdout(&steered).contains("ct search"));

    let allowed = steer().args(["check", "git status"]).output().unwrap();
    assert_eq!(code(&allowed), 0, "an allowed command exits 0");
}

#[test]
fn install_creates_idempotent_settings_then_uninstalls() {
    let dir = scratch("install");
    let settings = dir.join(".claude").join("settings.json");

    // fresh install writes the hook
    let first = steer()
        .args(["install", "--scope", "project"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(code(&first), 0);
    let written = std::fs::read_to_string(&settings).expect("settings.json created");
    assert!(written.contains("PreToolUse"));
    assert!(written.contains("ct steer hook"));
    assert!(written.contains("\"matcher\": \"Bash\""));

    // re-install is a no-op (content unchanged)
    let again = steer()
        .args(["install", "--scope", "project"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(code(&again), 0);
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), written);

    // uninstall removes the hook
    let removed = steer()
        .args(["uninstall", "--scope", "project"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(code(&removed), 0);
    assert!(
        !std::fs::read_to_string(&settings)
            .unwrap()
            .contains("steer hook")
    );
}

#[test]
fn install_with_multiple_tools_writes_a_matcher_each() {
    let dir = scratch("multitool");
    let settings = dir.join(".claude").join("settings.json");
    let out = steer()
        .args(["install", "--tools", "Bash,Grep,Glob,Read"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0);
    let written = std::fs::read_to_string(&settings).expect("settings.json created");
    for matcher in ["\"Bash\"", "\"Grep\"", "\"Glob\"", "\"Read\""] {
        assert!(written.contains(matcher), "missing {matcher} in {written}");
    }

    // uninstall removes every steer matcher in one pass
    let removed = steer()
        .args(["uninstall", "--scope", "project"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(code(&removed), 0);
    assert!(
        !std::fs::read_to_string(&settings)
            .unwrap()
            .contains("steer hook")
    );
}

#[test]
fn install_print_writes_nothing() {
    let dir = scratch("print");
    let out = steer()
        .args(["install", "--print"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).contains("ct steer hook"));
    assert!(
        !dir.join(".claude").exists(),
        "--print must not touch the filesystem"
    );
}

#[test]
fn hook_logs_every_call_by_default_under_ct_tclog() {
    let dir = temp_scratch("default-log");
    // A `.ct` here makes this the project root, so logs land in <dir>/.ct/tclog.
    std::fs::create_dir_all(dir.join(".ct")).unwrap();

    // An allowed command is still logged — the missed-pattern raw material.
    let out = run_hook_in(&dir, &bash_envelope("git status"), &[]);
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).trim().is_empty(), "an allow is still silent");

    let file = only_daily_log(&dir.join(".ct").join("tclog"));
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("git status"),
        "logged the command: {content}"
    );
    assert!(content.contains("\"decision\":\"allow\""), "{content}");
    assert!(content.contains("\"tool\":\"Bash\""), "{content}");

    // The log directory is excluded via .ct/.gitignore (*log matches tclog).
    let gitignore = std::fs::read_to_string(dir.join(".ct").join(".gitignore")).unwrap();
    assert!(gitignore.contains("*log"), "gitignore: {gitignore}");

    // A second call appends to the same day's file.
    run_hook_in(&dir, &bash_envelope("ls"), &[]);
    let after = std::fs::read_to_string(&file).unwrap();
    assert_eq!(after.lines().count(), 2, "second call appends: {after}");
}

#[test]
fn hook_log_dir_override_writes_there_and_leaves_ct_alone() {
    let dir = temp_scratch("override-log");
    let logs = dir.join("mylogs");
    let out = run_hook_in(
        &dir,
        &bash_envelope("git status"),
        &["--log-dir", logs.to_str().unwrap()],
    );
    assert_eq!(code(&out), 0);
    only_daily_log(&logs); // a daily file landed in the override directory
    assert!(
        !dir.join(".ct").exists(),
        "an explicit --log-dir must not create or manage .ct"
    );
}

#[test]
fn hook_no_log_writes_nothing() {
    let dir = temp_scratch("no-log");
    std::fs::create_dir_all(dir.join(".ct")).unwrap();
    let out = run_hook_in(&dir, &bash_envelope("git status"), &["--no-log"]);
    assert_eq!(code(&out), 0);
    assert!(
        !dir.join(".ct").join("tclog").exists(),
        "--no-log must write no log files"
    );
}
