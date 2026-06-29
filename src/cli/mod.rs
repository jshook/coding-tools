// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! CLI grammars for the leaf tools, hosted in the lib so each tool's clap
//! definition is introspectable and reusable. Each `src/bin/<tool>.rs` entry
//! point is a thin parse-and-dispatch wrapper over the `Cli` struct defined
//! here; the schema-drift guard reconciles every `docs/explain/<tool>.json`
//! against the live grammar these expose (see [`flags`]).

use clap::CommandFactory;

pub mod ct_await;
pub mod ct_check;
pub mod ct_each;
pub mod ct_edit;
pub mod ct_okf;
pub mod ct_outline;
pub mod ct_patch;
pub mod ct_rules;
pub mod ct_search;
pub mod ct_steer;
pub mod ct_test;
pub mod ct_tree;
pub mod ct_view;

/// Every leaf tool's name paired with its clap grammar. Built-in checks
/// (`deps`/`mods`) are probe heads, not standalone tools, and are introspected
/// separately via their own `check_flags`.
pub fn commands() -> Vec<(&'static str, clap::Command)> {
    vec![
        ("ct-await", ct_await::Cli::command()),
        ("ct-check", ct_check::Cli::command()),
        ("ct-each", ct_each::Cli::command()),
        ("ct-edit", ct_edit::Cli::command()),
        ("ct-okf", ct_okf::Cli::command()),
        ("ct-outline", ct_outline::Cli::command()),
        ("ct-patch", ct_patch::Cli::command()),
        ("ct-rules", ct_rules::Cli::command()),
        ("ct-search", ct_search::Cli::command()),
        ("ct-steer", ct_steer::Cli::command()),
        ("ct-test", ct_test::Cli::command()),
        ("ct-tree", ct_tree::Cli::command()),
        ("ct-view", ct_view::Cli::command()),
    ]
}

/// Every leaf tool's name paired with its introspected [`crate::deps::Grammar`]
/// (flag specs + clap-required names). The introspection behind the schema-drift
/// guard; uses the same reader as the built-in checks ([`crate::deps::grammar`]).
pub fn grammars() -> Vec<(&'static str, crate::deps::Grammar)> {
    commands()
        .into_iter()
        .map(|(name, command)| (name, crate::deps::grammar(command)))
        .collect()
}
