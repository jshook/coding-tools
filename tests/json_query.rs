// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end checks on `ct-json`: the `ct-patch` path grammar read-only, jq
//! filters with joins and variables, JSONL/slurp/stdin/`b64:` inputs, the
//! `--expect` verdict over the result count, bounded `--limit`, the `--json`
//! framing, the error contract, and dispatch through `ct-each` (it is on the
//! read-only allowlist) and the `ct` umbrella.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn ct_json(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ct-json"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn lines(o: &Output) -> Vec<String> {
    stdout(o).lines().map(str::to_string).collect()
}

/// Minimal standard-alphabet base64, for `b64:` payloads.
fn b64(s: &str) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in s.as_bytes().chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

const PATCH: &str = r#"{
  // a tiny modular-synth patch, with a trailing comma
  "wires": [
    {"from_node": 1, "from_port": "out", "to_node": 2, "to_port": "in"},
    {"from_node": 2, "from_port": "out", "to_node": 3, "to_port": "in"},
  ],
  "nodes": [
    {"id": 1, "type_id": "osc"},
    {"id": 2, "type_id": "amp"},
    {"id": 3, "type_id": "dac"},
  ],
}
"#;

fn patch_file(dir: &Path) -> String {
    let f = dir.join("patch.json");
    fs::write(&f, PATCH).unwrap();
    f.to_string_lossy().into_owned()
}

