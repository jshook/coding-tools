// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end guards on the block-payload surface: the `file:`/`text:` value
//! schemes, the explicit `--mode` switch (promotion off), line-anchored
//! literal block matching in `ct-search`/`ct-view`/`ct-edit` with the
//! nearest-miss diagnostic, and the `ct-edit --script` batch engine under the
//! prepare/confirm/write standard — atomic by construction, with a write
//! pre-flight and zero writes on any failure. The binaries are driven through
//! the paths Cargo exports (`CARGO_BIN_EXE_*`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

/// A unique, overwrite-friendly scratch dir under `target/` (never removed).
fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/blocks")
        .join(tag);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Clear the read-only attribute on `p` if it exists, so an overwrite can't
/// fail. Scratch dirs persist across runs (and Windows enforces the read-only
/// bit on writes), so a file a prior run left read-only must be cleared before
/// it is rewritten. Cross-platform and best-effort.
fn make_writable(p: &Path) {
    let Ok(meta) = std::fs::metadata(p) else {
        return;
    };
    let mut perms = meta.permissions();
    if !perms.readonly() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() | 0o200); // restore owner write only
    }
    #[cfg(not(unix))]
    {
        // Windows has no mode bits; clearing the read-only attribute is the
        // only way to make the file writable again.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
    }
    let _ = std::fs::set_permissions(p, perms);
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

fn tool(name: &str) -> Command {
    let exe = match name {
        "ct-search" => env!("CARGO_BIN_EXE_ct-search"),
        "ct-view" => env!("CARGO_BIN_EXE_ct-view"),
        "ct-edit" => env!("CARGO_BIN_EXE_ct-edit"),
        "ct-patch" => env!("CARGO_BIN_EXE_ct-patch"),
        "ct-test" => env!("CARGO_BIN_EXE_ct-test"),
        "ct-each" => env!("CARGO_BIN_EXE_ct-each"),
        other => panic!("unknown tool {other}"),
    };
    Command::new(exe)
}

/// The sample source used across the block tests.
const SAMPLE: &str = "enum Value {\n    U64(u64),\n}\n\nfn show(v: &Value) -> String {\n    match v {\n        Value::U64(v) => v.to_string(),\n    }\n}\n";

#[test]
fn schemes_resolve_file_text_and_leave_other_prefixes_alone() {
    let dir = scratch("schemes");
    std::fs::write(dir.join("hay.txt"), "a std::fmt line\nfile:not-a-read\n").unwrap();
    std::fs::write(dir.join("pat.block"), "std::fmt\n").unwrap();

    // file: sources the pattern from a file, matched literally by default.
    let out = tool("ct-search")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--grep",
            &format!("file:{}", dir.join("pat.block").display()),
            "--quiet",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    // text: escapes the prefix: the literal string 'file:not-a-read' matches.
    let out = tool("ct-search")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--grep",
            "text:file:not-a-read",
            "--quiet",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    // Unrecognised prefixes are literal as-is: 'std::fmt' needs no escape.
    let out = tool("ct-search")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--grep",
            "std::fmt",
            "--quiet",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    // A missing payload file is a usage error, not a clean negative.
    let out = tool("ct-search")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--grep",
            "file:/no/such/payload",
            "--quiet",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 2, "{}", stderr(&out));
}

#[test]
fn mode_literal_matches_verbatim_code_that_promotion_would_break() {
    let dir = scratch("mode");
    std::fs::write(dir.join("a.rs"), "    todo!(\"wire this\");\n").unwrap();

    // Promotion sees '(' '!' and tries regex: the anchor misses its own text.
    let promoted = tool("ct-search")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--grep",
            "todo!(\"wire this\")",
            "--quiet",
        ])
        .output()
        .unwrap();
    assert_eq!(
        code(&promoted),
        1,
        "promotion should miss: {}",
        stderr(&promoted)
    );

    // --mode literal pins it: the same text matches.
    let literal = tool("ct-search")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--grep",
            "todo!(\"wire this\")",
            "--mode",
            "literal",
            "--quiet",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&literal), 0, "{}", stderr(&literal));
}

