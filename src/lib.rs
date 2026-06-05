// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Shared library for the `coding_tools` command-line suite.
//!
//! The binaries (`ctsearch` and `cttest`) are thin front-ends over the reusable
//! pieces collected here:
//!
//! * [`pattern`] — the shared substring → glob → regex promotion that every
//!   pattern-accepting option uses.
//! * [`explain`] — the `--explain` agent-documentation format selector.

pub mod explain;
pub mod pattern;
