// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-edit` — declarative, verifiable text edits.
//!
//! A find/replace that *asserts its own effect*: it targets files with the same
//! predicates as `ct-search`, computes every replacement first, classifies the
//! total against an `--expect`ation into a `SUCCESS`/`ERROR` verdict, and only
//! writes when the verdict holds (never under `--dry-run`). `--find`/`--replace`
//! accept `file:PATH` / `text:VALUE` payloads; a multi-line find matches as a
//! line-anchored literal block. `--script` runs a `.ctb` batch of edits under
//! the prepare/confirm/write standard: the whole script is simulated in memory
//! and judged first, and no file changes unless every edit passes. Reachable
//! directly or as `ct edit`. The canonical reference is `docs/explain/ct-edit.md`
//! — the text this tool emits for `--explain md`; `docs/explain/ct-edit.json` is
//! the MCP tool-use definition emitted for `--explain json`. Both are embedded
//! below.

use std::process::ExitCode;

use clap::Parser;
use coding_tools::cli::ct_edit::Cli;
use coding_tools::edit::Site;
use coding_tools::editscript::{self, EditOutcome, FileBuf, Op};
use coding_tools::explain::Format;
use coding_tools::pulse::{self, PulseState, Watchdog};
use coding_tools::verdict::{Expect, Verdict};
use coding_tools::walk::{self, EntryType};
use coding_tools::{blockdoc, pattern, payload};
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-edit.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-edit.json");

/// Build the file selector shared by both forms.
fn selector(cli: &Cli) -> Result<walk::Selector, String> {
    let names = match &cli.name {
        Some(spec) => Some(
            pattern::compile_name_set_with(spec, cli.mode)
                .map_err(|e| format!("invalid --name pattern: {e}"))?,
        ),
        None => None,
    };
    Ok(walk::Selector {
        base: cli.base.clone(),
        names,
        types: vec![EntryType::F],
        size: None,
        hidden: cli.hidden,
        follow: cli.follow,
        no_ignore: cli.no_ignore,
    })
}

/// Read every selected UTF-8 file into memory. Files that do not read as
/// UTF-8 text (e.g. binaries) are left out, hence untouched.
fn load_files(sel: &walk::Selector) -> Result<Vec<FileBuf>, String> {
    let mut files = Vec::new();
    for entry in sel.walk() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            files.push(FileBuf {
                path: entry.path().display().to_string(),
                content,
            });
        }
    }
    Ok(files)
}

/// Confirm every changed file can be written before any write begins — the
/// pre-flight half of the prepare/confirm/write standard.
fn preflight(paths: &[&str]) -> Result<(), String> {
    for p in paths {
        let meta = std::fs::metadata(p)
            .map_err(|e| format!("write pre-flight: {p}: {e}; nothing written"))?;
        if meta.permissions().readonly() {
            return Err(format!(
                "write pre-flight: {p} is not writable; nothing written"
            ));
        }
    }
    Ok(())
}

/// Write every changed buffer. The watchdog is disarmed first: a write phase,
/// once begun, always completes.
fn write_changed(
    watchdog: &Option<Watchdog>,
    files: &[FileBuf],
    changed: &[bool],
) -> Result<(), String> {
    if let Some(w) = watchdog {
        w.disarm();
    }
    for (f, ch) in files.iter().zip(changed) {
        if *ch {
            std::fs::write(&f.path, &f.content).map_err(|e| format!("writing {}: {e}", f.path))?;
        }
    }
    Ok(())
}

/// Print sites as per-line diff rows, multi-line aware; `tag` prefixes each
/// row (empty for the argv form, `[i/N] ` for scripts).
fn print_sites(tag: &str, sites: &[Site]) {
    for s in sites {
        for l in s.before.lines() {
            println!("{tag}{}:{}:- {}", s.path, s.line, l);
        }
        for l in s.after.lines() {
            println!("{tag}{}:{}:+ {}", s.path, s.line, l);
        }
    }
}

fn site_json(s: &Site) -> serde_json::Value {
    json!({ "path": s.path, "line": s.line, "before": s.before, "after": s.after })
}

