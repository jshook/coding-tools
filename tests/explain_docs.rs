// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Guards on the embedded `--explain` payloads: the same `docs/explain` files
//! the binaries `include_str!` must stay parseable and well-formed, so a typo
//! in a hand-authored tool definition fails the build rather than shipping. The
//! umbrella `ct` additionally bundles each leaf tool's definition under `tools`,
//! which must not drift from the standalone leaf files.

use std::collections::{BTreeMap, BTreeSet};
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
    "ct-okf",
    "ct-outline",
    "ct-survey",
    "ct-rules",
    "ct-check",
    "ct-await",
    "ct-steer",
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
    "ct-okf",
    "ct-outline",
    "ct-survey",
    "ct-rules",
    "ct-check",
    "ct-await",
    "ct-steer",
];

/// Built-in checks — reserved probe heads run in-process by the rule layer, not
/// standalone tools — that the `ct` manifest advertises under `builtin_checks`.
const BUILTIN_CHECKS: &[&str] = &["deps", "mods", "okf"];

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

/// A tool definition with its `examples` array removed. The hoisted `ct`
/// manifest deliberately omits per-tool `examples` to stay lean — worked
/// examples live in each leaf's own `.json` (what `ct <tool> --explain json`
/// emits) and `.md`. So the bundle must match the leaf on everything *except*
/// `examples`.
fn without_examples(v: &serde_json::Value) -> serde_json::Value {
    let mut v = v.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("examples");
    }
    v
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
            without_examples(bundled),
            without_examples(&standalone),
            "ct manifest's '{leaf}' definition has drifted from docs/explain/{leaf}.json \
             (ignoring `examples`, which the manifest intentionally omits)"
        );
    }
}