#[test]
fn path_reads_nodes_in_ct_patch_grammar_and_keeps_document_order() {
    let dir = scratch("ct-json-path");
    let f = patch_file(&dir);

    // [*] iterates; each element is one compact line, keys in document order.
    let out = ct_json(&["--file", &f, "--path", "wires[*]"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        lines(&out),
        [
            r#"{"from_node":1,"from_port":"out","to_node":2,"to_port":"in"}"#,
            r#"{"from_node":2,"from_port":"out","to_node":3,"to_port":"in"}"#,
        ]
    );

    // [key=value] picks the first match, value read as JSON; --raw unquotes.
    let out = ct_json(&["--file", &f, "--path", "nodes[id=2].type_id", "--raw"]);
    assert_eq!(lines(&out), ["amp"]);
    let out = ct_json(&["--file", &f, "--path", ".nodes[type_id=dac].id"]);
    assert_eq!(lines(&out), ["3"]);

    // The whole document keeps its key order (wires before nodes), comments
    // and trailing commas notwithstanding.
    let out = ct_json(&["--file", &f, "--query", "keys_unsorted"]);
    assert_eq!(lines(&out), [r#"["wires","nodes"]"#]);

    // A node that is not there is a clean negative: nothing printed, exit 1,
    // the summary on stderr.
    let out = ct_json(&["--file", &f, "--path", "nodes[id=9]"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("0 value(s) -> ERROR (--expect any)"),
        "{}",
        stderr(&out)
    );

    // --path and --query compose: the path selects, the query projects.
    let out = ct_json(&[
        "--file", &f, "--path", "wires[*]", "--query", ".to_port", "--raw",
    ]);
    assert_eq!(lines(&out), ["in", "in"]);
}

#[test]
fn query_joins_two_arrays_binds_variables_and_travels_as_b64() {
    let dir = scratch("ct-json-query");
    let f = patch_file(&dir);
    let join = r#"(.nodes | map({(.id|tostring): .type_id}) | add) as $t | .wires[] | "\($t[.from_node|tostring]).\(.from_port) -> \($t[.to_node|tostring]).\(.to_port)""#;

    let out = ct_json(&["--file", &f, "--query", join, "--raw"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(lines(&out), ["osc.out -> amp.in", "amp.out -> dac.in"]);

    // The same filter, base64-carried: no quoting to survive at all.
    let enc = format!("b64:{}", b64(join));
    let out = ct_json(&["--file", &f, "--query", &enc, "--raw", "--expect", "=2"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(lines(&out), ["osc.out -> amp.in", "amp.out -> dac.in"]);

    // --arg is a string, --argjson is parsed.
    let out = ct_json(&[
        "--file",
        &f,
        "--query",
        ".nodes[] | select(.id > $min) | .type_id + $suffix",
        "--argjson",
        "min=1",
        "--arg",
        "suffix=!",
        "--raw",
    ]);
    assert_eq!(lines(&out), ["amp!", "dac!"]);

    // --pretty indents with a space after each colon.
    let out = ct_json(&["--file", &f, "--path", "nodes[0]", "--pretty"]);
    assert_eq!(stdout(&out), "{\n  \"id\": 1,\n  \"type_id\": \"osc\"\n}\n");
}

#[test]
fn jsonl_records_slurp_stdin_and_text_documents() {
    let dir = scratch("ct-json-jsonl");
    let f = dir.join("events.jsonl");
    fs::write(
        &f,
        "{\"level\":\"info\",\"msg\":\"a\"}\n\n{\"level\":\"warn\",\"msg\":\"b\"}\n{\"level\":\"info\",\"msg\":\"c\"}\n",
    )
    .unwrap();
    let f = f.to_string_lossy().into_owned();

    // Each non-blank line is one input.
    let out = ct_json(&["--file", &f, "--query", ".level", "--raw"]);
    assert_eq!(lines(&out), ["info", "warn", "info"]);

    // --slurp sees them all at once.
    let out = ct_json(&[
        "--file",
        &f,
        "--slurp",
        "--query",
        "group_by(.level) | map({level: .[0].level, n: length})",
    ]);
    assert_eq!(
        lines(&out),
        [r#"[{"level":"info","n":2},{"level":"warn","n":1}]"#]
    );

    // A record that does not parse names its line.
    fs::write(dir.join("bad.jsonl"), "{\"ok\":1}\nnot json\n").unwrap();
    let bad = dir.join("bad.jsonl").to_string_lossy().into_owned();
    let out = ct_json(&["--file", &bad, "--query", "."]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("bad.jsonl:2"), "{}", stderr(&out));

    // A piped document, and an inline one; two --file inputs run in order.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ct-json"))
        .args([
            "--file",
            "stdin:",
            "--file",
            "text:{\"v\": 2}",
            "--query",
            ".v",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"v\": 1}")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(lines(&out), ["1", "2"]);
}

#[test]
fn expect_frames_the_count_limit_bounds_and_json_frames_the_result() {
    let dir = scratch("ct-json-expect");
    let f = patch_file(&dir);

    // --expect over the result count: =2 holds, none fails, --quiet summarises.
    let out = ct_json(&[
        "--file", &f, "--path", "wires[*]", "--expect", "=2", "--quiet",
    ]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "2 value(s) -> SUCCESS\n");
    let out = ct_json(&["--file", &f, "--path", "wires[*]", "--expect", "none"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        lines(&out).len(),
        2,
        "values still print before the verdict"
    );
    let out = ct_json(&["--file", &f, "--path", "nodes[id=9]", "--expect", "none"]);
    assert!(out.status.success(), "absence asserted is a pass");

    // --limit stops evaluation, so an endless generator returns.
    let out = ct_json(&["--file", &f, "--query", "repeat(1)", "--limit", "3"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(lines(&out), ["1", "1", "1"]);

    // --json: the frame, the exact filter that ran, and the values.
    let out = ct_json(&[
        "--file",
        &f,
        "--path",
        "nodes[*].id",
        "--query",
        ". * 10",
        "--json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["tool"], "ct-json");
    assert_eq!(v["verdict"], "SUCCESS");
    assert_eq!(v["count"], 3);
    assert_eq!(v["values"], serde_json::json!([10, 20, 30]));
    assert_eq!(v["files"][0], f);
    assert_eq!(v["filter"], r#"(.["nodes"] | .[] | .["id"]) | (. * 10)"#);
}

#[test]
fn errors_are_exit_2_and_say_what_went_wrong() {
    let dir = scratch("ct-json-errors");
    let f = patch_file(&dir);

    let out = ct_json(&["--query", "."]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--file"), "{}", stderr(&out));

    let out = ct_json(&["--file", &f, "--query", ".nodes["]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("invalid --query") && stderr(&out).contains("offset 7"),
        "{}",
        stderr(&out)
    );

    let out = ct_json(&["--file", &f, "--query", "nosuch"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("undefined filter 'nosuch'"),
        "{}",
        stderr(&out)
    );

    // A filter error at run time names the input.
    let out = ct_json(&["--file", &f, "--query", ".wires | .a"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("patch.json"), "{}", stderr(&out));

    let out = ct_json(&["--file", &f, "--path", "nodes[x]"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("invalid --path"), "{}", stderr(&out));

    let out = ct_json(&["--file", &f, "--arg", "n=1", "--arg", "n=2"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("bound twice"), "{}", stderr(&out));

    let y = dir.join("cfg.yaml");
    fs::write(&y, "a: 1\n").unwrap();
    let out = ct_json(&["--file", &y.to_string_lossy()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("YAML"), "{}", stderr(&out));

    let out = ct_json(&["--file", &dir.join("missing.json").to_string_lossy()]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn ct_json_is_read_only_so_ct_each_dispatches_it_and_ct_resolves_it() {
    let dir = scratch("ct-json-dispatch");
    let f = patch_file(&dir);

    // On the read-only allowlist: ct-each runs it without --mutating.
    let out = Command::new(env!("CARGO_BIN_EXE_ct-each"))
        .args([
            "--items", &f, "--", "ct-json", "--file", "{ITEM}", "--path", "wires[*]", "--expect",
            "=2",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );

    // The umbrella finds ct-json beside itself.
    let out = Command::new(env!("CARGO_BIN_EXE_ct"))
        .args([
            "json",
            "--file",
            &f,
            "--path",
            "nodes[id=1].type_id",
            "--raw",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(lines(&out), ["osc"]);
}
