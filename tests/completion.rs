// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards the `ct` dynamic shell completion (veks-completion): the command tree
//! derived from the clap grammar offers the right subcommands, flags, and
//! value_enum sets, and the runtime providers complete ids/tags/defs from the
//! live `.ct/rules.jsonc` — driven through the real `_CT_COMPLETE` protocol.

use std::path::Path;
use std::process::Command;

/// Run `ct` as a completion callback for `line` (cursor at end), from `cwd` if
/// given, and return the candidate lines it prints.
fn complete(line: &str, cwd: Option<&Path>) -> Vec<String> {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ct"));
    c.env("_CT_COMPLETE", "bash").arg(line);
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    let out = c.output().expect("run ct completion");
    // Candidates may carry a trailing space (the shell convention for advancing
    // to the next word); compare on the bare token.
    String::from_utf8_lossy(&out.stdout).lines().map(|s| s.trim_end().to_string()).collect()
}

/// Run `ct <args>` and return (stdout, stderr, exit code).
fn run(args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_ct")).args(args).output().expect("run ct");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn completions_command_emits_wrapper_and_script() {
    // Bare `ct completions` prints the auto-detecting wrapper, which re-invokes
    // the `--shell` form — the chain that broke when the shell was parsed as a
    // positional only.
    let (wrap, _, code) = run(&["completions"]);
    assert_eq!(code, 0);
    assert!(wrap.contains("completions --shell"), "wrapper must re-invoke --shell: {wrap:?}");

    // The `--shell` form the wrapper calls emits the registration script.
    let (script, _, code) = run(&["completions", "--shell", "bash"]);
    assert_eq!(code, 0);
    assert!(
        script.contains("complete -") && script.contains("_ct_complete"),
        "registration script: {script:?}"
    );

    // A bare positional shell works too.
    let (pos, _, _) = run(&["completions", "bash"]);
    assert!(pos.contains("_ct_complete"), "positional shell: {pos:?}");

    // An unknown shell is a usage error.
    let (_, err, code) = run(&["completions", "--shell", "tcsh"]);
    assert_eq!(code, 2);
    assert!(err.contains("unknown shell"), "err: {err:?}");
}

#[test]
fn completes_subcommands_flags_and_enum_sets() {
    // Subcommands come from the lib-hosted clap grammar, plus the meta-command.
    let subs = complete("ct ", None);
    for want in ["search", "check", "rules", "completions"] {
        assert!(subs.iter().any(|s| s == want), "subcommand {want} missing from {subs:?}");
    }
    // A subcommand's flags.
    let flags = complete("ct search --", None);
    assert!(flags.iter().any(|s| s == "--grep"), "flags: {flags:?}");
    assert!(flags.iter().any(|s| s == "--no-ignore"), "flags: {flags:?}");
    // A value_enum's variants become its completion set.
    let modes = complete("ct search --mode ", None);
    for want in ["literal", "glob", "regex"] {
        assert!(modes.iter().any(|s| s == want), "--mode set missing {want}: {modes:?}");
    }
}

#[test]
fn completes_ids_tags_and_defs_from_the_live_store() {
    // A fixture project with one known rule, tag, and def.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/completion");
    std::fs::create_dir_all(dir.join(".ct")).unwrap();
    std::fs::write(
        dir.join(".ct/rules.jsonc"),
        r#"{"defs":{"core-types":["X"]},"rules":[{"id":"abc-rule","question":"q","probe":["ls"],"tags":["hygiene"]}]}"#,
    )
    .unwrap();

    // Runtime providers read that store — what static clap_complete cannot do.
    let ids = complete("ct check --id ", Some(&dir));
    assert!(ids.iter().any(|s| s == "abc-rule"), "dynamic rule id: {ids:?}");
    let tags = complete("ct check --tag ", Some(&dir));
    assert!(tags.iter().any(|s| s == "hygiene"), "dynamic tag: {tags:?}");
    let defs = complete("ct rules --def ", Some(&dir));
    assert!(defs.iter().any(|s| s == "core-types"), "dynamic def name: {defs:?}");
    let promote = complete("ct rules --promote ", Some(&dir));
    assert!(promote.iter().any(|s| s == "abc-rule"), "dynamic id for --promote: {promote:?}");
}
