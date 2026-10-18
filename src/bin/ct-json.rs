// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-json` — a read-only jq query over JSON / JSONC / JSONL.
//!
//! Address a node with `--path` (the `ct-patch` grammar plus `[*]`), run any
//! jq filter with `--query` (the jaq dialect, in-process), and get one JSON
//! value per line, framed by an `--expect` verdict over the result count.
//! Read-only, so it carries no allow-gate; reachable directly or as
//! `ct json`. The canonical reference is `docs/explain/ct-json.md`;
//! `docs/explain/ct-json.json` is the MCP tool-use definition. Both are
//! embedded below.

use std::process::ExitCode;

use coding_tools::cli::ct_json::{Cli, InputFormat};
use coding_tools::explain::Format;
use coding_tools::jsonq::{self, Program};
use coding_tools::payload;
use coding_tools::pulse::{self, PulseState};
use coding_tools::verdict::{Expect, Verdict};
use jaq_json::Val;
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-json.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-json.json");

/// One parsed input: where it came from (for messages) and its value.
struct Input {
    label: String,
    value: Val,
}

/// The format for a `--file` value: forced, else by the extension of a bare
/// path, else JSON. A YAML extension is refused rather than mis-read.
fn format_for(cli: &Cli, raw: &str) -> Result<InputFormat, String> {
    if let Some(f) = cli.format {
        return Ok(f);
    }
    if payload::has_scheme(raw) {
        return Ok(InputFormat::Json);
    }
    let ext = std::path::Path::new(raw)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "yaml" | "yml" => Err(format!(
            "{raw}: YAML input is not supported by ct-json (JSON, JSONC and JSONL are)"
        )),
        e => Ok(InputFormat::from_ext(e).unwrap_or(InputFormat::Json)),
    }
}

/// Parse one JSON-family document (comments and trailing commas tolerated)
/// into a jaq value, keeping key order.
fn parse_doc(text: &str, label: &str) -> Result<Val, String> {
    jsonq::parse_document(text)
        .map_err(|e| format!("{label}: {e}"))?
        .ok_or_else(|| format!("{label}: empty document"))
}

/// Read and parse every `--file` into the inputs the filter will see.
fn load_inputs(cli: &Cli) -> Result<Vec<Input>, String> {
    let mut inputs = Vec::new();
    for raw in &cli.file {
        let format = format_for(cli, raw)?;
        let (text, label) = payload::resolve_document(raw)?;
        match format {
            InputFormat::Json | InputFormat::Jsonc => inputs.push(Input {
                value: parse_doc(&text, &label)?,
                label,
            }),
            InputFormat::Jsonl => {
                for (i, line) in text.lines().enumerate() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let at = format!("{label}:{}", i + 1);
                    inputs.push(Input {
                        value: parse_doc(line, &at)?,
                        label: at,
                    });
                }
            }
        }
    }
    Ok(inputs)
}

/// `--arg` / `--argjson` bindings: `(name, value)` in command-line order.
fn bindings(cli: &Cli) -> Result<(Vec<String>, Vec<Val>), String> {
    let mut names = Vec::new();
    let mut values = Vec::new();
    let mut bind = |flag: &str, spec: &str, value: Val| -> Result<(), String> {
        let (name, _) = spec
            .split_once('=')
            .ok_or_else(|| format!("--{flag} needs NAME=VALUE, got '{spec}'"))?;
        if name.is_empty()
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || name.starts_with(|c: char| c.is_ascii_digit())
        {
            return Err(format!(
                "--{flag}: '{name}' is not a variable name (letters, digits, '_'; not starting with a digit)"
            ));
        }
        if names.iter().any(|n| n == name) {
            return Err(format!("--{flag}: ${name} is bound twice"));
        }
        names.push(name.to_string());
        values.push(value);
        Ok(())
    };
    for spec in &cli.arg {
        let value = spec.split_once('=').map(|(_, v)| v).unwrap_or("");
        let text = payload::resolve(value)?.text;
        bind("arg", spec, Val::from(text))?;
    }
    for spec in &cli.argjson {
        let value = spec.split_once('=').map(|(_, v)| v).unwrap_or("");
        let parsed = jsonq::parse_json(value)
            .map_err(|e| format!("--argjson '{spec}': VALUE is not JSON: {e}"))?;
        bind("argjson", spec, parsed)?;
    }
    Ok((names, values))
}

