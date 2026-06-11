// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Shared library for the `coding_tools` command-line suite.
//!
//! The binaries (`ct` and the `ct-*` tools it dispatches to) are thin
//! front-ends over the reusable, doctested pieces collected here.
//!
//! Cross-cutting surfaces, used by several commands:
//!
//! * [`pattern`] — the shared substring → glob → regex promotion that every
//!   pattern-accepting option uses.
//! * [`walk`] — the shared file-selection predicates (`--base`/`--name`/`--type`
//!   /`--size`/`--hidden`/`--follow`) that `ct-search`/`ct-edit`/`ct-patch`/
//!   `ct-tree` target with.
//! * [`verdict`] — the shared `SUCCESS`/`ERROR` outcome, its exit-status
//!   mapping, and the count [`Expect`](verdict::Expect)ation that frames a
//!   search/edit/patch as a pass/fail test.
//! * [`template`] — the `{TOKEN}` substitution engine behind every `--emit`
//!   verdict template.
//! * [`allowlist`] — the fixed command allow-gates behind `ct-test` and
//!   `ct-each`.
//! * [`explain`] — the `--explain` agent-documentation format selector.
//! * [`pulse`] — the `--timeout` watchdog and `--heartbeat` liveness pulse
//!   every tool carries.
//! * [`rules`] — the `.ct/rules.jsonc` invariant surface shared by
//!   `ct-rules` and `ct-check`: store model, defs, probe gate, the external
//!   bridge, and outcome adapters.
//! * [`supervise`] — bounded, captured child execution for the dispatching
//!   tools (`ct-test`, `ct-each`), including suite sibling resolution.
//!
//! Per-command surfaces (the pure logic each `ct-*` tool is built on):
//!
//! * [`deps`] — `ct-deps`'s crate-graph queries over `cargo metadata`.
//! * [`outline`] — `ct-outline`'s heuristic per-language declaration
//!   detection.
//! * [`view`] — `ct-view`'s range parsing and context-window merging.
//! * [`tree`] — `ct-tree`'s line/word/character counts and grouping.
//! * [`edit`] — `ct-edit`'s line-scoped, byte-preserving replacement engine.
//! * [`patch`] — `ct-patch`'s node-path / predicate / value parsing.
//! * [`testrun`] — `ct-test`'s `--focus` output distiller.

pub mod allowlist;
pub mod deps;
pub mod edit;
pub mod explain;
pub mod outline;
pub mod patch;
pub mod pattern;
pub mod pulse;
pub mod rules;
pub mod supervise;
pub mod template;
pub mod testrun;
pub mod tree;
pub mod verdict;
pub mod view;
pub mod walk;
