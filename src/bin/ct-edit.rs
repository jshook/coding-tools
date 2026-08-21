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

/// Render a block nearest-miss as self-diagnosing diagnostic lines (without any
/// tool/indent prefix — the caller prepends one). The header names the parsed
/// block's line count, empty `expected`/`found` lines are shown as `(empty
/// line)` so a phantom blank is visible rather than rendering as nothing, and an
/// empty expected line adds a `note:` pointing at the likely stray blank line.
fn render_miss(path: &str, m: &coding_tools::block::NearestMiss) -> Vec<String> {
    let shown = |s: &str| {
        if s.is_empty() {
            "(empty line)".to_string()
        } else {
            s.to_string()
        }
    };
    let mut lines = vec![
        format!(
            "nearest miss: {path}:{}: block diverges at its line {} of {}",
            m.line, m.first_diverging_line, m.block_len
        ),
        format!("  expected: {}", shown(&m.expected)),
        format!("  found:    {}", shown(&m.found)),
    ];
    if let Some(hint) = m.blank_line_hint() {
        lines.push(format!("  note:     {hint}"));
    }
    lines
}

/// A single-line `--find`, remembered as written so a zero-match run can be
/// diagnosed: the raw needle text and the non-literal kind promotion read it as
/// (`None` when it was already literal). A block find carries its own
/// nearest-miss machinery and needs neither.
struct LineFind {
    needle: String,
    promoted_as: Option<&'static str>,
}

/// Render a single-line nearest-miss as diagnostic lines, in the same shape as
/// [`render_miss`]: a positioned header, the pattern as written against the
/// line as found, and a `note:` naming the fix. This is the diagnosis a
/// single-line `--find` used to lack entirely — it failed with a bare
/// `0 replacement(s) -> ERROR`, which reads as "not there" even when the text is
/// sitting in the file and only the *promotion* missed it.
fn render_line_miss(m: &coding_tools::edit::LineMiss) -> Vec<String> {
    // Both rows are quoted: whitespace drift is one of the diagnosed causes, and
    // it is invisible without delimiters.
    vec![
        format!("nearest miss: {}:{}: {}", m.path, m.line, m.headline()),
        format!("  pattern:  '{}'", m.pattern),
        format!("  found:    '{}'", m.found),
        format!("  note:     {}", m.note()),
    ]
}

/// The note for a `--find` whose selection read no files at all: the failure is
/// in `--base`/`--name`, not in the pattern, and saying so stops the caller
/// re-checking an anchor that was never compared to anything.
fn empty_selection_note(cli: &Cli) -> String {
    let name = match &cli.name {
        Some(n) => format!(" --name '{n}'"),
        None => String::new(),
    };
    let mut note = format!(
        "the selection read 0 file(s): --base '{}'{name} — nothing was compared, so the \
         verdict says nothing about the pattern",
        cli.base.display()
    );
    // `--mode` pins *every* pattern in the invocation, `--name` included, so
    // `--mode literal --name '*.rs'` looks for a file literally called `*.rs`.
    // That is the suite's stated rule, and also the likeliest reason a selection
    // that worked a moment ago suddenly reads nothing — name it here rather than
    // leaving the caller to rediscover it.
    if let (Some(mode), Some(n)) = (cli.mode, cli.name.as_deref())
        && !matches!(mode, pattern::Mode::Glob | pattern::Mode::Auto)
        && n.contains(['*', '?', '['])
    {
        note.push_str(&format!(
            "; note that --mode pins every pattern in the call, so --name '{n}' was matched \
             as {} rather than as a glob — drop --mode (a bare --find is already literal), \
             or state --mode auto to keep promotion for --name",
            mode.label()
        ));
    }
    note
}

/// The note for a single-line `--find` that no file came close to: report the
/// scan's shape so the caller can tell "wrong pattern" from "wrong files".
fn no_candidate_note(lf: &LineFind, scanned: usize) -> String {
    let read_as = match lf.promoted_as {
        Some(kind) => format!("a {kind}"),
        None => "a literal".to_string(),
    };
    format!(
        "no line in {scanned} scanned file(s) matched, and none contains the text \
         even ignoring whitespace; the pattern was read as {read_as} \
         (pin it with --mode, or widen/narrow --base/--name)"
    )
}

