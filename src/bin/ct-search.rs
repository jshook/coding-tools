// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-search` — recursive, predicate-based file search.
//!
//! A declarative replacement for `find … | xargs grep …`, reachable directly or
//! as `ct search`. The canonical, self-contained reference is
//! `docs/explain/ct-search.md` — the same text this tool emits for `--explain
//! md`; `docs/explain/ct-search.json` is the MCP tool-use definition emitted for
//! `--explain json`. Both are embedded below.

use std::process::ExitCode;

use coding_tools::block::{self, NearestMiss};
use coding_tools::cli::ct_search::Cli;
use coding_tools::explain::Format;
use coding_tools::pulse::{self, PulseState};
use coding_tools::verdict::Expect;
use coding_tools::view::{expand_and_merge, segments};
use coding_tools::walk;
use coding_tools::{pattern, payload, template};
use regex::Regex;
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-search.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-search.json");

/// Resolved output mode, derived from the mutually-exclusive output flags.
enum Mode {
    List,
    Summary,
    Detail,
    Quiet,
}

impl Mode {
    fn from(cli: &Cli) -> Mode {
        if cli.summary {
            Mode::Summary
        } else if cli.detail || cli.context.is_some() {
            Mode::Detail
        } else if cli.quiet {
            Mode::Quiet
        } else {
            Mode::List
        }
    }
}

/// How `--grep` matches a file: a single-line pattern (regex after promotion
/// or an explicit `--mode`), or a multi-line line-anchored literal block.
enum Grep {
    Line(Regex),
    Block(Vec<String>),
}

/// Compile a resolved `--grep` payload. A `file:`-sourced pattern defaults to
/// literal; a multi-line payload is a literal block (an explicit non-literal
/// `--mode` on a block is a usage error).
fn compile_grep(resolved: &payload::Resolved, mode: Option<pattern::Mode>) -> Result<Grep, String> {
    let lines = payload::to_find_lines(&resolved.text);
    if lines.len() > 1 {
        if matches!(mode, Some(pattern::Mode::Glob) | Some(pattern::Mode::Regex)) {
            return Err(
                "a multi-line pattern matches as a literal block; --mode glob/regex is reserved"
                    .to_string(),
            );
        }
        return Ok(Grep::Block(lines));
    }
    let effective = mode.or(resolved.from_file.then_some(pattern::Mode::Literal));
    let single = lines.into_iter().next().unwrap_or_default();
    pattern::compile_with(&single, effective)
        .map(Grep::Line)
        .map_err(|e| format!("invalid --grep pattern: {e}"))
}

/// Print one file's hits with `ctx` lines of context, in grep's own shape: a
/// hit row is `path:line:text`, a context row is `path-line-text`, and a `--`
/// separates non-adjacent windows (including across files, which is why
/// `opened` is threaded through the whole run rather than reset per file).
///
/// The distinction between `:` and `-` is what makes the output re-greppable:
/// `ct search --grep X --context 3` can be filtered back down to just the hits
/// without re-running the search.
fn print_windows(path: &str, file_lines: &[&str], hits: &[usize], ctx: usize, opened: &mut bool) {
    let selected = expand_and_merge(hits, ctx, file_lines.len());
    for (start, end) in segments(&selected) {
        if *opened {
            println!("--");
        }
        *opened = true;
        for (i, line) in file_lines.iter().enumerate().take(end + 1).skip(start) {
            let sep = if hits.binary_search(&i).is_ok() {
                ':'
            } else {
                '-'
            };
            println!("{path}{sep}{}{sep}{line}", i + 1);
        }
    }
}

