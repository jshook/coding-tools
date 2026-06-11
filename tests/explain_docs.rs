// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on the embedded `--explain` payloads: the same `docs/explain` files
//! the binaries `include_str!` must stay parseable and well-formed, so a typo
//! in a hand-authored tool definition fails the build rather than shipping. The
//! umbrella `ct` additionally bundles each leaf tool's definition under `tools`,
//! which must not drift from the standalone leaf files.

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
    "ct-deps",
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
    "ct-deps",
];

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
