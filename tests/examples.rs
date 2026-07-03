// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on the worked `examples` each tool's `docs/explain` payload carries.
//! Examples are the highest-leverage part of the agent docs — an agent copies
//! them verbatim — so a stale one is worse than none. Every example must:
//!
//! 1. parse against the tool's live clap grammar (so a renamed/removed flag or a
//!    missing required argument fails the build, exactly like the schema-drift
//!    guard in `explain_docs.rs`), and
//! 2. appear verbatim in the tool's `.md`, so the human reference and the
//!    machine definition never drift apart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every leaf tool that must carry examples (the `cli::commands()` set).
const TOOLS: &[&str] = &[
    "ct-search",
    "ct-view",
    "ct-tree",
    "ct-edit",
    "ct-patch",
    "ct-test",
    "ct-each",
    "ct-outline",
    "ct-survey",
    "ct-okf",
    "ct-rules",
    "ct-check",
    "ct-await",
    "ct-steer",
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

fn read_md(tool: &str) -> String {
    let path = explain_path(tool, "md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The tool's clap command, by its `ct-<name>` key.
fn command_for(tool: &str) -> clap::Command {
    coding_tools::cli::commands()
        .into_iter()
        .find(|(name, _)| *name == tool)
        .map(|(_, command)| command)
        .unwrap_or_else(|| panic!("no clap command registered for {tool}"))
}

/// Split a command line into argv, honouring single and double quotes (no
/// escapes — examples are written to stay simple). Quotes are removed; a quoted
/// empty string is preserved as an empty token.
fn tokenize(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                have = true;
                for q in chars.by_ref() {
                    if q == c {
                        break;
                    }
                    cur.push(q);
                }
            }
            ' ' | '\t' => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            other => {
                have = true;
                cur.push(other);
            }
        }
    }
    if have {
        out.push(cur);
    }
    out
}

/// The short subcommand name for a `ct-<name>` tool (`ct-search` -> `search`).
fn short(tool: &str) -> &str {
    tool.strip_prefix("ct-").unwrap_or(tool)
}

/// Strip the leading `ct <short>` / `ct-<short>` invocation from a tokenized
/// example, returning the argument tail that clap should parse.
fn strip_invocation(tool: &str, argv: &[String]) -> Vec<String> {
    let s = short(tool);
    if argv.first().map(String::as_str) == Some("ct") && argv.get(1).map(String::as_str) == Some(s)
    {
        argv[2..].to_vec()
    } else if argv.first().map(String::as_str) == Some(tool) {
        argv[1..].to_vec()
    } else {
        panic!("example for {tool} must start with `ct {s}` or `{tool}`: {argv:?}");
    }
}

#[test]
fn every_tool_has_examples_that_parse_and_appear_in_md() {
    for &tool in TOOLS {
        let json = read_json(tool);
        let md = read_md(tool);
        let examples = json["examples"]
            .as_array()
            .unwrap_or_else(|| panic!("{tool}.json must carry an `examples` array"));
        assert!(
            examples.len() >= 2,
            "{tool}.json should carry at least two examples"
        );

        // Names must be unique so an agent sees distinct, non-redundant cases.
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();
        for ex in examples {
            let cmd = ex["cmd"]
                .as_str()
                .unwrap_or_else(|| panic!("{tool}.json: each example needs a string `cmd`"));
            assert!(
                ex["why"].as_str().is_some_and(|w| !w.trim().is_empty()),
                "{tool}.json: example `{cmd}` needs a non-empty `why`"
            );
            assert!(
                seen.insert(cmd.to_string(), ()).is_none(),
                "{tool}.json: duplicate example `{cmd}`"
            );

            // (2) verbatim in the .md
            assert!(
                md.contains(cmd),
                "{tool}.md is missing the example `{cmd}` (it must appear verbatim)"
            );

            // (1) parses against the live clap grammar
            let argv = strip_invocation(tool, &tokenize(cmd));
            let full: Vec<String> = std::iter::once(tool.to_string()).chain(argv).collect();
            if let Err(e) = command_for(tool).try_get_matches_from(&full) {
                panic!(
                    "{tool}: example does not parse against the CLI:\n  {cmd}\n{}",
                    e.to_string().lines().next().unwrap_or("")
                );
            }
        }
    }
}