#[test]
fn block_grep_counts_occurrences_and_reports_nearest_miss_on_detail() {
    let dir = scratch("block-grep");
    std::fs::write(dir.join("ast.rs"), SAMPLE).unwrap();
    std::fs::write(
        dir.join("hit.block"),
        "    match v {\n        Value::U64(v) => v.to_string(),\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("miss.block"),
        "    match v {\n        Value::F64(v) => v.to_string(),\n",
    )
    .unwrap();

    let hit = tool("ct-search")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--name",
            "*.rs",
            "--grep",
            &format!("file:{}", dir.join("hit.block").display()),
            "--detail",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&hit), 0, "{}", stderr(&hit));
    assert!(
        stdout(&hit).contains("ast.rs:6:"),
        "block start line: {}",
        stdout(&hit)
    );

    let miss = tool("ct-search")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--name",
            "*.rs",
            "--grep",
            &format!("file:{}", dir.join("miss.block").display()),
            "--detail",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&miss), 1);
    let diag = stderr(&miss);
    assert!(diag.contains("nearest miss"), "{diag}");
    assert!(diag.contains("diverges at its line 2"), "{diag}");
    assert!(diag.contains("Value::U64(v) => v.to_string(),"), "{diag}");
}

#[test]
fn block_view_shows_the_region_and_misses_cleanly() {
    let dir = scratch("block-view");
    let file = dir.join("ast.rs");
    std::fs::write(&file, SAMPLE).unwrap();
    std::fs::write(dir.join("b.block"), "enum Value {\n    U64(u64),\n}\n").unwrap();

    let out = tool("ct-view")
        .args([
            file.to_str().unwrap(),
            "--match",
            &format!("file:{}", dir.join("b.block").display()),
            "--context",
            "0",
            "--plain",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "enum Value {\n    U64(u64),\n}\n");

    std::fs::write(dir.join("no.block"), "enum Value {\n    I64(u64),\n}\n").unwrap();
    let out = tool("ct-view")
        .args([
            file.to_str().unwrap(),
            "--match",
            &format!("file:{}", dir.join("no.block").display()),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("nearest miss"), "{}", stderr(&out));
}

#[test]
fn argv_block_edit_replaces_and_empty_replacement_deletes() {
    let dir = scratch("block-edit");
    let file = dir.join("ast.rs");
    std::fs::write(&file, SAMPLE).unwrap();
    std::fs::write(dir.join("find.block"), "enum Value {\n    U64(u64),\n}\n").unwrap();
    std::fs::write(
        dir.join("repl.block"),
        "enum Value {\n    U64(u64),\n    I64(i64),\n}\n",
    )
    .unwrap();

    let out = tool("ct-edit")
        .args([
            "--base",
            file.to_str().unwrap(),
            "--find",
            &format!("file:{}", dir.join("find.block").display()),
            "--replace",
            &format!("file:{}", dir.join("repl.block").display()),
            "--expect",
            "=1",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let now = std::fs::read_to_string(&file).unwrap();
    assert!(now.contains("    I64(i64),\n}"), "{now}");

    // An empty replace payload deletes a (multi-line) block's lines entirely.
    // A single-line find keeps per-line substring semantics, so deletion is a
    // block affair by design.
    std::fs::write(dir.join("kill.block"), "    U64(u64),\n    I64(i64),\n").unwrap();
    std::fs::write(dir.join("empty.block"), "").unwrap();
    let out = tool("ct-edit")
        .args([
            "--base",
            file.to_str().unwrap(),
            "--find",
            &format!("file:{}", dir.join("kill.block").display()),
            "--replace",
            &format!("file:{}", dir.join("empty.block").display()),
            "--expect",
            "=1",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        SAMPLE.replace("    U64(u64),\n", "")
    );
}

/// Write the canonical two-edit script (variant + match arm) for `dir`.
fn write_script(dir: &Path, second_find: &str) -> PathBuf {
    let script = dir.join("edits.ctb");
    std::fs::write(
        &script,
        format!(
            "#% edit expect=\"=1\"\n#% find\n    U64(u64),\n#% replace\n    U64(u64),\n    I64(i64),\n#% edit expect=\"=1\"\n#% find\n{second_find}\n#% replace\n        Value::U64(v) => v.to_string(),\n        Value::I64(v) => v.to_string(),\n#% end\n"
        ),
    )
    .unwrap();
    script
}

#[test]
fn script_batch_applies_atomically_and_dry_run_writes_nothing() {
    let dir = scratch("script-ok");
    let file = dir.join("ast.rs");
    std::fs::write(&file, SAMPLE).unwrap();
    let script = write_script(&dir, "        Value::U64(v) => v.to_string(),");

    let dry = tool("ct-edit")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--name",
            "*.rs",
            "--script",
            script.to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&dry), 0, "{}", stderr(&dry));
    assert!(stdout(&dry).contains("dry-run, not written"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), SAMPLE);

    let apply = tool("ct-edit")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--name",
            "*.rs",
            "--script",
            script.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&apply), 0, "{}", stderr(&apply));
    let v: serde_json::Value = serde_json::from_str(&stdout(&apply)).unwrap();
    assert_eq!(v["verdict"], "SUCCESS");
    assert_eq!(v["applied"], true);
    assert_eq!(v["edits"].as_array().unwrap().len(), 2);
    // Per-edit default expect inside a script is "=1".
    assert_eq!(v["edits"][0]["expect"], "=1");
    let now = std::fs::read_to_string(&file).unwrap();
    assert!(now.contains("    I64(i64),"), "{now}");
    assert!(now.contains("Value::I64(v) => v.to_string(),"), "{now}");
}

#[test]
fn script_failure_means_zero_writes_and_a_nearest_miss() {
    let dir = scratch("script-fail");
    let file = dir.join("ast.rs");
    std::fs::write(&file, SAMPLE).unwrap();
    // The second edit's anchor diverges from the source (F64 vs U64), and is
    // two lines so it matches as a block.
    let script = dir.join("bad.ctb");
    std::fs::write(
        &script,
        "#% edit\n#% find\n    U64(u64),\n#% replace\n    U64(u64),\n    I64(i64),\n#% edit\n#% find\n    match v {\n        Value::F64(v) => v.to_string(),\n#% replace\nx\n#% end\n",
    )
    .unwrap();

    let out = tool("ct-edit")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--name",
            "*.rs",
            "--script",
            script.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["verdict"], "ERROR");
    assert_eq!(v["applied"], false);
    assert_eq!(v["edits"][0]["verdict"], "SUCCESS");
    assert_eq!(v["edits"][1]["verdict"], "ERROR");
    let miss = &v["edits"][1]["nearest_miss"];
    assert_eq!(miss["first_diverging_line"], 2, "{miss}");
    // Atomic: edit 1 passed, yet nothing was written.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), SAMPLE);
}

