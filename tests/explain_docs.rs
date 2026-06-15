// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on the embedded `--explain` payloads: the same `docs/explain` files
//! the binaries `include_str!` must stay parseable and well-formed, so a typo
//! in a hand-authored tool definition fails the build rather than shipping. The
//! umbrella `ct` additionally bundles each leaf tool's definition under `tools`,
//! which must not drift from the standalone leaf files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Tools whose canonical docs live in `docs/explain/`.
const TOOLS: &[&str] = &[
    "ct",
    "ct-search",
    "ct-view",
    "ct-tree",
    "ct-edit",
    "ct-patch",
    "ct-test",
    "ct-each",
    "ct-outline",
    "ct-rules",
    "ct-check",
    "ct-await",
];

/// Leaf tools whose definitions the `ct` manifest must mirror exactly.
const LEAVES: &[&str] = &[
    "ct-search",
    "ct-view",
    "ct-tree",
    "ct-edit",
    "ct-patch",
    "ct-test",
    "ct-each",
    "ct-outline",
    "ct-rules",
    "ct-check",
    "ct-await",
];

/// Built-in checks — reserved probe heads run in-process by the rule layer, not
/// standalone tools — that the `ct` manifest advertises under `builtin_checks`.
const BUILTIN_CHECKS: &[&str] = &["deps", "mods"];

fn explain_path(tool: &str, ext: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs/explain")
        .join(format!("{tool}.{ext}"))
}

fn read_json(tool: &str) -> serde_json::Value {
    let path = explain_path(tool, "json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn json_payloads_are_well_formed_tool_definitions() {
    for tool in TOOLS {
        let v = read_json(tool);
        assert_eq!(v["name"], *tool, "name must equal the tool name");
        assert!(v["description"].is_string(), "description must be a string");
        assert_eq!(v["input_schema"]["type"], "object");
        assert!(
            v["input_schema"]["properties"].is_object(),
            "input_schema.properties must be an object"
        );
    }
}

#[test]
fn md_payloads_lead_with_the_tool_heading() {
    for tool in TOOLS {
        let path = explain_path(tool, "md");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(
            text.starts_with(&format!("# {tool}")),
            "{} should begin with an H1 naming the tool",
            path.display()
        );
    }
}

#[test]
fn ct_manifest_bundles_each_leaf_definition_verbatim() {
    let manifest = read_json("ct");
    let tools = manifest["tools"]
        .as_array()
        .expect("ct.json must carry a `tools` array for agent hoisting");

    for leaf in LEAVES {
        let bundled = tools
            .iter()
            .find(|t| t["name"] == *leaf)
            .unwrap_or_else(|| panic!("ct manifest is missing the '{leaf}' tool definition"));
        let standalone = read_json(leaf);
        assert_eq!(
            *bundled, standalone,
            "ct manifest's '{leaf}' definition has drifted from docs/explain/{leaf}.json"
        );
    }
}

/// Schema properties that are positional or trailing-argv inputs, not `--flags`
/// — excluded when reconciling a tool's schema against its clap grammar.
fn non_flag_props(tool: &str) -> &'static [&'static str] {
    match tool {
        "ct-view" => &["path"],    // positional <PATH>
        "ct-test" => &["args"],    // positional [ARGS] (the command argv)
        "ct-each" => &["command"], // trailing argv after `--`
        "ct-rules" => &["probe"],  // trailing argv after `--`
        "ct-await" => &["probe"],  // trailing argv after `--`
        _ => &[],
    }
}

/// Collapse a schema `type` to the kind clap can actually witness: every
/// single-value type (`string` / `number` / `integer`) is one scalar bucket
/// (clap introspection exposes no way to recover a value's target type),
/// leaving `boolean` / `array` / `string`.
fn normalize_kind(ty: &str) -> String {
    if matches!(ty, "number" | "integer") { "string" } else { ty }.to_string()
}

/// The unified schema-drift guard: every tool's `docs/explain/<tool>.json` must
/// enumerate exactly the flags its clap grammar accepts, with matching kinds.
/// Leaf grammars come from the lib-hosted `coding_tools::cli`; built-in checks
/// from their `check_flags`. Meta flags (`--help`/`--version`/`--explain`) are
/// dropped by the grammar reader; positional/trailing-argv inputs (see
/// `non_flag_props`) are dropped from the schema side. A flag added to or
/// removed from a clap struct, or a kind change (bool ↔ value, scalar ↔ array),
/// without the matching doc edit fails here.
#[test]
fn schema_matches_clap_grammar() {
    let mut grammars: Vec<(&str, Vec<(String, &'static str)>)> = coding_tools::cli::flags();
    grammars.push(("deps", coding_tools::deps::check_flags()));
    grammars.push(("mods", coding_tools::modgraph::check_flags()));

    for (tool, flags) in grammars {
        let cli: BTreeMap<String, String> =
            flags.iter().map(|(n, k)| (n.clone(), normalize_kind(k))).collect();
        let skip = non_flag_props(tool);
        let v = read_json(tool);
        let doc: BTreeMap<String, String> = v["input_schema"]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{tool}.json input_schema.properties must be an object"))
            .iter()
            .filter(|(k, _)| !skip.contains(&k.as_str()))
            .map(|(k, spec)| {
                let ty = spec["type"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{tool}.json: property {k} needs a string type"));
                (k.clone(), normalize_kind(ty))
            })
            .collect();
        assert_eq!(
            doc, cli,
            "{tool}: docs/explain/{tool}.json (left) disagrees with the clap grammar (right) \
             on flag names or kinds"
        );
    }
}

#[test]
fn builtin_check_defs_are_well_formed_and_bundled() {
    let manifest = read_json("ct");
    let bundled = manifest["builtin_checks"]
        .as_array()
        .expect("ct.json must carry a `builtin_checks` array for agent hoisting");

    for check in BUILTIN_CHECKS {
        let standalone = read_json(check);
        assert_eq!(standalone["name"], *check, "name must equal the check name");
        assert!(standalone["description"].is_string(), "description must be a string");
        assert_eq!(standalone["input_schema"]["type"], "object");
        assert!(
            standalone["input_schema"]["properties"].is_object(),
            "input_schema.properties must be an object"
        );
        let entry = bundled
            .iter()
            .find(|c| c["name"] == *check)
            .unwrap_or_else(|| panic!("ct manifest is missing the '{check}' built-in check"));
        assert_eq!(
            *entry, standalone,
            "ct manifest's '{check}' check has drifted from docs/explain/{check}.json"
        );
    }
}
