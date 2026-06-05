// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `--explain` agent-documentation format selector.
//!
//! Each binary embeds its own payloads from `docs/explain/<tool>.{md,json}` via
//! `include_str!`, so the bytes printed by `--explain` are exactly the bytes
//! checked into the repository. This module only carries the shared [`Format`]
//! choice between the two payloads.

/// Output format for the self-describing `--explain` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Human- and agent-readable Markdown usage guide (llms.txt-style). Default.
    Md,
    /// Machine-readable MCP / tool-use definition (JSON).
    Json,
}
