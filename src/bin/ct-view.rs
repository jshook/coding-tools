// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-view` — bounded, context-aware file viewing.
//!
//! A focused reader for one file, reachable directly or as `ct view`: show a
//! line range, or the regions around a pattern with N lines of context, instead
//! of dumping a whole file. Read-only, so it carries no allow-gate. The
//! canonical reference is `docs/explain/ct-view.md` — the text this tool emits
//! for `--explain md`; `docs/explain/ct-view.json` is the MCP tool-use
//! definition emitted for `--explain json`. Both are embedded below.

use std::process::ExitCode;

use clap::Parser;
use coding_tools::cli::ct_view::Cli;
use coding_tools::explain::Format;
use coding_tools::pulse::{self, PulseState};
use coding_tools::view::{expand_and_merge, parse_range, segments};
use coding_tools::{block, pattern, payload};
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-view.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-view.json");

fn run(mut cli: Cli) -> Result<ExitCode, String> {
    // --json-pretty enables JSON output on its own; treat it as --json
    // everywhere the text path is gated.
    if cli.json_pretty {
        cli.json = true;
    }
    let _watchdog = pulse::watchdog("ct-view", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-view", PulseState::new())?;
    let content = std::fs::read_to_string(&cli.path)
        .map_err(|e| format!("read {}: {e}", cli.path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    // Resolve which line indices to show, and whether a --match found anything.
    let (mut selected, matched): (Vec<usize>, Option<bool>) = if let Some(p) = &cli.pattern {
        let resolved = payload::resolve(p)?;
        let pat_lines = payload::to_lines(&resolved.text);
        let hits: Vec<usize> = if pat_lines.len() > 1 {
            // A multi-line pattern is a line-anchored literal block; the
            // context window expands around the whole matched region.
            if matches!(cli.mode, Some(pattern::Mode::Glob) | Some(pattern::Mode::Regex)) {
                return Err(
                    "a multi-line pattern matches as a literal block; --mode glob/regex is reserved"
                        .to_string(),
                );
            }
            let starts = block::find_starts(&lines, &pat_lines);
            if starts.is_empty()
                && let Some(m) = block::nearest_miss(&lines, &pat_lines)
            {
                eprintln!(
                    "ct-view: nearest miss: {}:{}: block diverges at its line {}",
                    cli.path.display(),
                    m.line,
                    m.first_diverging_line
                );
                eprintln!("ct-view:   expected: {}", m.expected);
                eprintln!("ct-view:   found:    {}", m.found);
            }
            starts
                .iter()
                .flat_map(|&s| s..s + pat_lines.len())
                .collect()
        } else {
            let effective = cli
                .mode
                .or(resolved.from_file.then_some(pattern::Mode::Literal));
            let single = pat_lines.into_iter().next().unwrap_or_default();
            let re = pattern::compile_with(&single, effective)
                .map_err(|e| format!("invalid --match pattern: {e}"))?;
            lines
                .iter()
                .enumerate()
                .filter(|(_, l)| re.is_match(l))
                .map(|(i, _)| i)
                .collect()
        };
        let found = !hits.is_empty();
        (expand_and_merge(&hits, cli.context, total), Some(found))
    } else if let Some(r) = &cli.range {
        let sel = match parse_range(r, total)? {
            Some((s, e)) => (s..=e).collect(),
            None => Vec::new(),
        };
        (sel, None)
    } else {
        ((0..total).collect(), None)
    };

    if let Some(limit) = cli.limit {
        selected.truncate(limit);
    }

    if cli.json {
        let out_lines: Vec<_> = selected
            .iter()
            .map(|&i| json!({ "n": i + 1, "text": lines[i] }))
            .collect();
        let mut obj = json!({
            "tool": "ct-view",
            "path": cli.path.display().to_string(),
            "total_lines": total,
            "shown": selected.len(),
            "lines": out_lines,
        });
        if let Some(found) = matched {
            obj["matched"] = json!(found);
        }
        coding_tools::jsonout::print(&obj, cli.json_pretty);
    } else {
        let width = total.max(1).to_string().len();
        for (gi, (s, e)) in segments(&selected).iter().enumerate() {
            if gi > 0 {
                println!("--");
            }
            for (offset, line) in lines[*s..=*e].iter().enumerate() {
                let n = *s + offset + 1;
                if cli.plain {
                    println!("{line}");
                } else {
                    println!("{n:>width$}  {line}");
                }
            }
        }
    }

    // A --match that found nothing is a clean negative (exit 1), like a search;
    // any other successful view is exit 0.
    Ok(match matched {
        Some(false) => ExitCode::from(1),
        _ => ExitCode::SUCCESS,
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();

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
            eprintln!("ct-view: {msg}");
            ExitCode::from(2)
        }
    }
}
