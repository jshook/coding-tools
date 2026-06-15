// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-patch` — structured, format-preserving edits for JSON / JSONC / JSONL / YAML.
//!
//! Address a node by path and `--set`, `--add`, `--delete`, or `--move-*` it;
//! array elements can be selected by index or by an object predicate
//! (`[key=value]`). For JSON/JSONC/JSONL, edits are **byte-range splices** against
//! the parsed tree, so everything outside the changed node — comments,
//! indentation, key order, blank lines, trailing commas — is preserved exactly.
//! YAML uses the pure-Rust `yaml-edit` backend (comment-preserving; structural
//! edits may relocate an adjacent comment). Like `ct-edit`, it is framed by
//! `--expect` and previewable with `--dry-run`, and writes only when the verdict
//! holds. Reachable directly or as `ct patch`. The canonical reference is
//! `docs/explain/ct-patch.md`; `docs/explain/ct-patch.json` is the MCP tool-use
//! definition. Both are embedded below.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use coding_tools::cli::ct_patch::{Cli, DocFormat};
use coding_tools::explain::Format;
use coding_tools::patch::{
    MoveTo, Op, apply_doc, apply_jsonl, apply_yaml, normalize_value, parse_path, split_assign,
};
use coding_tools::payload;
use coding_tools::pulse::{self, PulseState};
use coding_tools::verdict::{Expect, Verdict};
use coding_tools::walk::{self, EntryType};
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-patch.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-patch.json");

/// Resolve a VALUE through the payload schemes: a `file:`-sourced value is
/// taken verbatim as a string node (never re-parsed as JSON); anything else
/// is parsed as JSON, or taken as a string if it is not valid JSON.
fn patch_value(v: &str) -> Result<String, String> {
    let r = payload::resolve(v)?;
    Ok(if r.from_file {
        serde_json::Value::String(r.text).to_string()
    } else {
        normalize_value(&r.text)
    })
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let watchdog = pulse::watchdog("ct-patch", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-patch", PulseState::new())?;
    let mut ops: Vec<Op> = Vec::new();
    for spec in &cli.set {
        let (p, v) =
            split_assign(spec).ok_or_else(|| format!("--set needs PATH=VALUE, got '{spec}'"))?;
        ops.push(Op::Set {
            path: parse_path(p)?,
            raw: p.to_string(),
            value: patch_value(v)?,
        });
    }
    for spec in &cli.add {
        let (p, v) =
            split_assign(spec).ok_or_else(|| format!("--add needs PATH=VALUE, got '{spec}'"))?;
        ops.push(Op::Add {
            path: parse_path(p)?,
            raw: p.to_string(),
            value: patch_value(v)?,
        });
    }
    for (specs, to) in [
        (&cli.move_first, MoveTo::First),
        (&cli.move_last, MoveTo::Last),
        (&cli.move_up, MoveTo::Up),
        (&cli.move_down, MoveTo::Down),
    ] {
        for spec in specs {
            ops.push(Op::Move {
                path: parse_path(spec)?,
                raw: spec.to_string(),
                to,
            });
        }
    }
    for spec in &cli.delete {
        ops.push(Op::Delete {
            path: parse_path(spec)?,
            raw: spec.to_string(),
        });
    }
    if ops.is_empty() {
        return Err(
            "nothing to do: supply at least one --set, --add, --move-*, or --delete".to_string(),
        );
    }

    let expect = match &cli.expect {
        Some(s) => Expect::parse(s).map_err(|e| format!("invalid --expect: {e}"))?,
        None => Expect::default(),
    };
    let names = match &cli.name {
        Some(spec) => Some(
            coding_tools::pattern::compile_name_set_with(spec, cli.mode)
                .map_err(|e| format!("invalid --name pattern: {e}"))?,
        ),
        None => None,
    };
    let selector = walk::Selector {
        base: cli.base.clone(),
        names,
        types: vec![EntryType::F],
        size: None,
        hidden: cli.hidden,
        follow: cli.follow,
    };

    let mut total_changes = 0usize;
    let mut changed_files: Vec<(PathBuf, String, usize)> = Vec::new();

    for entry in selector.walk() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let fmt = match cli.format.or_else(|| {
            entry
                .path()
                .extension()
                .and_then(|e| DocFormat::from_ext(&e.to_string_lossy()))
        }) {
            Some(f) => f,
            None => continue, // not a recognised structured file
        };
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let path = entry.path().display().to_string();
        let (patched, changes) = match fmt {
            DocFormat::Jsonl => apply_jsonl(&content, &ops),
            DocFormat::Yaml => apply_yaml(&content, &ops),
            _ => apply_doc(&content, &ops),
        }
        .map_err(|e| format!("{path}: {e}"))?;
        total_changes += changes;
        if patched != content {
            changed_files.push((entry.path().to_path_buf(), patched, changes));
        }
    }

    let verdict = expect.eval(total_changes as u64);
    // The timeout bound ends here: a write phase, once begun, always completes.
    if let Some(w) = &watchdog {
        w.disarm();
    }
    let applied = verdict == Verdict::Success && !cli.dry_run;
    if applied {
        for (path, content, _) in &changed_files {
            std::fs::write(path, content)
                .map_err(|e| format!("writing {}: {e}", path.display()))?;
        }
    }

    if cli.json {
        let files: Vec<_> = changed_files
            .iter()
            .map(|(p, _, n)| json!({ "path": p.display().to_string(), "changes": n }))
            .collect();
        let obj = json!({
            "tool": "ct-patch",
            "verdict": verdict.label(),
            "dry_run": cli.dry_run,
            "applied": applied,
            "changes": total_changes,
            "files_changed": changed_files.len(),
            "files": files,
        });
        println!("{obj}");
    } else {
        if !cli.quiet {
            for (path, _, n) in &changed_files {
                println!("{}: {n} change(s)", path.display());
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
            "{total_changes} change(s) in {} file(s) -> {} ({status})",
            changed_files.len(),
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
            eprintln!("ct-patch: {msg}");
            ExitCode::from(2)
        }
    }
}