/// The filter text: `--path` translated, then `--query`, piped.
fn filter_text(cli: &Cli) -> Result<String, String> {
    let query = match &cli.query {
        Some(q) => Some(payload::resolve(q)?.text),
        None => None,
    };
    let path = match &cli.path {
        Some(p) => Some(jsonq::path_filter(p).map_err(|e| format!("invalid --path: {e}"))?),
        None => None,
    };
    Ok(match (path, query) {
        (Some(p), Some(q)) => format!("({p}) | ({q})"),
        (Some(p), None) => p,
        (None, Some(q)) => q,
        (None, None) => ".".to_string(),
    })
}

fn run(mut cli: Cli) -> Result<ExitCode, String> {
    // --json-pretty enables JSON output on its own; treat it as --json
    // everywhere the text path is gated.
    if cli.json_pretty {
        cli.json = true;
    }
    let _watchdog = pulse::watchdog("ct-json", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-json", PulseState::new())?;
    if cli.file.is_empty() {
        return Err(
            "nothing to query: supply --file PATH (or --file stdin: for a piped document)"
                .to_string(),
        );
    }
    let expect = match &cli.expect {
        Some(s) => Expect::parse(s).map_err(|e| format!("invalid --expect: {e}"))?,
        None => Expect::default(),
    };
    let code = filter_text(&cli)?;
    let (names, values) = bindings(&cli)?;
    let program = Program::compile(&code, &names).map_err(|e| format!("invalid --query: {e}"))?;

    let mut inputs = load_inputs(&cli)?;
    let labels: Vec<String> = inputs.iter().map(|i| i.label.clone()).collect();
    if cli.slurp {
        let all: Val = inputs.drain(..).map(|i| i.value).collect();
        inputs.push(Input {
            label: "slurp".to_string(),
            value: all,
        });
    }

    // Evaluate: every result of every input, in order, up to --limit.
    let mut results: Vec<Val> = Vec::new();
    let limit = cli.limit.unwrap_or(usize::MAX);
    'inputs: for input in inputs {
        for r in program.run(input.value, values.clone()) {
            if results.len() >= limit {
                break 'inputs;
            }
            results.push(r.map_err(|e| format!("{}: {e}", input.label))?);
        }
    }
    let count = results.len();
    let verdict = expect.eval(count as u64);

    if cli.json {
        let obj = json!({
            "tool": "ct-json",
            "verdict": verdict.label(),
            "count": count,
            "files": labels,
            "filter": code,
            "values": results.iter().map(jsonq::to_json).collect::<Vec<_>>(),
        });
        coding_tools::jsonout::print(&obj, cli.json_pretty);
    } else if cli.quiet {
        println!("{count} value(s) -> {}", verdict.label());
    } else {
        for v in &results {
            println!("{}", jsonq::render(v, cli.raw, cli.pretty));
        }
        if verdict != Verdict::Success {
            eprintln!(
                "ct-json: {count} value(s) -> {} (--expect {})",
                verdict.label(),
                cli.expect.as_deref().unwrap_or("any")
            );
        }
    }
    Ok(verdict.exit_code())
}

fn main() -> ExitCode {
    let cli: Cli = coding_tools::cli::parse();

    if let Some(fmt) = cli.explain {
        let body = match fmt {
            Format::Md => EXPLAIN_MD,
            Format::Json => EXPLAIN_JSON,
        };
        print!("{body}");
        return ExitCode::SUCCESS;
    }

    match run(cli) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("ct-json: {msg}");
            ExitCode::from(2)
        }
    }
}
