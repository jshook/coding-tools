// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-search` — recursive, predicate-based file search.
//!
//! A declarative replacement for `find … | xargs grep …`, reachable directly or
//! as `ct search`. The canonical, self-contained reference is
//! `docs/explain/ct-search.md` — the same text this tool emits for `--explain
//! md`; `docs/explain/ct-search.json` is the MCP tool-use definition emitted for
//! `--explain json`. Both are embedded below.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use coding_tools::block::{self, NearestMiss};
use coding_tools::explain::Format;
use coding_tools::pulse::{self, HeartbeatOpts, PulseState};
use coding_tools::verdict::Expect;
use coding_tools::walk::{self, EntryType};
use coding_tools::{pattern, payload, template};
use regex::Regex;
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-search.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-search.json");

#[derive(Parser, Debug)]
#[command(
    name = "ct-search",
    version,
    about = "Recursively find files by name, type, size, and content from a chosen root.",
    long_about = "ct-search combines the predicates you would otherwise assemble from find, xargs, \
                  and grep into one declarative command (also reachable as `ct search`). An entry \
                  matches only when every supplied predicate holds. See `ct-search --explain` for \
                  agent-oriented documentation."
)]
#[command(group = clap::ArgGroup::new("output_mode")
    .args(["list", "summary", "detail", "quiet"])
    .multiple(false))]
struct Cli {
    /// Search root (relative or absolute), independent of the current directory.
    #[arg(long, default_value = ".")]
    base: PathBuf,

    /// File-name pattern; '|'-separated alternatives, each substring->glob->regex promoted and anchored to the whole name.
    #[arg(long)]
    name: Option<String>,

    /// Restrict to entry kinds: f=file, d=dir, l=symlink (repeatable or comma-joined).
    #[arg(long, value_enum, value_delimiter = ',')]
    r#type: Vec<EntryType>,

    /// Content pattern (substring->glob->regex promoted); searches file contents. Accepts file:PATH / text:VALUE; a multi-line pattern matches as a line-anchored literal block.
    #[arg(long)]
    grep: Option<String>,

    /// Pin how patterns are interpreted (promotion off): literal, glob, or regex.
    #[arg(long, value_enum)]
    mode: Option<pattern::Mode>,

    /// Size predicate [+|-]N[k|m|g]: +N larger than, -N smaller than, N at least N.
    #[arg(long)]
    size: Option<String>,

    /// Include dot-entries (names starting with '.'); default skips them.
    #[arg(long)]
    hidden: bool,

    /// Follow symlinks while traversing.
    #[arg(long)]
    follow: bool,

    /// Stop after N matches.
    #[arg(long)]
    limit: Option<usize>,

    /// Abort with exit 2 if the search exceeds SECS seconds (fractional allowed).
    #[arg(long, value_name = "SECS")]
    timeout: Option<f64>,

    #[command(flatten)]
    heartbeat: HeartbeatOpts,

    /// Question this search answers, framing it as a test; printed as a "== ... ==" banner unless --quiet.
    #[arg(long)]
    question: Option<String>,

    /// Verdict expectation over the match count: any|none|N|=N|+N|-N (default: any). Turns the search into a pass/fail test whose exit status follows the verdict.
    #[arg(long)]
    expect: Option<String>,

    /// Template written to stdout after the search. Tokens: {RESULT} {QUESTION} {COUNT} {LINES} {BASE} {MATCHES}.
    #[arg(long, alias = "emit-stdout")]
    emit: Option<String>,

    /// Template written to stderr after the search (same tokens as --emit).
    #[arg(long)]
    emit_stderr: Option<String>,

    /// Output mode: print one matching path per line (default).
    #[arg(long)]
    list: bool,

    /// Output mode: print counts only.
    #[arg(long)]
    summary: bool,

    /// Output mode: print matches plus, for --grep, each hit as path:line:text.
    #[arg(long)]
    detail: bool,

    /// Output mode: print nothing; report via exit status only.
    #[arg(long)]
    quiet: bool,

    /// Emit a structured JSON result instead of text (overrides the output mode and --emit).
    #[arg(long)]
    json: bool,

    /// Print agent usage docs (md or json) and exit.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "md")]
    explain: Option<Format>,
}

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
        } else if cli.detail {
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
    let lines = payload::to_lines(&resolved.text);
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

fn run(cli: Cli) -> Result<ExitCode, String> {
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

    let selector = walk::Selector {
        base: cli.base.clone(),
        names,
        types: cli.r#type.clone(),
        size,
        hidden: cli.hidden,
        follow: cli.follow,
    };

    let mode = Mode::from(&cli);
    let emit_present = cli.emit.is_some() || cli.emit_stderr.is_some();
    let need_lines =
        (matches!(mode, Mode::Detail | Mode::Summary) || emit_present) && grep_re.is_some();
    let collect_matches = emit_present || cli.json;

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

        let mut lines: Vec<(usize, String)> = Vec::new();
        if let Some(grep) = &grep_re {
            if !entry.file_type().is_file() {
                continue;
            }
            let bytes = match std::fs::read(entry.path()) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let content = String::from_utf8_lossy(&bytes);
            match grep {
                Grep::Line(re) => {
                    if !re.is_match(&content) {
                        continue;
                    }
                    if need_lines {
                        for (i, line) in content.lines().enumerate() {
                            if re.is_match(line) {
                                lines.push((i + 1, line.to_string()));
                            }
                        }
                    }
                }
                Grep::Block(b) => {
                    let file_lines: Vec<&str> = content.lines().collect();
                    let starts = block::find_starts(&file_lines, b);
                    if starts.is_empty() {
                        if let Some(miss) = block::nearest_miss(&file_lines, b)
                            && nearest
                                .as_ref()
                                .is_none_or(|(_, n)| miss.first_diverging_line > n.first_diverging_line)
                        {
                            nearest = Some((entry.path().display().to_string(), miss));
                        }
                        continue;
                    }
                    for s in starts {
                        lines.push((s + 1, file_lines[s].to_string()));
                    }
                }
            }
        }

        matched += 1;
        total_lines += lines.len();
        if collect_matches {
            match_paths.push(entry.path().display().to_string());
        }

        if !cli.json {
            match mode {
                Mode::List => println!("{}", entry.path().display()),
                Mode::Detail => {
                    if grep_re.is_some() && !lines.is_empty() {
                        for (ln, text) in &lines {
                            println!("{}:{}:{}", entry.path().display(), ln, text);
                        }
                    } else {
                        println!("{}", entry.path().display());
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
            "ct-search: nearest miss: {path}:{}: block diverges at its line {}",
            m.line, m.first_diverging_line
        );
        eprintln!("ct-search:   expected: {}", m.expected);
        eprintln!("ct-search:   found:    {}", m.found);
    }

    // The verdict generalises the historic exit status: the default `any`
    // expectation passes exactly when something matched, so a plain search is
    // unchanged, while `--expect none` (and the threshold forms) let a search
    // be posed as a pass/fail test.
    let verdict = expect.eval(matched as u64);

    if cli.json {
        let obj = json!({
            "tool": "ct-search",
            "verdict": verdict.label(),
            "base": cli.base.display().to_string(),
            "count": matched,
            "lines": total_lines,
            "matches": match_paths,
        });
        println!("{obj}");
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
            eprintln!("ct-search: {msg}");
            ExitCode::from(2)
        }
    }
}
