// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Shared JSON result output for the `--json` / `--json-pretty` tools: one
//! place that decides compact (the default, one line for piping into `jq`) vs.
//! pretty-printed (indented, for reading). `--json-pretty` implies `--json`.

use serde_json::Value;

/// Print a JSON value to stdout — pretty-printed (indented) when `pretty`, else
/// compact on a single line.
pub fn print(value: &Value, pretty: bool) {
    if pretty {
        println!("{}", serde_json::to_string_pretty(value).expect("a JSON value serializes"));
    } else {
        println!("{value}");
    }
}