/// Compile the argv `--find`/`--replace` pair into an [`Op`], resolving the
/// payload schemes. A `file:`-sourced find defaults to literal; a multi-line
/// find is a literal block.
fn compile_argv_op(cli: &Cli) -> Result<Op, String> {
    let (Some(find_raw), Some(replace_raw)) = (cli.find.as_deref(), cli.replace.as_deref())
    else {
        return Err("missing --find/--replace (or run a batch with --script)".to_string());
    };
    let find = payload::resolve(find_raw)?;
    let replace = payload::resolve(replace_raw)?;
    let find_lines = payload::to_lines(&find.text);
    match find_lines.len() {
        0 => Err("empty --find payload".to_string()),
        1 => {
            let effective = cli
                .mode
                .or(find.from_file.then_some(pattern::Mode::Literal));
            let single = find_lines.into_iter().next().unwrap();
            let re = pattern::compile_with(&single, effective)
                .map_err(|e| format!("invalid --find pattern: {e}"))?;
            let literal = !matches!(
                pattern::classify_with(&single, effective),
                pattern::PatternKind::Regex
            );
            let text = replace.text.as_str();
            Ok(Op::Line {
                re,
                literal,
                replace: text.strip_suffix('\n').unwrap_or(text).to_string(),
            })
        }
        _ => {
            if matches!(
                cli.mode,
                Some(pattern::Mode::Glob) | Some(pattern::Mode::Regex)
            ) {
                return Err(
                    "a multi-line --find matches as a literal block; --mode glob/regex is reserved"
                        .to_string(),
                );
            }
            Ok(Op::Block {
                find: find_lines,
                replace: payload::to_lines(&replace.text),
            })
        }
    }
}

/// The single-edit argv form: one op over the selection, verdict over the
/// total count, write only on SUCCESS.
fn run_single(cli: &Cli, watchdog: &Option<Watchdog>) -> Result<ExitCode, String> {
    let op = compile_argv_op(cli)?;
    let expect = match &cli.expect {
        Some(s) => Expect::parse(s).map_err(|e| format!("invalid --expect: {e}"))?,
        None => Expect::default(),
    };

    let mut files = load_files(&selector(cli)?)?;
    let mut replacements = 0usize;
    let mut sites: Vec<Site> = Vec::new();
    let mut changed = vec![false; files.len()];
    let mut miss: Option<(String, coding_tools::block::NearestMiss)> = None;

    for (i, f) in files.iter_mut().enumerate() {
        let (new_content, hits, file_sites) = op.apply(&f.path, &f.content);
        replacements += hits;
        if hits > 0 && new_content != f.content {
            f.content = new_content;
            changed[i] = true;
            sites.extend(file_sites);
        } else if hits == 0
            && let Op::Block { find, .. } = &op
        {
            let lines: Vec<&str> = f.content.lines().collect();
            if let Some(m) = coding_tools::block::nearest_miss(&lines, find)
                && miss
                    .as_ref()
                    .is_none_or(|(_, b)| m.first_diverging_line > b.first_diverging_line)
            {
                miss = Some((f.path.clone(), m));
            }
        }
    }

    if replacements == 0
        && !cli.json
        && let Some((path, m)) = &miss
    {
        eprintln!(
            "ct-edit: nearest miss: {path}:{}: block diverges at its line {}",
            m.line, m.first_diverging_line
        );
        eprintln!("ct-edit:   expected: {}", m.expected);
        eprintln!("ct-edit:   found:    {}", m.found);
    }

    let verdict = expect.eval(replacements as u64);
    let applied = verdict == Verdict::Success && !cli.dry_run;
    if applied {
        let to_write: Vec<&str> = files
            .iter()
            .zip(&changed)
            .filter(|(_, ch)| **ch)
            .map(|(f, _)| f.path.as_str())
            .collect();
        preflight(&to_write)?;
        write_changed(watchdog, &files, &changed)?;
    }

    let files_changed = changed.iter().filter(|c| **c).count();
    if cli.json {
        let mut obj = json!({
            "tool": "ct-edit",
            "verdict": verdict.label(),
            "dry_run": cli.dry_run,
            "applied": applied,
            "replacements": replacements,
            "files_changed": files_changed,
            "sites": sites.iter().map(site_json).collect::<Vec<_>>(),
        });
        if let Some((path, m)) = &miss
            && replacements == 0
        {
            obj["nearest_miss"] = miss_json(path, m);
        }
        coding_tools::jsonout::print(&obj, cli.json_pretty);
    } else {
        if !cli.quiet {
            print_sites("", &sites);
        }
        println!(
            "{} replacement(s) in {} file(s) -> {} ({})",
            replacements,
            files_changed,
            verdict.label(),
            status_label(applied, cli.dry_run),
        );
    }

    Ok(verdict.exit_code())
}

fn status_label(applied: bool, dry_run: bool) -> &'static str {
    if applied {
        "applied"
    } else if dry_run {
        "dry-run, not written"
    } else {
        "verdict ERROR, not written"
    }
}

fn miss_json(path: &str, m: &coding_tools::block::NearestMiss) -> serde_json::Value {
    json!({
        "path": path,
        "line": m.line,
        "first_diverging_line": m.first_diverging_line,
        "expected": m.expected,
        "found": m.found,
    })
}

