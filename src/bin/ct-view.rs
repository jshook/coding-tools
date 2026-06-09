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

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use coding_tools::explain::Format;
use coding_tools::pattern;
use coding_tools::view::{expand_and_merge, parse_range, segments};
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-view.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-view.json");

#[derive(Parser, Debug)]
#[command(
    name = "ct-view",
    version,
    about = "Show a file's lines by range, or the regions around a pattern with context.",
    long_about = "ct-view is a focused, bounded reader for a single file (also reachable as \
                  `ct view`): print a line range with --range, or the windows around a \
                  --match pattern with --context lines, rather than dumping the whole file. \
                  See `ct-view --explain` for agent-oriented documentation."
)]
struct Cli {
    /// File to view.
    path: PathBuf,

    /// Line range A:B (1-based, inclusive); also A: (to end), :B (from start), or A (one line).
    #[arg(long)]
    range: Option<String>,

    /// Show only lines matching this pattern (substring->glob->regex promoted), with --context around each.
    #[arg(long = "match")]
    pattern: Option<String>,

    /// Lines of context shown around each --match hit.
    #[arg(long, short = 'C', default_value_t = 2)]
    context: usize,

    /// Cap the number of lines emitted.
    #[arg(long)]
    limit: Option<usize>,

    /// Suppress the line-number gutter in text output.
    #[arg(long)]
    plain: bool,

    /// Emit a structured JSON result instead of text.
    #[arg(long)]
    json: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    explain: Option<Format>,
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let content = std::fs::read_to_string(&cli.path)
        .map_err(|e| format!("read {}: {e}", cli.path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    // Resolve which line indices to show, and whether a --match found anything.
    let (mut selected, matched): (Vec<usize>, Option<bool>) = if let Some(p) = &cli.pattern {
        let re = pattern::compile(p).map_err(|e| format!("invalid --match pattern: {e}"))?;
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| re.is_match(l))
            .map(|(i, _)| i)
            .collect();
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
        println!("{obj}");
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
