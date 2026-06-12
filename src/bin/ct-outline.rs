// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-outline` — heuristic structural outline.
//!
//! Reports the declarations in a file or tree — kind, name, `start:end` span,
//! nesting depth — so the next read can be a bounded `ct-view --range` instead
//! of a whole-file dump; reachable directly or as `ct outline`. Read-only.
//! Start lines are exact; an end the block heuristic cannot derive renders as
//! `start:?`. The canonical, self-contained reference is
//! `docs/explain/ct-outline.md` — the same text this tool emits for
//! `--explain md`; `docs/explain/ct-outline.json` is the MCP tool-use
//! definition emitted for `--explain json`. Both are embedded below.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use coding_tools::explain::Format;
use coding_tools::outline::{Entry, language_for, outline};
use coding_tools::pulse::{self, HeartbeatOpts, PulseState};
use coding_tools::verdict::Expect;
use coding_tools::walk::{self, EntryType};
use coding_tools::{pattern, template};
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-outline.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-outline.json");

#[derive(Parser, Debug)]
#[command(
    name = "ct-outline",
    version,
    about = "Report the declarations in a file or tree: kind, name, start:end span, and nesting.",
    long_about = "ct-outline detects declarations heuristically per language (Rust, Python, Markdown) \
                  and reports each with its kind, name, and 1-based start:end line span (also \
                  reachable as `ct outline`) — locate a symbol, then read exactly that region with \
                  ct-view --range. Start lines are exact; an underivable end renders as start:?. \
                  See `ct-outline --explain` for agent-oriented documentation."
)]
struct Cli {
    /// Root to outline; a file outlines just that file, a directory is descended.
    #[arg(long, default_value = ".")]
    base: PathBuf,

    /// Limit to files whose name matches; '|'-separated alternatives, each substring->glob->regex promoted and anchored.
    #[arg(long)]
    name: Option<String>,

    /// Restrict to these extensions (comma-separated, no dots), e.g. --ext rs,py. Combined with --name as alternatives.
    #[arg(long, value_delimiter = ',')]
    ext: Vec<String>,

    /// Include dot-entries (names starting with '.'); default skips them.
    #[arg(long)]
    hidden: bool,

    /// Follow symlinks while traversing.
    #[arg(long)]
    follow: bool,

    /// Keep entries whose name matches (substring->glob->regex promoted, anchored to the whole declaration name).
    #[arg(long = "match")]
    pattern: Option<String>,

    /// Pin how --match/--name patterns are interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    mode: Option<pattern::Mode>,

    /// Keep entries of these kinds (comma-separated), e.g. --kind fn,struct. Kinds are per-language keywords.
    #[arg(long, value_delimiter = ',')]
    kind: Vec<String>,

    /// Keep entries nested at most N levels deep (1 = top-level only).
    #[arg(long)]
    depth: Option<usize>,

    /// Output one grep-friendly row per matched entry: path:start:end:kind:name.
    #[arg(long)]
    flat: bool,

    /// Question this outline answers, framing it as a test; printed as a "== ... ==" banner unless --quiet.
    #[arg(long)]
    question: Option<String>,

    /// Verdict expectation over the matched-entry count: any|none|N|=N|+N|-N (default: any).
    #[arg(long)]
    expect: Option<String>,

    /// Template written to stdout after the outline. Tokens: {RESULT} {QUESTION} {COUNT} {BASE} {MATCHES}.
    #[arg(long, alias = "emit-stdout")]
    emit: Option<String>,

    /// Template written to stderr after the outline (same tokens as --emit).
    #[arg(long)]
    emit_stderr: Option<String>,

    /// Print nothing; report via exit status (and --emit, which still fires).
    #[arg(long)]
    quiet: bool,

    /// Emit a structured JSON result instead of text (overrides the text modes and --emit).
    #[arg(long)]
    json: bool,

    /// Abort with exit 2 if the run exceeds SECS seconds (fractional allowed).
    #[arg(long, value_name = "SECS")]
    timeout: Option<f64>,

    #[command(flatten)]
    heartbeat: HeartbeatOpts,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    explain: Option<Format>,
}

/// One file's outline with per-entry match flags.
struct FileOutline {
    path: String,
    entries: Vec<Entry>,
    matched: Vec<bool>,
}

/// `start:end` with `?` for an underivable end.
fn span(e: &Entry) -> String {
    match e.end {
        Some(end) => format!("{}:{}", e.start, end),
        None => format!("{}:?", e.start),
    }
}

/// The grep-friendly `path:start:end:kind:name` row.
fn flat_row(path: &str, e: &Entry) -> String {
    let end = e.end.map_or("?".to_string(), |n| n.to_string());
    format!("{path}:{}:{end}:{}:{}", e.start, e.kind, e.name)
}

