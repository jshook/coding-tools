// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on the embedded `--explain` payloads: the same `docs/explain` files
//! the binaries `include_str!` must stay parseable and well-formed, so a typo
//! in a hand-authored tool definition fails the build rather than shipping.

use std::path::{Path, PathBuf};

fn explain_path(tool: &str, ext: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs/explain")
        .join(format!("{tool}.{ext}"))
}

#[test]
fn json_payloads_are_well_formed_tool_definitions() {
    for tool in ["ctsearch", "cttest"] {
        let path = explain_path(tool, "json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let v: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));

        assert_eq!(v["name"], tool, "name must equal the tool name");
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
    for tool in ["ctsearch", "cttest"] {
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
