// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! End-to-end guards on OKF support: the `ct-okf` tool's read verbs (validate,
//! list, show, links) and authoring round-trip (new/index/log/set), the
//! `okf` built-in check run in-process, and the additive OKF-awareness of
//! `ct-search`/`ct-tree`/`ct-view`/`ct-outline`. Binaries are driven through the
//! paths Cargo exports (`CARGO_BIN_EXE_*`); the built-in check through the lib.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use coding_tools::okf;
use coding_tools::rules::ProbeOutcome;

/// A unique, overwrite-friendly scratch dir under `target/` (never removed).
fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/okf")
        .join(tag);
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

fn write(path: &Path, body: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

/// A small conformant bundle: two BigQuery-table concepts (one PII-tagged) plus
/// a reserved index.md. Returns the bundle root.
fn bundle(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    // Start clean so re-runs are deterministic.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tables")).unwrap();
    write(
        &dir.join("tables/customers.md"),
        "---\ntype: BigQuery Table\ntitle: Customers\ntags: [core, pii]\n---\n# Schema\nid, name\n",
    );
    write(
        &dir.join("tables/orders.md"),
        "---\ntype: BigQuery Table\ntitle: Orders\ntags: [core]\n---\n# Schema\nid, total\n",
    );
    write(
        &dir.join("index.md"),
        "# Index\n\n* [Customers](tables/customers.md)\n",
    );
    dir
}

fn ct_okf(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ct-okf"));
    c.arg("--base").arg(dir);
    c
}

// ----- the okf built-in check (in-process) --------------------------------------------

#[test]
fn builtin_check_holds_for_a_conformant_bundle() {
    let dir = bundle("check-holds");
    let (o, reason, _) = okf::check(&["--base".into(), ".".into()], &dir, None);
    assert_eq!(o, ProbeOutcome::Holds, "{reason}");
    assert!(reason.contains("conform"), "{reason:?}");
}

#[test]
fn builtin_check_violated_when_a_concept_lacks_type() {
    let dir = bundle("check-notype");
    write(
        &dir.join("tables/orphan.md"),
        "---\ntitle: Orphan\n---\nbody\n",
    );
    let (o, _reason, report) = okf::check(&["--base".into(), ".".into()], &dir, None);
    assert_eq!(o, ProbeOutcome::Violated);
    assert!(report.contains("orphan.md"), "{report:?}");
    assert!(report.contains("type"), "{report:?}");
}

#[test]
fn builtin_check_strict_flags_broken_links() {
    let dir = bundle("check-links");
    write(
        &dir.join("tables/with_link.md"),
        "---\ntype: Note\n---\nsee [gone](/tables/gone.md)\n",
    );
    // Without --strict the bundle conforms (a broken link is tolerated).
    assert_eq!(
        okf::check(&["--base".into(), ".".into()], &dir, None).0,
        ProbeOutcome::Holds
    );
    // With --strict the missing target is a violation.
    let (o, _r, report) = okf::check(
        &["--base".into(), ".".into(), "--strict".into()],
        &dir,
        None,
    );
    assert_eq!(o, ProbeOutcome::Violated);
    assert!(report.contains("gone.md"), "{report:?}");
}

#[test]
fn builtin_check_broken_for_a_missing_base() {
    let dir = scratch("check-missing");
    let (o, reason, _) = okf::check(&["--base".into(), "nope".into()], &dir, None);
    assert_eq!(o, ProbeOutcome::Broken);
    assert!(reason.contains("does not exist"), "{reason:?}");
}

// ----- ct-okf read verbs --------------------------------------------------------------

#[test]
fn validate_passes_then_fails_on_a_nonconformant_concept() {
    let dir = bundle("validate");
    let ok = ct_okf(&dir).arg("--validate").output().unwrap();
    assert_eq!(code(&ok), 0, "stderr: {}", stderr(&ok));

    write(&dir.join("tables/orphan.md"), "no frontmatter here\n");
    let bad = ct_okf(&dir).arg("--validate").output().unwrap();
    assert_eq!(code(&bad), 1, "stdout: {}", stdout(&bad));
    assert!(stdout(&bad).contains("ERROR"), "{}", stdout(&bad));
}

#[test]
fn list_json_filters_by_type_and_tag() {
    let dir = bundle("list");
    let out = ct_okf(&dir)
        .args(["--list", "--okf-tag", "pii", "--json"])
        // note: list uses --tag, not --okf-tag; correct below
        .output()
        .unwrap();
    // The wrong flag is a usage error, proving the flag surface is real.
    assert_eq!(code(&out), 2);

    let out = ct_okf(&dir)
        .args(["--list", "--tag", "pii", "--json"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["concepts"][0]["title"], "Customers");
    assert_eq!(v["concepts"][0]["type"], "BigQuery Table");

    // Filter by type matches both tables.
    let out = ct_okf(&dir)
        .args(["--list", "--type", "BigQuery Table", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["count"], 2);
}

#[test]
fn show_reports_one_concepts_frontmatter() {
    let dir = bundle("show");
    let path = dir.join("tables/customers.md");
    let out = ct_okf(&dir)
        .args(["--show".as_ref(), path.as_os_str(), "--json".as_ref()])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["type"], "BigQuery Table");
    assert_eq!(v["tags"][1], "pii");
}

#[test]
fn links_verdict_is_clean_then_broken() {
    let dir = bundle("links");
    assert_eq!(code(&ct_okf(&dir).arg("--links").output().unwrap()), 0);
    write(
        &dir.join("tables/bad.md"),
        "---\ntype: Note\n---\n[x](/tables/missing.md)\n",
    );
    let out = ct_okf(&dir).arg("--links").output().unwrap();
    assert_eq!(code(&out), 1);
    assert!(stdout(&out).contains("missing.md"), "{}", stdout(&out));
}

// ----- ct-okf --script (atomic batch) -------------------------------------------------

const SCRIPT: &str = "\
#% new file=tables/customers.md type=\"BigQuery Table\" title=Customers
#% description
The customers dimension.
#% tags
core
pii
#% new file=tables/orders.md type=\"BigQuery Table\" title=Orders
#% index base=tables
#% set file=tables/customers.md field=resource value=bq://proj.ds.customers
#% log kind=Creation
#% message
scaffolded customers + orders
";

#[test]
fn script_dry_run_writes_nothing_then_applies_atomically() {
    let dir = scratch("script-apply");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("batch.ctb");
    write(&script, SCRIPT);

    // --dry-run plans but writes nothing.
    let dry = ct_okf(&dir)
        .args([
            "--script".as_ref(),
            script.as_os_str(),
            "--dry-run".as_ref(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&dry), 0, "stderr: {}", stderr(&dry));
    assert!(stdout(&dry).contains("would new"), "{}", stdout(&dry));
    assert!(
        !dir.join("tables/customers.md").exists(),
        "dry-run wrote a file"
    );

    // Apply: the cascade lands (index sees both concepts; set enriched customers).
    let run = ct_okf(&dir)
        .args(["--script".as_ref(), script.as_os_str()])
        .output()
        .unwrap();
    assert_eq!(code(&run), 0, "stderr: {}", stderr(&run));
    let customers = std::fs::read_to_string(dir.join("tables/customers.md")).unwrap();
    assert!(
        customers.contains("resource: bq://proj.ds.customers"),
        "{customers}"
    );
    assert!(customers.contains("tags: [core, pii]"), "{customers}");
    let index = std::fs::read_to_string(dir.join("tables/index.md")).unwrap();
    assert!(
        index.contains("[Customers](customers.md) - The customers dimension."),
        "{index}"
    );
    assert!(index.contains("[Orders](orders.md)"), "{index}");
    let log = std::fs::read_to_string(dir.join("log.md")).unwrap();
    assert!(
        log.contains("**Creation**: scaffolded customers + orders"),
        "{log}"
    );
    // The result is conformant.
    assert_eq!(code(&ct_okf(&dir).arg("--validate").output().unwrap()), 0);
}

#[test]
fn script_is_atomic_a_failing_op_writes_nothing() {
    let dir = scratch("script-atomic");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tables")).unwrap();
    // Pre-existing concept the batch will try to clobber on its second op.
    write(
        &dir.join("tables/customers.md"),
        "---\ntype: Note\ntitle: Existing\n---\nbody\n",
    );
    let script = dir.join("batch.ctb");
    write(
        &script,
        "#% new file=tables/fresh.md type=Note title=Fresh\n\
         #% new file=tables/customers.md type=Note title=Dup\n\
         #% log\n#% message\nshould not run\n",
    );

    let out = ct_okf(&dir)
        .args(["--script".as_ref(), script.as_os_str()])
        .output()
        .unwrap();
    assert_eq!(code(&out), 2, "stdout: {}", stdout(&out));
    assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));
    // The first op's file must NOT have been written, and no log created.
    assert!(
        !dir.join("tables/fresh.md").exists(),
        "atomic guarantee broken"
    );
    assert!(!dir.join("log.md").exists(), "atomic guarantee broken");
}

// ----- ct-okf authoring round-trip ----------------------------------------------------

#[test]
fn authoring_new_index_log_set_then_revalidate() {
    let dir = bundle("author");
    let concept = dir.join("tables/products.md");

    // --new scaffolds a conformant concept.
    let out = ct_okf(&dir)
        .args([
            "--new".as_ref(),
            concept.as_os_str(),
            "--type".as_ref(),
            "BigQuery Table".as_ref(),
            "--title".as_ref(),
            "Products".as_ref(),
            "--tag".as_ref(),
            "core".as_ref(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(concept.is_file());

    // --new refuses to clobber an existing file.
    let clob = ct_okf(&dir)
        .args([
            "--new".as_ref(),
            concept.as_os_str(),
            "--type".as_ref(),
            "X".as_ref(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&clob), 2);
    assert!(stderr(&clob).contains("refusing to overwrite"));

    // --set updates a frontmatter field in place; --show confirms it.
    let setc = ct_okf(&dir)
        .args([
            "--set".as_ref(),
            "timestamp=2026-06-27".as_ref(),
            "--file".as_ref(),
            concept.as_os_str(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&setc), 0, "stderr: {}", stderr(&setc));
    let shown = ct_okf(&dir)
        .args(["--show".as_ref(), concept.as_os_str()])
        .output()
        .unwrap();
    assert!(stdout(&shown).contains("timestamp: 2026-06-27"));

    // --index regenerates the directory listing including the new concept.
    let tables = dir.join("tables");
    let idx = Command::new(env!("CARGO_BIN_EXE_ct-okf"))
        .arg("--base")
        .arg(&tables)
        .arg("--index")
        .output()
        .unwrap();
    assert_eq!(code(&idx), 0, "stderr: {}", stderr(&idx));
    let index_md = std::fs::read_to_string(tables.join("index.md")).unwrap();
    assert!(index_md.contains("[Products](products.md)"), "{index_md}");

    // --log prepends a dated, labelled entry.
    let logc = ct_okf(&dir)
        .args(["--log", "Added products", "--log-kind", "Creation"])
        .output()
        .unwrap();
    assert_eq!(code(&logc), 0, "stderr: {}", stderr(&logc));
    let log_md = std::fs::read_to_string(dir.join("log.md")).unwrap();
    assert!(log_md.contains("**Creation**: Added products"), "{log_md}");

    // The bundle still validates after all the authoring.
    assert_eq!(code(&ct_okf(&dir).arg("--validate").output().unwrap()), 0);
}

// ----- okf as a recorded invariant (ct rules / ct check) ------------------------------

#[test]
fn okf_check_records_and_enforces_through_the_store() {
    // A project root holding a bundle; the store lives at <root>/.ct.
    let root = scratch("invariant");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".ct")).unwrap();
    write(
        &root.join("bundle/notes.md"),
        "---\ntype: Note\ntitle: N\n---\nbody\n",
    );

    // Record the conformance invariant: the probe runs NOW and must hold.
    let add = Command::new(env!("CARGO_BIN_EXE_ct-rules"))
        .current_dir(&root)
        .args([
            "--add",
            "okf-conformant",
            "--question",
            "Is the bundle OKF-conformant?",
            "--",
            "okf",
            "--base",
            "bundle",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&add), 0, "stderr: {}", stderr(&add));

    // ct check enforces it: green while the bundle conforms.
    let ok = Command::new(env!("CARGO_BIN_EXE_ct-check"))
        .current_dir(&root)
        .arg("--quiet")
        .output()
        .unwrap();
    assert_eq!(code(&ok), 0, "stderr: {}", stderr(&ok));

    // Break conformance: a concept without a type. ct check now fails.
    write(
        &root.join("bundle/orphan.md"),
        "---\ntitle: no type\n---\nx\n",
    );
    let bad = Command::new(env!("CARGO_BIN_EXE_ct-check"))
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(code(&bad), 1, "stdout: {}", stdout(&bad));
}

// ----- additive OKF-awareness of the file/structure tools -----------------------------

#[test]
fn search_filters_by_okf_tag_and_type() {
    let dir = bundle("aware-search");
    // Only the PII-tagged concept matches.
    let out = Command::new(env!("CARGO_BIN_EXE_ct-search"))
        .args([
            "--base".as_ref(),
            dir.as_os_str(),
            "--okf-tag".as_ref(),
            "pii".as_ref(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("customers.md"), "{text}");
    assert!(!text.contains("orders.md"), "{text}");

    // A type that matches nothing is a clean negative (exit 1).
    let none = Command::new(env!("CARGO_BIN_EXE_ct-search"))
        .args([
            "--base".as_ref(),
            dir.as_os_str(),
            "--okf-type".as_ref(),
            "Playbook".as_ref(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&none), 1);
}

#[test]
fn tree_groups_by_okf_type() {
    let dir = bundle("aware-tree");
    let out = Command::new(env!("CARGO_BIN_EXE_ct-tree"))
        .args([
            "--base".as_ref(),
            dir.as_os_str(),
            "--summary".as_ref(),
            "--group".as_ref(),
            "okf-type".as_ref(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("BigQuery Table"), "{text}");
    // index.md has no type -> the "(none)" bucket.
    assert!(text.contains("(none)"), "{text}");
}

#[test]
fn view_isolates_frontmatter_both_ways() {
    let dir = bundle("aware-view");
    let path = dir.join("tables/customers.md");
    let fm = Command::new(env!("CARGO_BIN_EXE_ct-view"))
        .args([
            path.as_os_str(),
            "--frontmatter".as_ref(),
            "--plain".as_ref(),
        ])
        .output()
        .unwrap();
    let fm_text = stdout(&fm);
    assert!(fm_text.contains("type: BigQuery Table"), "{fm_text}");
    assert!(!fm_text.contains("# Schema"), "{fm_text}");

    let body = Command::new(env!("CARGO_BIN_EXE_ct-view"))
        .args([
            path.as_os_str(),
            "--no-frontmatter".as_ref(),
            "--plain".as_ref(),
        ])
        .output()
        .unwrap();
    let body_text = stdout(&body);
    assert!(body_text.contains("# Schema"), "{body_text}");
    assert!(!body_text.contains("type: BigQuery Table"), "{body_text}");
}

#[test]
fn outline_frontmatter_is_opt_in() {
    let dir = bundle("aware-outline");
    let path = dir.join("tables/customers.md");

    // Default outline shows no synthetic meta entries.
    let plain = Command::new(env!("CARGO_BIN_EXE_ct-outline"))
        .args(["--base".as_ref(), path.as_os_str()])
        .output()
        .unwrap();
    assert!(!stdout(&plain).contains("meta:"), "{}", stdout(&plain));

    // --frontmatter surfaces them, and --kind narrows to one.
    let meta = Command::new(env!("CARGO_BIN_EXE_ct-outline"))
        .args([
            "--base".as_ref(),
            path.as_os_str(),
            "--frontmatter".as_ref(),
            "--kind".as_ref(),
            "meta:type".as_ref(),
            "--flat".as_ref(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&meta), 0, "stderr: {}", stderr(&meta));
    let text = stdout(&meta);
    assert!(text.contains("meta:type:BigQuery Table"), "{text}");
}