#[test]
fn script_write_preflight_refuses_a_readonly_target_with_zero_writes() {
    let dir = scratch("script-preflight");
    let ok = dir.join("a.rs");
    let ro = dir.join("b.rs");
    // A prior run may have left b.rs read-only (scratch dirs persist, and the
    // restore at the end is skipped on a panic), so clear it before setup —
    // this makes the test self-healing rather than wedged after one failure.
    make_writable(&ok);
    make_writable(&ro);
    std::fs::write(&ok, "alpha()\n").unwrap();
    std::fs::write(&ro, "beta()\n").unwrap();
    let mut perms = std::fs::metadata(&ro).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&ro, perms).unwrap();

    let script = dir.join("two.ctb");
    std::fs::write(
        &script,
        "#% edit file=a.rs\n#% find\nalpha()\n#% replace\nALPHA()\n#% edit file=b.rs\n#% find\nbeta()\n#% replace\nBETA()\n#% end\n",
    )
    .unwrap();

    let out = tool("ct-edit")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--name",
            "*.rs",
            "--script",
            script.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    // Pre-flight fails the whole batch before any write begins.
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("pre-flight"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&ok).unwrap(), "alpha()\n");
    assert_eq!(std::fs::read_to_string(&ro).unwrap(), "beta()\n");

    // Restore writability so reruns can overwrite the scratch files (and so a
    // leftover read-only file doesn't wedge an unrelated test). Cross-platform.
    make_writable(&ro);
}

