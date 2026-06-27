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
//! * [`payload`] — the `file:` / `text:` value schemes every payload-typed
//!   option resolves through.
//! * [`block`] — line-anchored literal block matching (and the nearest-miss
//!   diagnostic) behind multi-line patterns in `ct-search`/`ct-view`/`ct-edit`.
//! * [`blockdoc`] — the `.ctb` block-document parser behind `ct-edit --script`.
//! * [`editscript`] — the `--script` batch engine: compiled edits simulated
//!   in memory under the prepare/confirm/write standard.
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
//! * [`deps`] — the `deps` built-in check's crate-graph queries over `cargo
//!   metadata` (including its in-process [`deps::check`] entry point).
//! * [`modgraph`] — the `mods` built-in check's heuristic intra-crate module-use
//!   graph, reusing [`deps`]'s assertions at module granularity.
//! * [`okf`] — Open Knowledge Format support: frontmatter parsing, bundle
//!   conformance, cross-link checking, and the `okf` built-in check, shared by
//!   `ct-okf` and the OKF-aware file/structure tools.
//! * [`okfscript`] — the `ct-okf --script` batch engine: `.ctb` OKF mutations
//!   simulated over an in-memory overlay under the prepare/confirm/write standard.
//! * [`outline`] — `ct-outline`'s heuristic per-language declaration
//!   detection.
//! * [`view`] — `ct-view`'s range parsing and context-window merging.
//! * [`tree`] — `ct-tree`'s line/word/character counts and grouping.
//! * [`edit`] — `ct-edit`'s line-scoped, byte-preserving replacement engine.
//! * [`patch`] — `ct-patch`'s node-path / predicate / value parsing.
//! * [`testrun`] — `ct-test`'s `--focus` output distiller.

pub mod allowlist;
pub mod block;
pub mod blockdoc;
pub mod cli;
pub mod completion;
pub mod deps;
pub mod edit;
pub mod editscript;
pub mod explain;
pub mod jsonout;
pub mod modgraph;
pub mod okf;
pub mod okfscript;
pub mod outline;
pub mod patch;
pub mod pattern;
pub mod payload;
pub mod pulse;
pub mod rules;
pub mod supervise;
pub mod template;
pub mod testrun;
pub mod tree;
pub mod verdict;
pub mod view;
pub mod walk;