/// The `--script` form: parse the `.ctb` document, simulate the whole batch
/// in memory, and write only when every edit passed — atomic by design, with
/// no flag that makes a partial write possible.
fn run_script(cli: &Cli, watchdog: &Option<Watchdog>) -> Result<ExitCode, String> {
    let script_path = cli.script.as_ref().unwrap();
    let src = std::fs::read_to_string(script_path)
        .map_err(|e| format!("reading script {}: {e}", script_path.display()))?;
    let items = blockdoc::parse(&src, &cli.fence, &["edit"])
        .map_err(|e| format!("{}: {e}", script_path.display()))?;
    if items.is_empty() {
        return Err(format!(
            "{}: script contains no edits",
            script_path.display()
        ));
    }
    let specs = items
        .iter()
        .enumerate()
        .map(|(i, it)| editscript::compile_item(it, i + 1))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{}: {e}", script_path.display()))?;

    let mut files = load_files(&selector(cli)?)?;
    let pristine: Vec<String> = files.iter().map(|f| f.content.clone()).collect();

    // Phase 1: the whole batch, simulated and judged in memory.
    let outcomes = if cli.no_cascade {
        editscript::run_no_cascade(&specs, &mut files)?
    } else {
        editscript::run_cascade(&specs, &mut files)?
    };
    let batch_ok = outcomes.iter().all(|o| o.verdict == Verdict::Success);
    let changed: Vec<bool> = files
        .iter()
        .zip(&pristine)
        .map(|(f, p)| f.content != *p)
        .collect();
    let replacements: usize = outcomes.iter().map(|o| o.replacements).sum();
    let files_changed = changed.iter().filter(|c| **c).count();

    // Phase 2: confirm writability, then write — only when every edit passed.
    let applied = batch_ok && !cli.dry_run;
    if applied {
        let to_write: Vec<&str> = files
            .iter()
            .zip(&changed)
            .filter(|(_, ch)| **ch)
            .map(|(f, _)| f.path.as_str())
            .collect();
        preflight(&to_write)?;
        write_changed(watchdog, &files, &changed)?;
    }

    let verdict = if batch_ok {
        Verdict::Success
    } else {
        Verdict::Error
    };
    let total = specs.len();

    if cli.json {
        let edits: Vec<_> = outcomes.iter().map(outcome_json).collect();
        let obj = json!({
            "tool": "ct-edit",
            "script": script_path.display().to_string(),
            "verdict": verdict.label(),
            "cascade": !cli.no_cascade,
            "dry_run": cli.dry_run,
            "applied": applied,
            "replacements": replacements,
            "files_changed": files_changed,
            "edits": edits,
        });
        coding_tools::jsonout::print(&obj, cli.json_pretty);
    } else {
        if !cli.quiet {
            for o in &outcomes {
                print_sites(&format!("[{}/{total}] ", o.ordinal), &o.sites);
            }
            for o in &outcomes {
                println!(
                    "edit {}/{total}: expect {}, mode {} -> {} ({} replacement(s))",
                    o.ordinal,
                    o.expect,
                    o.mode,
                    o.verdict.label(),
                    o.replacements,
                );
                if let Some((path, m)) = &o.miss {
                    println!(
                        "  nearest miss: {path}:{}: block diverges at its line {}",
                        m.line, m.first_diverging_line
                    );
                    println!("    expected: {}", m.expected);
                    println!("    found:    {}", m.found);
                }
            }
        }
        println!(
            "{} replacement(s) in {} file(s) over {} edit(s) -> {} ({})",
            replacements,
            files_changed,
            total,
            verdict.label(),
            status_label(applied, cli.dry_run),
        );
    }

    Ok(verdict.exit_code())
}

fn outcome_json(o: &EditOutcome) -> serde_json::Value {
    let mut obj = json!({
        "ordinal": o.ordinal,
        "expect": o.expect,
        "mode": o.mode,
        "replacements": o.replacements,
        "verdict": o.verdict.label(),
        "sites": o.sites.iter().map(site_json).collect::<Vec<_>>(),
    });
    if let Some((path, m)) = &o.miss {
        obj["nearest_miss"] = miss_json(path, m);
    }
    obj
}

fn run(mut cli: Cli) -> Result<ExitCode, String> {
    // --json-pretty enables JSON output on its own; treat it as --json
    // everywhere the text path is gated.
    if cli.json_pretty {
        cli.json = true;
    }
    let watchdog = pulse::watchdog("ct-edit", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-edit", PulseState::new())?;
    if cli.script.is_some() {
        run_script(&cli, &watchdog)
    } else {
        run_single(&cli, &watchdog)
    }
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
