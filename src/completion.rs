// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Dynamic shell completion for the `ct` umbrella, via `veks-completion`.
//!
//! The command tree is derived from the lib-hosted clap grammar
//! ([`crate::cli`]) so it can never drift from the actual CLI: every leaf tool
//! becomes a `ct <name>` subcommand whose value/boolean flags come straight
//! from the introspected grammar, and a value_enum flag's variants become its
//! completion set the same way. On top of that, a few flags get **runtime**
//! value providers that read the live rule store — ids, tags, and def names —
//! which a static `clap_complete` script cannot do. The `ct` binary drives this
//! tree through [`veks_completion::handle_complete_env`].

use std::sync::Arc;

use veks_completion::{CommandTree, Node, ValueProvider};

use crate::{cli, rules};

/// Which rule-store field a dynamic provider serves.
#[derive(Clone, Copy)]
enum Field {
    Id,
    Tag,
    Def,
}

/// Read the nearest rule store (`.ct/rules.jsonc`, discovered upward from the
/// cwd) and return its ids / tags / def names. Empty on any failure —
/// completion offers nothing rather than erroring.
fn store_values(field: Field) -> Vec<String> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    let Some(root) = rules::discover_root(&cwd) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(rules::store_path(&root)) else {
        return Vec::new();
    };
    let Ok(store) = rules::parse_store(&text) else {
        return Vec::new();
    };
    match field {
        Field::Id => store.rules.iter().map(|r| r.id.clone()).collect(),
        Field::Tag => {
            let mut tags: Vec<String> = store
                .rules
                .iter()
                .flat_map(|r| r.tags.iter().cloned())
                .collect();
            tags.sort();
            tags.dedup();
            tags
        }
        Field::Def => store.defs.keys().cloned().collect(),
    }
}

/// A provider over a live store field, filtered by the partial word.
fn store_provider(field: Field) -> ValueProvider {
    Arc::new(move |partial: &str, _: &[&str]| {
        store_values(field)
            .into_iter()
            .filter(|v| v.starts_with(partial))
            .collect()
    })
}

/// A provider over a fixed value set (a value_enum's variants).
fn set_provider(values: Vec<String>) -> ValueProvider {
    Arc::new(move |partial: &str, _: &[&str]| {
        values
            .iter()
            .filter(|v| v.starts_with(partial))
            .cloned()
            .collect()
    })
}

/// Attach the store-backed dynamic providers for a subcommand that selects by
/// id/tag/def (`ct check`, `ct rules`).
fn with_store_providers(sub: &str, node: Node) -> Node {
    match sub {
        "check" => node
            .with_value_provider("--id", store_provider(Field::Id))
            .with_value_provider("--tag", store_provider(Field::Tag)),
        "rules" => node
            .with_value_provider("--promote", store_provider(Field::Id))
            .with_value_provider("--remove", store_provider(Field::Id))
            .with_value_provider("--tag", store_provider(Field::Tag))
            .with_value_provider("--def", store_provider(Field::Def)),
        _ => node,
    }
}

/// Build the `ct` completion tree from the live clap grammar: one subcommand
/// per leaf tool (its `ct-` prefix dropped), flags split into value/boolean by
/// the introspected kind, enum value-sets and store providers attached.
pub fn command_tree() -> CommandTree {
    let mut tree = CommandTree::new("ct");
    for (name, grammar) in cli::grammars() {
        let sub = name.strip_prefix("ct-").unwrap_or(name);
        let value_flags: Vec<String> = grammar
            .flags
            .iter()
            .filter(|f| f.kind != "boolean")
            .map(|f| format!("--{}", f.name))
            .collect();
        let bool_flags: Vec<String> = grammar
            .flags
            .iter()
            .filter(|f| f.kind == "boolean")
            .map(|f| format!("--{}", f.name))
            .collect();
        let vf: Vec<&str> = value_flags.iter().map(String::as_str).collect();
        let bf: Vec<&str> = bool_flags.iter().map(String::as_str).collect();

        let mut node = Node::leaf_with_flags(&vf, &bf);
        for f in grammar
            .flags
            .iter()
            .filter(|f| f.kind != "boolean" && !f.values.is_empty())
        {
            node =
                node.with_value_provider(&format!("--{}", f.name), set_provider(f.values.clone()));
        }
        node = with_store_providers(sub, node);
        tree = tree.command(sub, node);
    }
    // The meta-command that prints the shell registration script; its single
    // positional is the shell name.
    let shells = ["bash", "zsh", "fish"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    tree = tree.command(
        "completions",
        Node::leaf(&[]).with_positional_provider(set_provider(shells)),
    );
    tree
}
