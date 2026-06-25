// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `--json-pretty` mirrors `--json` but indented, and enables JSON output on its
//! own (implies `--json`). Driven against `ct-search` — the shared `jsonout`
//! path is the same for every `--json` tool.

use std::process::Command;

fn search(extra: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_ct-search"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["--base", "src", "--name", "jsonout.rs", "--summary"])
        .args(extra)
        .output()
        .expect("run ct-search");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn json_pretty_indents_and_implies_json() {
    // --json is compact: the whole object on one line.
    let compact = search(&["--json"]);
    assert_eq!(
        compact.lines().count(),
        1,
        "compact should be one line: {compact:?}"
    );

    // --json-pretty alone (no --json) emits the same object, indented.
    let pretty = search(&["--json-pretty"]);
    assert!(
        pretty.trim_start().starts_with('{'),
        "pretty is a JSON object: {pretty:?}"
    );
    assert!(
        pretty.lines().count() > 1,
        "pretty should span multiple lines: {pretty:?}"
    );
    assert!(
        pretty.contains("\n  \"tool\""),
        "pretty should indent its keys: {pretty:?}"
    );
}