/// Indices to display in tree mode: every matched entry plus its ancestors
/// (by depth-stack reconstruction over the ordered entries), in source order.
fn with_context(entries: &[Entry], matched: &[bool]) -> Vec<(usize, bool)> {
    let mut keep = vec![false; entries.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        while let Some(&top) = stack.last() {
            if entries[top].depth >= e.depth {
                stack.pop();
            } else {
                break;
            }
        }
        if matched[i] {
            keep[i] = true;
            for &a in &stack {
                keep[a] = true;
            }
        }
        stack.push(i);
    }
    keep.iter()
        .enumerate()
        .filter(|(_, k)| **k)
        .map(|(i, _)| (i, matched[i]))
        .collect()
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let _watchdog = pulse::watchdog("ct-outline", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-outline", PulseState::new())?;

    // --ext is sugar for additional name alternatives, as in ct-tree.
    let mut name_spec = cli.name.clone().unwrap_or_default();
    for e in &cli.ext {
        let e = e.trim().trim_start_matches('.');
        if e.is_empty() {
            continue;
        }
        if !name_spec.is_empty() {
            name_spec.push('|');
        }
        name_spec.push_str(&format!("*.{e}"));
    }
    let names = if name_spec.is_empty() {
        None
    } else {
        Some(
            pattern::compile_name_set_with(&name_spec, cli.mode)
                .map_err(|e| format!("invalid --name/--ext pattern: {e}"))?,
        )
    };
    let match_re = match &cli.pattern {
        Some(p) => Some(
            pattern::compile_anchored_with(p, cli.mode)
                .map_err(|e| format!("invalid --match pattern: {e}"))?,
        ),
        None => None,
    };
    let expect = match &cli.expect {
        Some(s) => Expect::parse(s).map_err(|e| format!("invalid --expect: {e}"))?,
        None => Expect::default(),
    };
    let base_is_file = cli.base.is_file();

    let selector = walk::Selector {
        base: cli.base.clone(),
        names,
        types: vec![EntryType::F],
        size: None,
        hidden: cli.hidden,
        follow: cli.follow,
    };

    let keeps = |e: &Entry| -> bool {
        if let Some(re) = &match_re
            && !re.is_match(&e.name)
        {
            return false;
        }
        if !cli.kind.is_empty() && !cli.kind.iter().any(|k| k == &e.kind) {
            return false;
        }
        if let Some(d) = cli.depth
            && e.depth > d
        {
            return false;
        }
        true
    };

    let mut files: Vec<FileOutline> = Vec::new();
    let mut count = 0usize;
    for entry in selector.walk() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry
            .path()
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(lang) = language_for(&ext) else {
            if base_is_file {
                return Err(format!(
                    "no outline rules for '{}' (recognised: rs, py, md)",
                    entry.path().display()
                ));
            }
            continue; // unrecognised language: skipped in a walk
        };
        let text = match std::fs::read_to_string(entry.path()) {
            Ok(t) => t,
            Err(e) if base_is_file => {
                return Err(format!("read {}: {e}", entry.path().display()));
            }
            Err(_) => continue, // unreadable / non-UTF-8 in a walk: skipped
        };
        let entries = outline(lang, &text);
        let matched: Vec<bool> = entries.iter().map(keeps).collect();
        let n = matched.iter().filter(|m| **m).count();
        if n == 0 {
            continue;
        }
        count += n;
        files.push(FileOutline {
            path: entry.path().display().to_string(),
            entries,
            matched,
        });
    }

    let verdict = expect.eval(count as u64);

    if cli.json {
        let file_objs: Vec<_> = files
            .iter()
            .map(|f| {
                let entry_objs: Vec<_> = f
                    .entries
                    .iter()
                    .zip(&f.matched)
                    .filter(|(_, m)| **m)
                    .map(|(e, _)| {
                        json!({
                            "kind": e.kind,
                            "name": e.name,
                            "start": e.start,
                            "end": e.end,
                            "depth": e.depth,
                        })
                    })
                    .collect();
                json!({ "path": f.path, "entries": entry_objs })
            })
            .collect();
        let obj = json!({
            "tool": "ct-outline",
            "verdict": verdict.label(),
            "base": cli.base.display().to_string(),
            "count": count,
            "files": file_objs,
        });
        println!("{obj}");
        return Ok(verdict.exit_code());
    }

    if !cli.quiet
        && let Some(q) = &cli.question
    {
        println!("== {q} ==");
    }

    if !cli.quiet {
        if cli.flat {
            for f in &files {
                for (e, m) in f.entries.iter().zip(&f.matched) {
                    if *m {
                        println!("{}", flat_row(&f.path, e));
                    }
                }
            }
        } else {
            for f in &files {
                println!("{}", f.path);
                for (i, is_match) in with_context(&f.entries, &f.matched) {
                    let e = &f.entries[i];
                    let indent = "  ".repeat(e.depth);
                    let note = if is_match { "" } else { "      (context)" };
                    println!("{indent}{:<9} {:<7} {}{note}", span(e), e.kind, e.name);
                }
            }
        }
    }

    if cli.emit.is_some() || cli.emit_stderr.is_some() {
        let count_s = count.to_string();
        let base_s = cli.base.display().to_string();
        let matches_joined = files
            .iter()
            .flat_map(|f| {
                f.entries
                    .iter()
                    .zip(&f.matched)
                    .filter(|(_, m)| **m)
                    .map(|(e, _)| flat_row(&f.path, e))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let tokens = [
            ("RESULT", verdict.label()),
            ("QUESTION", cli.question.as_deref().unwrap_or("")),
            ("COUNT", count_s.as_str()),
            ("BASE", base_s.as_str()),
            ("MATCHES", matches_joined.as_str()),
        ];
        if let Some(t) = &cli.emit {
            println!("{}", template::render(t, &tokens));
        }
        if let Some(t) = &cli.emit_stderr {
            eprintln!("{}", template::render(t, &tokens));
        }
    }

    Ok(verdict.exit_code())
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
            eprintln!("ct-outline: {msg}");
            ExitCode::from(2)
        }
    }
}
