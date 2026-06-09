// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-edit` — declarative, verifiable text edits.
//!
//! A find/replace that *asserts its own effect*: it targets files with the same
//! predicates as `ct-search`, computes every replacement first, classifies the
//! total against an `--expect`ation into a `SUCCESS`/`ERROR` verdict, and only
//! writes when the verdict holds (never under `--dry-run`). Reachable directly
//! or as `ct edit`. The canonical reference is `docs/explain/ct-edit.md` — the
//! text this tool emits for `--explain md`; `docs/explain/ct-edit.json` is the
//! MCP tool-use definition emitted for `--explain json`. Both are embedded below.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use coding_tools::edit::{Site, edit_content};
use coding_tools::explain::Format;
use coding_tools::pattern::{self, PatternKind};
use coding_tools::verdict::{Expect, Verdict};
use coding_tools::walk::{self, EntryType};
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-edit.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-edit.json");

#[derive(Parser, Debug)]
#[command(
    name = "ct-edit",
    version,
    about = "Find/replace across selected files, gated by an --expect verdict and previewable with --dry-run.",
    long_about = "ct-edit applies a find/replace to the files chosen by ct-search-style predicates \
                  (also reachable as `ct edit`). It computes every replacement first, classifies \
                  the total against --expect, and writes only when the verdict is SUCCESS and \
                  --dry-run is not set. See `ct-edit --explain` for agent-oriented documentation."
)]
struct Cli {
    /// Search root (relative or absolute); a file edits just that file, a directory is descended.
    #[arg(long, default_value = ".")]
    base: PathBuf,

    /// Limit to files whose name matches; '|'-separated alternatives, each substring->glob->regex promoted and anchored.
    #[arg(long)]
    name: Option<String>,

    /// Include dot-entries (names starting with '.'); default skips them.
    #[arg(long)]
    hidden: bool,

    /// Follow symlinks while traversing.
    #[arg(long)]
    follow: bool,

    /// Pattern to find (substring->glob->regex promoted); matched per line.
    #[arg(long)]
    find: String,

    /// Replacement text. With a regex --find, $1/${name} expand; with a literal or glob --find, it is literal.
    #[arg(long)]
    replace: String,

    /// Verdict expectation over the total replacement count: any|none|N|=N|+N|-N (default: any).
    #[arg(long)]
    expect: Option<String>,

    /// Show what would change and the verdict, but write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Suppress the per-site diff; print only the summary line.
    #[arg(long)]
    quiet: bool,

    /// Emit a structured JSON result instead of text.
    #[arg(long)]
    json: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    explain: Option<Format>,
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let re = pattern::compile(&cli.find).map_err(|e| format!("invalid --find pattern: {e}"))?;
    let literal = !matches!(pattern::classify(&cli.find), PatternKind::Regex);
    let names = match &cli.name {
        Some(spec) => Some(
            pattern::compile_name_set(spec).map_err(|e| format!("invalid --name pattern: {e}"))?,
        ),
        None => None,
    };
    let expect = match &cli.expect {
        Some(s) => Expect::parse(s).map_err(|e| format!("invalid --expect: {e}"))?,
        None => Expect::default(),
    };

    let selector = walk::Selector {
        base: cli.base.clone(),
        names,
        types: vec![EntryType::F],
        size: None,
        hidden: cli.hidden,
        follow: cli.follow,
    };

    let mut replacements = 0usize;
    let mut sites: Vec<Site> = Vec::new();
    let mut changed: Vec<(PathBuf, String)> = Vec::new();

    for entry in selector.walk() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        // Files we cannot read as UTF-8 text (e.g. binaries) are left untouched.
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let path = entry.path().display().to_string();
        let (new_content, hits, file_sites) =
            edit_content(&path, &content, &re, &cli.replace, literal);
        replacements += hits;
        if new_content != content {
            changed.push((entry.path().to_path_buf(), new_content));
            sites.extend(file_sites);
        }
    }

    let verdict = expect.eval(replacements as u64);
    // Write only when the expectation held and this is not a preview.
    let applied = verdict == Verdict::Success && !cli.dry_run;
    if applied {
        for (path, content) in &changed {
            std::fs::write(path, content)
                .map_err(|e| format!("writing {}: {e}", path.display()))?;
        }
    }

    if cli.json {
        let site_objs: Vec<_> = sites
            .iter()
            .map(
                |s| json!({ "path": s.path, "line": s.line, "before": s.before, "after": s.after }),
            )
            .collect();
        let obj = json!({
            "tool": "ct-edit",
            "verdict": verdict.label(),
            "dry_run": cli.dry_run,
            "applied": applied,
            "replacements": replacements,
            "files_changed": changed.len(),
            "sites": site_objs,
        });
        println!("{obj}");
    } else {
        if !cli.quiet {
            for s in &sites {
                println!("{}:{}:- {}", s.path, s.line, s.before);
                println!("{}:{}:+ {}", s.path, s.line, s.after);
            }
        }
        let status = if applied {
            "applied"
        } else if cli.dry_run {
            "dry-run, not written"
        } else {
            "verdict ERROR, not written"
        };
        println!(
            "{} replacement(s) in {} file(s) -> {} ({status})",
            replacements,
            changed.len(),
            verdict.label(),
        );
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
            eprintln!("ct-edit: {msg}");
            ExitCode::from(2)
        }
    }
}
