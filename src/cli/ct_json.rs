// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-json` command grammar (see [`crate::cli`]); the `ct-json` bin is a
//! thin parse-and-dispatch wrapper over this `Cli`.

use clap::Parser;

use crate::explain::Format;
use crate::pulse::HeartbeatOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ct-json",
    version,
    about = "Query JSON/JSONC/JSONL with a jq filter, framed by an --expect verdict over the result count.",
    long_about = "ct-json reads one or more JSON-family documents (also reachable as `ct json`) and \
                  runs a jq filter over them (the jaq dialect, in-process: no jq binary, no shell). \
                  --path addresses a node in ct-patch's grammar (plus [*] for every element) and \
                  --query is any jq program; each result prints as one JSON value per line. The \
                  verdict is --expect over the number of results. Read-only, so it carries no \
                  allow-gate. See `ct-json --explain` for agent-oriented documentation."
)]
pub struct Cli {
    /// Document to query (repeatable). A path reads a file; file:PATH / stdin: / b64:DATA / text:JSON carry the document itself.
    #[arg(long, value_name = "PATH|SCHEME")]
    pub file: Vec<String>,

    /// jq filter to run (jaq dialect). Accepts file:PATH / stdin: / b64:DATA / text:VALUE, so a filter full of quotes travels shell-safely. Default: '.' (or just --path).
    #[arg(long, value_name = "FILTER|SCHEME")]
    pub query: Option<String>,

    /// Node to read first, in ct-patch's path grammar: dotted keys, [N] indices, [key=value] predicates, plus [*] for every element. --query then runs on each node selected.
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,

    /// Bind $NAME to the string VALUE for the filter (repeatable). VALUE accepts file:/stdin:/b64:/text:.
    #[arg(long, value_name = "NAME=VALUE")]
    pub arg: Vec<String>,

    /// Bind $NAME to VALUE parsed as JSON (repeatable).
    #[arg(long, value_name = "NAME=JSON")]
    pub argjson: Vec<String>,

    /// Force the input format instead of detecting it from the file extension (json/jsonc/jsonl/ndjson; an unknown extension reads as JSON).
    #[arg(long, value_enum)]
    pub format: Option<InputFormat>,

    /// Collect every input (each --file document, every JSONL record) into one array and run the filter once over it.
    #[arg(long)]
    pub slurp: bool,

    /// Print string results as their own characters, without quotes (like jq -r).
    #[arg(long)]
    pub raw: bool,

    /// Indent each result value over several lines instead of one compact value per line.
    #[arg(long)]
    pub pretty: bool,

    /// Stop after N results (the filter is not evaluated further).
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Verdict expectation over the number of results: any|none|N|=N|+N|-N (default: any).
    #[arg(long)]
    pub expect: Option<String>,

    /// Suppress the results; print only the one-line summary.
    #[arg(long)]
    pub quiet: bool,

    /// Emit a structured JSON result instead of the bare values.
    #[arg(long)]
    pub json: bool,

    /// Like `--json`, but pretty-printed (indented).
    #[arg(long)]
    pub json_pretty: bool,

    /// Abort with exit 2 if the query exceeds SECS seconds (fractional allowed).
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<f64>,

    #[command(flatten)]
    pub heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    pub explain: Option<Format>,
}

/// Input document format. JSON and JSONC parse through the same lenient
/// `jsonc-parser` tree (so a strict `.json` file with a stray trailing comma
/// still reads); JSONL feeds each non-blank line as its own input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum InputFormat {
    Json,
    Jsonc,
    Jsonl,
}

impl InputFormat {
    /// Detect a format from a file extension; `None` for an extension the
    /// suite's JSON family does not cover (the caller decides whether that
    /// defaults to JSON or is refused, as for YAML).
    pub fn from_ext(ext: &str) -> Option<InputFormat> {
        match ext.to_ascii_lowercase().as_str() {
            "json" => Some(InputFormat::Json),
            "jsonc" => Some(InputFormat::Jsonc),
            "jsonl" | "ndjson" => Some(InputFormat::Jsonl),
            _ => None,
        }
    }
}