/// Compile the argv `--find`/`--replace` pair into an [`Op`], resolving the
/// payload schemes. A `file:`/`stdin:`-sourced find defaults to literal; a
/// multi-line find is a literal block. The second half of the pair is the
/// single-line diagnosis context (see [`LineFind`]).
fn compile_argv_op(cli: &Cli) -> Result<(Op, Option<LineFind>), String> {
    let (Some(find_raw), Some(replace_raw)) = (cli.find.as_deref(), cli.replace.as_deref()) else {
        return Err("missing --find/--replace (or run a batch with --script)".to_string());
    };
    let find = payload::resolve(find_raw)?;
    let replace = payload::resolve(replace_raw)?;
    // Anchor lines drop the phantom trailing blank line editors leave behind;
    // the replacement keeps every line (a trailing blank may be intentional).
    let find_lines = payload::to_find_lines(&find.text);
    match find_lines.len() {
        0 => Err("empty --find payload".to_string()),
        1 => {
            // A bare `--find` is an anchor in code, not a query: `(`, `[`, `.`,
            // `?` and `|` are what code is made of, so the default reading is
            // the verbatim one. `--mode auto` asks for the promotion rule back.
            let effective = Some(cli.mode.unwrap_or(pattern::Mode::Literal));
            let single = find_lines.into_iter().next().unwrap();
            let re = pattern::compile_with(&single, effective)
                .map_err(|e| format!("invalid --find pattern: {e}"))?;
            let kind = pattern::classify_with(&single, effective);
            let literal = !matches!(kind, pattern::PatternKind::Regex);
            let text = replace.text.as_str();
            let diag = LineFind {
                promoted_as: match kind {
                    pattern::PatternKind::Literal => None,
                    pattern::PatternKind::Glob => Some("glob"),
                    pattern::PatternKind::Regex => Some("regex"),
                },
                needle: single,
            };
            Ok((
                Op::Line {
                    re,
                    literal,
                    replace: text.strip_suffix('\n').unwrap_or(text).to_string(),
                },
                Some(diag),
            ))
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
            Ok((
                Op::Block {
                    find: find_lines,
                    replace: payload::to_lines(&replace.text),
                    squeeze: cli.squeeze_blank,
                },
                None,
            ))
        }
    }
}

/// The single-edit argv form: one op over the selection, verdict over the
/// total count, write only on SUCCESS.
fn run_single(cli: &Cli, watchdog: &Option<Watchdog>) -> Result<ExitCode, String> {
    let (op, line_find) = compile_argv_op(cli)?;
    let expect = match &cli.expect {
        Some(s) => Expect::parse(s).map_err(|e| format!("invalid --expect: {e}"))?,
        None => Expect::default(),
    };

    let mut files = load_files(&selector(cli)?)?;
    let scanned = files.len();
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
            && let Op::Block { find, squeeze, .. } = &op
        {
            let lines: Vec<&str> = f.content.lines().collect();
            if let Some(m) = coding_tools::block::nearest_miss_with(&lines, find, *squeeze)
                && miss
                    .as_ref()
                    .is_none_or(|(_, b)| m.first_diverging_line > b.first_diverging_line)
            {
                miss = Some((f.path.clone(), m));
            }
        }
    }

    // A single-line --find that matched nothing gets the same positioned
    // diagnosis a block find has always had (see `render_line_miss`).
    let line_miss = (replacements == 0)
        .then_some(line_find.as_ref())
        .flatten()
        .and_then(|lf| {
            coding_tools::edit::diagnose_line_miss(
                files.iter().map(|f| (f.path.as_str(), f.content.as_str())),
                &lf.needle,
                lf.promoted_as,
            )
        });

    if replacements == 0 && !cli.json {
        // Nothing matched because nothing was *read* is a distinct failure from
        // nothing matched in the files that were: say which one happened.
        if scanned == 0 {
            eprintln!("ct-edit: {}", empty_selection_note(cli));
        }
        if let Some((path, m)) = &miss {
            for line in render_miss(path, m) {
                eprintln!("ct-edit: {line}");
            }
        } else if let Some(m) = &line_miss {
            for line in render_line_miss(m) {
                eprintln!("ct-edit: {line}");
            }
        } else if scanned > 0
            && let Some(lf) = &line_find
        {
            eprintln!("ct-edit: {}", no_candidate_note(lf, scanned));
        }
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
        if replacements == 0 {
            obj["files_scanned"] = json!(scanned);
            if let Some((path, m)) = &miss {
                obj["nearest_miss"] = miss_json(path, m);
            } else if let Some(m) = &line_miss {
                obj["nearest_miss"] = line_miss_json(m);
            }
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

/// The single-line nearest-miss, in the same `nearest_miss` slot a block miss
/// uses. `kind` distinguishes them: `"block"` carries the divergence indices,
/// `"line"` carries the diagnosed `cause`.
fn line_miss_json(m: &coding_tools::edit::LineMiss) -> serde_json::Value {
    use coding_tools::edit::LineMissCause;
    let mut obj = json!({
        "kind": "line",
        "path": m.path,
        "line": m.line,
        "pattern": m.pattern,
        "found": m.found,
        "hint": m.note(),
    });
    match &m.cause {
        LineMissCause::Promoted { kind, literal_hits } => {
            obj["cause"] = json!("promoted");
            obj["promoted_as"] = json!(kind);
            obj["literal_hits"] = json!(literal_hits);
        }
        LineMissCause::Whitespace => obj["cause"] = json!("whitespace"),
    }
    obj
}

fn miss_json(path: &str, m: &coding_tools::block::NearestMiss) -> serde_json::Value {
    json!({
        "kind": "block",
        "path": path,
        "line": m.line,
        "first_diverging_line": m.first_diverging_line,
        "block_lines": m.block_len,
        "expected": m.expected,
        "found": m.found,
        "hint": m.blank_line_hint(),
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
                    for line in render_miss(path, m) {
                        println!("  {line}");
                    }
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
            eprintln!("ct-edit: {msg}");
            ExitCode::from(2)
        }
    }
}