/// Schema properties that are positional or trailing-argv inputs, not `--flags`
/// — excluded when reconciling a tool's schema against its clap grammar.
fn non_flag_props(tool: &str) -> &'static [&'static str] {
    match tool {
        "ct-view" => &["path"],    // positional <PATH>
        "ct-survey" => &["path"],  // positional [PATH]
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
    if matches!(ty, "number" | "integer") {
        "string"
    } else {
        ty
    }
    .to_string()
}

/// The unified schema-drift guard: every tool's `docs/explain/<tool>.json` must
/// agree with its live clap grammar on four facets — flag **names**, value
/// **kinds** (bool/array/scalar), **enum** values (e.g. `--mode`, `--type`,
/// `--edges`), and **required**ness — so a flag added/removed, a kind or enum
/// change, or a newly-mandatory argument can't ship without the matching doc
/// edit. Leaf grammars come from the lib-hosted `coding_tools::cli`; built-in
/// checks from their `check_grammar`. Meta flags (`--help`/`--version`/
/// `--explain`) are dropped by the grammar reader; positional/trailing-argv
/// inputs (see `non_flag_props`) are dropped from the name/kind/enum side but
/// kept for the required check (a positional like `path` can be mandatory).
#[test]
fn schema_matches_clap_grammar() {
    let mut grammars: Vec<(&str, coding_tools::deps::Grammar)> = coding_tools::cli::grammars();
    grammars.push(("deps", coding_tools::deps::check_grammar()));
    grammars.push(("mods", coding_tools::modgraph::check_grammar()));
    grammars.push(("okf", coding_tools::okf::check_grammar()));

    for (tool, grammar) in grammars {
        let v = read_json(tool);
        let props = v["input_schema"]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{tool}.json input_schema.properties must be an object"));
        let skip = non_flag_props(tool);

        // 1. Names + kinds: the long flags must match the schema properties
        //    (minus positional/trailing-argv inputs), with scalar types bucketed.
        let cli_kinds: BTreeMap<String, String> = grammar
            .flags
            .iter()
            .map(|f| (f.name.clone(), normalize_kind(f.kind)))
            .collect();
        let doc_kinds: BTreeMap<String, String> = props
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
            doc_kinds, cli_kinds,
            "{tool}: docs/explain/{tool}.json (left) disagrees with the clap grammar (right) \
             on flag names or kinds"
        );

        // 2. Enum values: a value_enum flag's variants must equal the schema's
        //    `enum` (at `.enum` for a scalar, `.items.enum` for an array).
        //    Booleans are skipped — clap reports `[true, false]` for them, which
        //    is not a schema enum.
        for f in grammar
            .flags
            .iter()
            .filter(|f| f.kind != "boolean" && !f.values.is_empty())
        {
            let spec = &props[&f.name];
            let enum_node = if f.kind == "array" {
                &spec["items"]["enum"]
            } else {
                &spec["enum"]
            };
            let doc_vals: BTreeSet<&str> = enum_node
                .as_array()
                .unwrap_or_else(|| panic!("{tool}.json: --{} should carry an enum array", f.name))
                .iter()
                .map(|x| x.as_str().expect("enum entries are strings"))
                .collect();
            let cli_vals: BTreeSet<&str> = f.values.iter().map(String::as_str).collect();
            assert_eq!(
                doc_vals, cli_vals,
                "{tool}: --{} enum disagrees with the clap grammar",
                f.name
            );
        }

        // 3. Required: every argument clap structurally requires (flags and
        //    positionals) must be listed in the schema's `required`. The schema
        //    may additionally mark semantic requireds clap doesn't enforce.
        let doc_required: BTreeSet<&str> = v["input_schema"]["required"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        for name in &grammar.required {
            assert!(
                doc_required.contains(name.as_str()),
                "{tool}: clap requires '{name}' but docs/explain/{tool}.json omits it from `required`"
            );
        }
    }
}

/// Every flag a leaf tool accepts must be mentioned in its `.md` reference —
/// the human-facing doc can't silently omit a flag (ct-search gained `--json`
/// in the schema while the doc lagged, exactly this drift). The reverse — a
/// `--flag` in the prose that is no longer real — is deliberately *not* checked:
/// the docs richly cross-reference other tools' flags, cargo flags
/// (`--locked`/`--offline`), and `--move-*`-style shorthands, so a phantom-flag
/// check would be dominated by false positives.
#[test]
fn md_documents_every_flag() {
    for (tool, grammar) in coding_tools::cli::grammars() {
        let path = explain_path(tool, "md");
        let md = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        // Long-flag tokens mentioned anywhere in the doc: each `--` followed by
        // its run of flag-name characters.
        let mentioned: BTreeSet<&str> = md
            .match_indices("--")
            .map(|(i, _)| {
                md[i + 2..]
                    .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
                    .next()
                    .unwrap_or("")
            })
            .collect();
        for f in &grammar.flags {
            assert!(
                mentioned.contains(f.name.as_str()),
                "{tool}: --{} is accepted by the CLI but never mentioned in docs/explain/{tool}.md",
                f.name
            );
        }
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
        assert!(
            standalone["description"].is_string(),
            "description must be a string"
        );
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

/// `ct-okf` is the suite's one subcommand-shaped tool, so the flat-flag
/// schema-drift guard (`schema_matches_clap_grammar`) only reconciles its
/// global flags. This keeps the subcommand surface honest the other way: every
/// clap subcommand must be listed verbatim in the `commands` array of
/// `docs/explain/ct-okf.json` and mentioned in `ct-okf.md`, and the doc must not
/// invent a subcommand that does not exist.
#[test]
fn ct_okf_subcommands_match_docs() {
    use clap::CommandFactory;
    let command = coding_tools::cli::ct_okf::Cli::command();
    let subs: BTreeSet<String> = command
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();

    let json = read_json("ct-okf");
    let doc_cmds: BTreeSet<String> = json["commands"]
        .as_array()
        .expect("ct-okf.json must carry a `commands` array for the subcommand surface")
        .iter()
        .map(|c| {
            c["name"]
                .as_str()
                .expect("each command needs a string name")
                .to_string()
        })
        .collect();
    assert_eq!(
        subs, doc_cmds,
        "ct-okf.json `commands` (right) disagrees with the clap subcommands (left)"
    );

    let md = std::fs::read_to_string(explain_path("ct-okf", "md"))
        .unwrap_or_else(|e| panic!("read ct-okf.md: {e}"));
    for name in &subs {
        assert!(
            md.contains(name.as_str()),
            "ct-okf.md never mentions the `{name}` subcommand"
        );
    }
}

/// `ct-steer` is subcommand-shaped like `ct-okf`, so the flat-flag drift guard
/// only reconciles its global flags. Keep the subcommand surface honest: every
/// clap subcommand must be listed in `docs/explain/ct-steer.json`'s `commands`
/// array and mentioned in `ct-steer.md`, and the doc must not invent one.
#[test]
fn ct_steer_subcommands_match_docs() {
    use clap::CommandFactory;
    let command = coding_tools::cli::ct_steer::Cli::command();
    let subs: BTreeSet<String> = command
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();

    let json = read_json("ct-steer");
    let doc_cmds: BTreeSet<String> = json["commands"]
        .as_array()
        .expect("ct-steer.json must carry a `commands` array for the subcommand surface")
        .iter()
        .map(|c| {
            c["name"]
                .as_str()
                .expect("each command needs a string name")
                .to_string()
        })
        .collect();
    assert_eq!(
        subs, doc_cmds,
        "ct-steer.json `commands` (right) disagrees with the clap subcommands (left)"
    );

    let md = std::fs::read_to_string(explain_path("ct-steer", "md"))
        .unwrap_or_else(|e| panic!("read ct-steer.md: {e}"));
    for name in &subs {
        assert!(
            md.contains(name.as_str()),
            "ct-steer.md never mentions the `{name}` subcommand"
        );
    }
}