#[test]
fn cascade_lets_later_edits_see_earlier_output_and_no_cascade_rejects_overlap() {
    let dir = scratch("script-cascade");
    let file = dir.join("c.rs");
    std::fs::write(&file, "base()\n").unwrap();
    let script = dir.join("chain.ctb");
    std::fs::write(
        &script,
        "#% edit\n#% find\nbase()\n#% replace\nbase()\nadded()\n#% edit\n#% find\nadded()\n#% replace\nadded(1)\n#% end\n",
    )
    .unwrap();

    // Cascade (default): edit 2 matches the line edit 1 just added.
    let out = tool("ct-edit")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--name",
            "*.rs",
            "--script",
            script.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "base()\nadded(1)\n"
    );

    // --no-cascade: the same chain is invalid — edit 2 matches nothing in
    // pristine content, so the batch fails with zero writes.
    std::fs::write(&file, "base()\n").unwrap();
    let out = tool("ct-edit")
        .args([
            "--base",
            dir.to_str().unwrap(),
            "--name",
            "*.rs",
            "--script",
            script.to_str().unwrap(),
            "--no-cascade",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "base()\n");

    // Overlapping edits under --no-cascade are a usage error.
    std::fs::write(&file, "a\nb\nc\n").unwrap();
    let overlap = dir.join("overlap.ctb");
    std::fs::write(
        &overlap,
        "#% edit\n#% find\na\nb\n#% replace\nA\n#% edit\n#% find\nb\nc\n#% replace\nC\n#% end\n",
    )
    .unwrap();
    let out = tool("ct-edit")
        .args([
            "--base",
            file.to_str().unwrap(),
            "--script",
            overlap.to_str().unwrap(),
            "--no-cascade",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("overlap"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\nb\nc\n");
}

#[test]
fn block_find_anchor_with_a_trailing_blank_line_still_matches() {
    // The reported failure: a 2-line anchor file ending in a trailing blank
    // line (an editor's final newline over an already newline-terminated body)
    // must match the same two consecutive source lines, not parse as a 3-line
    // block whose phantom empty 3rd line diverges from the source below it.
    let dir = scratch("trailing-blank");
    let file = dir.join("game.rs");
    std::fs::write(
        &file,
        "/// Place rare derelict hauler cars.\nfn spawn_cars(\n    settings: Res<Settings>,\n) {\n}\n",
    )
    .unwrap();
    // Anchor body is two lines, then an extra blank line (two trailing \n).
    std::fs::write(
        dir.join("anchor.block"),
        "/// Place rare derelict hauler cars.\nfn spawn_cars(\n\n",
    )
    .unwrap();

    let out = tool("ct-edit")
        .args([
            "--base",
            file.to_str().unwrap(),
            "--find",
            &format!("file:{}", dir.join("anchor.block").display()),
            "--replace",
            "text:fn spawn_cars(",
            "--expect",
            "1",
            "--dry-run",
        ])
        .output()
        .unwrap();
    // One replacement, no phantom-line divergence.
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("nearest miss"),
        "should not miss: {}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("1 replacement(s)"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn block_find_anchor_with_crlf_matches_lf_source() {
    // An anchor file saved by a Windows editor (CRLF line endings) must match
    // LF source: the trailing \r on each line is normalized away.
    let dir = scratch("crlf-anchor");
    let file = dir.join("ast.rs");
    std::fs::write(&file, SAMPLE).unwrap();
    std::fs::write(
        dir.join("anchor.block"),
        "enum Value {\r\n    U64(u64),\r\n}\r\n",
    )
    .unwrap();

    let out = tool("ct-edit")
        .args([
            "--base",
            file.to_str().unwrap(),
            "--find",
            &format!("file:{}", dir.join("anchor.block").display()),
            "--replace",
            "text:enum V {}",
            "--expect",
            "1",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        stdout(&out).contains("1 replacement(s)"),
        "CRLF anchor should match LF source: {} / {}",
        stdout(&out),
        stderr(&out)
    );
}

#[test]
fn block_miss_is_self_diagnosing_about_a_stray_empty_line() {
    // A block anchor whose diverging line is empty should surface the parsed
    // line count and a blank-line note — the diagnostic the original report
    // asked for. The anchor here has an interior empty line (line 2) that
    // diverges from the real source, so the trailing-blank trim leaves it.
    let dir = scratch("miss-diag");
    let file = dir.join("ast.rs");
    std::fs::write(&file, SAMPLE).unwrap();

    let anchor = dir.join("blank-mid.block");
    std::fs::write(&anchor, "enum Value {\n\nnope\n").unwrap();
    let out = tool("ct-edit")
        .args([
            "--base",
            file.to_str().unwrap(),
            "--find",
            &format!("file:{}", anchor.display()),
            "--replace",
            "text:x",
            "--expect",
            "1",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let diag = stderr(&out);
    assert!(diag.contains("nearest miss"), "{diag}");
    assert!(diag.contains("of 3"), "names the parsed block size: {diag}");
    assert!(diag.contains("note:"), "flags the empty line: {diag}");
    assert!(diag.contains("(empty line)"), "renders the blank: {diag}");
}

#[test]
fn squeeze_blank_lets_an_anchor_tolerate_blank_line_drift() {
    let dir = scratch("squeeze-argv");
    let file = dir.join("game.rs");
    // Source has TWO blank lines between the two anchored lines.
    std::fs::write(&file, "fn spawn() {\n\n\n    body();\n}\n").unwrap();
    // Anchor carries only ONE blank line between them.
    std::fs::write(dir.join("anchor.block"), "fn spawn() {\n\n    body();\n").unwrap();
    let find = format!("file:{}", dir.join("anchor.block").display());

    // Without --squeeze-blank the blank-count mismatch misses.
    let exact = tool("ct-edit")
        .args([
            "--base",
            file.to_str().unwrap(),
            "--find",
            &find,
            "--replace",
            "text:fn spawn() {}",
            "--expect",
            "1",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&exact), 1, "exact should miss: {}", stdout(&exact));
    assert!(
        stderr(&exact).contains("nearest miss"),
        "{}",
        stderr(&exact)
    );

    // With --squeeze-blank the single anchor blank absorbs the two source
    // blanks and the block matches.
    let squeezed = tool("ct-edit")
        .args([
            "--base",
            file.to_str().unwrap(),
            "--find",
            &find,
            "--replace",
            "text:fn spawn() {}",
            "--squeeze-blank",
            "--expect",
            "1",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&squeezed), 0, "{}", stderr(&squeezed));
    assert!(
        stdout(&squeezed).contains("1 replacement(s)"),
        "{}",
        stdout(&squeezed)
    );
    // The whole matched span (incl. both blanks) is replaced by the one line.
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn spawn() {}\n}\n"
    );
}

#[test]
fn script_squeeze_attribute_enables_blank_tolerant_matching() {
    let dir = scratch("squeeze-script");
    let file = dir.join("c.rs");
    std::fs::write(&file, "open()\n\n\n\nclose()\n").unwrap();
    let script = dir.join("sq.ctb");
    // The anchor has one blank line; the source has three. squeeze=true makes
    // the edit match; the default (exact) would fail the atomic batch.
    std::fs::write(
        &script,
        "#% edit expect=\"=1\" squeeze=true\n#% find\nopen()\n\nclose()\n#% replace\ndone()\n#% end\n",
    )
    .unwrap();

    let out = tool("ct-edit")
        .args([
            "--base",
            file.to_str().unwrap(),
            "--script",
            script.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "done()\n");

    // An invalid squeeze value is a usage error before any write.
    std::fs::write(&file, "open()\n\n\n\nclose()\n").unwrap();
    let bad = dir.join("bad.ctb");
    std::fs::write(
        &bad,
        "#% edit squeeze=maybe\n#% find\nopen()\n#% replace\ndone()\n#% end\n",
    )
    .unwrap();
    let out = tool("ct-edit")
        .args([
            "--base",
            file.to_str().unwrap(),
            "--script",
            bad.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("squeeze"), "{}", stderr(&out));
}

#[test]
fn patch_file_value_is_a_verbatim_string_and_each_expands_file_items() {
    let dir = scratch("patch-each");
    let cfg = dir.join("cfg.json");
    std::fs::write(&cfg, "{\n  \"notes\": \"old\"\n}\n").unwrap();
    // Looks like JSON, but a file: value must stay a verbatim string.
    std::fs::write(dir.join("notes.txt"), "[1,2]\nline two\n").unwrap();

    let out = tool("ct-patch")
        .args([
            "--base",
            cfg.to_str().unwrap(),
            "--set",
            &format!("notes=file:{}", dir.join("notes.txt").display()),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(v["notes"], "[1,2]\nline two\n");

    // ct-each: a file: item expands to the file's non-empty lines.
    std::fs::write(dir.join("items.txt"), "alpha\n\nbeta\n").unwrap();
    let out = tool("ct-each")
        .args([
            "--items",
            &format!("file:{}", dir.join("items.txt").display()),
            "--quiet",
            "--emit",
            "{TOTAL} item(s)",
        ])
        .arg("--")
        .args(common::exit_ok(&dir))
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("2 item(s)"), "{}", stdout(&out));
}

#[test]
fn ct_test_stdin_accepts_a_file_payload() {
    let dir = scratch("test-stdin");
    std::fs::write(dir.join("input.txt"), "first\nMARKER here\nlast\n").unwrap();

    let cat = common::cat_stdin();
    let out = tool("ct-test")
        .args(["--cmd", &cat[0]])
        .args([
            "--stdin",
            &format!("file:{}", dir.join("input.txt").display()),
            "--ok-match",
            "MARKER",
            "--quiet",
            "--emit",
            "{RESULT}",
        ])
        .arg("--")
        .args(&cat[1..])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("SUCCESS"), "{}", stdout(&out));
}