fn run(mut cli: Cli) -> Result<ExitCode, String> {
    // --json-pretty enables JSON output on its own; treat it as --json
    // everywhere the text path is gated.
    if cli.json_pretty {
        cli.json = true;
    }
    let _watchdog = pulse::watchdog("ct-search", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-search", PulseState::new())?;
    let names = match &cli.name {
        Some(spec) => Some(
            pattern::compile_name_set_with(spec, cli.mode)
                .map_err(|e| format!("invalid --name pattern: {e}"))?,
        ),
        None => None,
    };
    let grep_re = match &cli.grep {
        Some(p) => Some(compile_grep(&payload::resolve(p)?, cli.mode)?),
        None => None,
    };
    let size = match &cli.size {
        Some(s) => Some(walk::parse_size(s)?),
        None => None,
    };
    let expect = match &cli.expect {
        Some(s) => Expect::parse(s).map_err(|e| format!("invalid --expect: {e}"))?,
        None => Expect::default(),
    };
    // OKF frontmatter predicates: an optional `type` matcher and required tags.
    let okf_type_re = match &cli.okf_type {
        Some(t) => Some(
            pattern::compile_with(t, cli.mode).map_err(|e| format!("invalid --okf-type: {e}"))?,
        ),
        None => None,
    };
    let okf_active = okf_type_re.is_some() || !cli.okf_tag.is_empty();

    let selector = walk::Selector {
        base: cli.base.clone(),
        names,
        types: cli.r#type.clone(),
        size,
        hidden: cli.hidden,
        follow: cli.follow,
        no_ignore: cli.no_ignore,
    };

    let mode = Mode::from(&cli);
    let emit_present = cli.emit.is_some() || cli.emit_stderr.is_some();
    let need_lines = (matches!(mode, Mode::Detail | Mode::Summary) || emit_present || cli.json)
        && grep_re.is_some();
    let collect_matches = emit_present || cli.json;
    // Context is a *display* of the hits, so it is only assembled where hits are
    // printed as lines; `--context 0` stays the plain `path:line:text` form.
    let ctx = cli.context.unwrap_or(0);
    let want_context = ctx > 0 && matches!(mode, Mode::Detail) && !cli.json;
    // `--` separates windows across the whole run, not per file.
    let mut opened = false;
    let mut hit_rows: Vec<serde_json::Value> = Vec::new();

    if !cli.json
        && !cli.quiet
        && let Some(q) = &cli.question
    {
        println!("== {q} ==");
    }

    let mut matched = 0usize;
    let mut total_lines = 0usize;
    let mut match_paths: Vec<String> = Vec::new();
    // Best block alignment seen in any non-matching file (deepest divergence
    // wins) — the nearest-miss diagnostic for a clean negative.
    let mut nearest: Option<(String, NearestMiss)> = None;

    for entry in selector.walk() {
        let entry = entry?;

        // OKF predicates gate a candidate before any content match: a file with
        // no conforming frontmatter (or the wrong type/tags) is skipped.
        if okf_active {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Some(parsed) = coding_tools::okf::parse(&text) else {
                continue;
            };
            if let Some(re) = &okf_type_re
                && !parsed.fm.type_.as_deref().is_some_and(|t| re.is_match(t))
            {
                continue;
            }
            if !cli.okf_tag.is_empty() && !cli.okf_tag.iter().all(|t| parsed.fm.tags.contains(t)) {
                continue;
            }
        }

        let mut lines: Vec<(usize, String)> = Vec::new();
        // 0-based indices to *show* — the hit lines themselves, plus the rest of
        // a matched block, which `lines` deliberately counts only once.
        let mut shown: Vec<usize> = Vec::new();
        let mut content = String::new();
        if let Some(grep) = &grep_re {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let bytes = match std::fs::read(entry.path()) {
                Ok(b) => b,
                Err(_) => continue,
            };
            content = String::from_utf8_lossy(&bytes).into_owned();
            match grep {
                Grep::Line(re) => {
                    if !re.is_match(&content) {
                        continue;
                    }
                    if need_lines {
                        for (i, line) in content.lines().enumerate() {
                            if re.is_match(line) {
                                lines.push((i + 1, line.to_string()));
                                shown.push(i);
                            }
                        }
                    }
                }
                Grep::Block(b) => {
                    let file_lines: Vec<&str> = content.lines().collect();
                    let starts = block::find_starts(&file_lines, b);
                    if starts.is_empty() {
                        if let Some(miss) = block::nearest_miss(&file_lines, b)
                            && nearest.as_ref().is_none_or(|(_, n)| {
                                miss.first_diverging_line > n.first_diverging_line
                            })
                        {
                            nearest = Some((entry.path().display().to_string(), miss));
                        }
                        continue;
                    }
                    for s in starts {
                        lines.push((s + 1, file_lines[s].to_string()));
                        // A block hit shows whole, so the window around it is
                        // context on the block, not on its first line.
                        shown.extend(s..(s + b.len()).min(file_lines.len()));
                    }
                }
            }
        }

        matched += 1;
        total_lines += lines.len();
        if collect_matches {
            match_paths.push(entry.path().display().to_string());
        }
        if cli.json && cli.detail {
            for (ln, text) in &lines {
                hit_rows.push(json!({
                    "path": entry.path().display().to_string(),
                    "line": ln,
                    "text": text,
                }));
            }
        }

        if !cli.json {
            match mode {
                Mode::List => println!("{}", entry.path().display()),
                Mode::Detail => {
                    if grep_re.is_none() || lines.is_empty() {
                        println!("{}", entry.path().display());
                    } else if want_context {
                        let file_lines: Vec<&str> = content.lines().collect();
                        shown.sort_unstable();
                        shown.dedup();
                        print_windows(
                            &entry.path().display().to_string(),
                            &file_lines,
                            &shown,
                            ctx,
                            &mut opened,
                        );
                    } else {
                        for (ln, text) in &lines {
                            println!("{}:{}:{}", entry.path().display(), ln, text);
                        }
                    }
                }
                Mode::Summary | Mode::Quiet => {}
            }
        }

        if let Some(limit) = cli.limit
            && matched >= limit
        {
            break;
        }
    }

    if !cli.json
        && let Mode::Summary = mode
    {
        if grep_re.is_some() {
            println!("{matched} file(s) matched, {total_lines} matching line(s)");
        } else {
            println!("{matched} match(es)");
        }
    }

    // A block pattern that matched nothing reports its nearest miss under
    // --detail: where the best candidate aligned and which line diverged.
    if matched == 0
        && matches!(mode, Mode::Detail)
        && let Some((path, m)) = &nearest
    {
        eprintln!(
            "ct-search: nearest miss: {path}:{}: block diverges at its line {} of {}",
            m.line, m.first_diverging_line, m.block_len
        );
        eprintln!("ct-search:   expected: {}", m.expected);
        eprintln!("ct-search:   found:    {}", m.found);
        if let Some(hint) = m.blank_line_hint() {
            eprintln!("ct-search:   note:     {hint}");
        }
    }

    // The verdict generalises the historic exit status: the default `any`
    // expectation passes exactly when something matched, so a plain search is
    // unchanged, while `--expect none` (and the threshold forms) let a search
    // be posed as a pass/fail test.
    let verdict = expect.eval(matched as u64);

    if cli.json {
        let mut obj = json!({
            "tool": "ct-search",
            "verdict": verdict.label(),
            "base": cli.base.display().to_string(),
            "count": matched,
            "lines": total_lines,
            "matches": match_paths,
        });
        if cli.detail {
            obj["hits"] = json!(hit_rows);
        }
        coding_tools::jsonout::print(&obj, cli.json_pretty);
    } else if emit_present {
        let count = matched.to_string();
        let lines = total_lines.to_string();
        let base = cli.base.display().to_string();
        let matches_joined = match_paths.join("\n");
        let tokens = [
            ("RESULT", verdict.label()),
            ("QUESTION", cli.question.as_deref().unwrap_or("")),
            ("BASE", base.as_str()),
            ("COUNT", count.as_str()),
            ("LINES", lines.as_str()),
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
            eprintln!("ct-search: {msg}");
            ExitCode::from(2)
        }
    }
}
