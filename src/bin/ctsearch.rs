// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ctsearch` — recursive, predicate-based file search.
//!
//! A declarative replacement for `find … | xargs grep …`. The canonical,
//! self-contained reference is `docs/explain/ctsearch.md` — the same text this
//! tool emits for `--explain md`; `docs/explain/ctsearch.json` is the MCP
//! tool-use definition emitted for `--explain json`. Both are embedded below.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use coding_tools::explain::Format;
use coding_tools::pattern;
use walkdir::{DirEntry, WalkDir};

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ctsearch.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ctsearch.json");

/// Entry-kind selector for `--type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum EntryType {
    /// Regular file.
    F,
    /// Directory.
    D,
    /// Symbolic link.
    L,
}

#[derive(Parser, Debug)]
#[command(
    name = "ctsearch",
    version,
    about = "Recursively find files by name, type, size, and content from a chosen root.",
    long_about = "ctsearch combines the predicates you would otherwise assemble from find, xargs, \
                  and grep into one declarative command. An entry matches only when every supplied \
                  predicate holds. See `ctsearch --explain` for agent-oriented documentation."
)]
#[command(group = clap::ArgGroup::new("mode")
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

    /// Content pattern (substring->glob->regex promoted); searches file contents.
    #[arg(long)]
    grep: Option<String>,

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

/// A parsed `--size` predicate, in bytes.
enum SizeCmp {
    Gt(u64),
    Lt(u64),
    Ge(u64),
}

fn parse_size(spec: &str) -> Result<SizeCmp, String> {
    let spec = spec.trim();
    let (ctor, body): (fn(u64) -> SizeCmp, &str) = if let Some(r) = spec.strip_prefix('+') {
        (SizeCmp::Gt, r)
    } else if let Some(r) = spec.strip_prefix('-') {
        (SizeCmp::Lt, r)
    } else {
        (SizeCmp::Ge, spec)
    };
    let body = body.trim();
    if body.is_empty() {
        return Err(format!("empty size value in '{spec}'"));
    }
    let last = body.chars().last().unwrap();
    let (num_part, mult): (&str, u64) = match last.to_ascii_lowercase() {
        'k' => (&body[..body.len() - 1], 1024),
        'm' => (&body[..body.len() - 1], 1024 * 1024),
        'g' => (&body[..body.len() - 1], 1024 * 1024 * 1024),
        'b' => (&body[..body.len() - 1], 1),
        _ => (body, 1),
    };
    let n: u64 = num_part
        .trim()
        .parse()
        .map_err(|_| format!("invalid size number '{num_part}' in '{spec}'"))?;
    let bytes = n
        .checked_mul(mult)
        .ok_or_else(|| format!("size too large: '{spec}'"))?;
    Ok(ctor(bytes))
}

fn size_matches(cmp: &SizeCmp, len: u64) -> bool {
    match *cmp {
        SizeCmp::Gt(n) => len > n,
        SizeCmp::Lt(n) => len < n,
        SizeCmp::Ge(n) => len >= n,
    }
}

/// True for dot-entries below the search root (the root itself is never hidden).
fn is_hidden(entry: &DirEntry) -> bool {
    entry.depth() > 0 && entry.file_name().to_string_lossy().starts_with('.')
}

fn entry_kind_matches(types: &[EntryType], entry: &DirEntry) -> bool {
    if types.is_empty() {
        return true;
    }
    let ft = entry.file_type();
    types.iter().any(|t| match t {
        EntryType::F => ft.is_file(),
        EntryType::D => ft.is_dir(),
        EntryType::L => ft.is_symlink(),
    })
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let name_set = match &cli.name {
        Some(spec) => Some(
            pattern::compile_name_set(spec).map_err(|e| format!("invalid --name pattern: {e}"))?,
        ),
        None => None,
    };
    let grep_re = match &cli.grep {
        Some(p) => Some(pattern::compile(p).map_err(|e| format!("invalid --grep pattern: {e}"))?),
        None => None,
    };
    let size_pred = match &cli.size {
        Some(s) => Some(parse_size(s)?),
        None => None,
    };

    let mode = Mode::from(&cli);
    let need_lines = matches!(mode, Mode::Detail | Mode::Summary) && grep_re.is_some();

    let mut matched = 0usize;
    let mut total_lines = 0usize;

    let walker = WalkDir::new(&cli.base)
        .follow_links(cli.follow)
        .into_iter()
        .filter_entry(|e| cli.hidden || !is_hidden(e));

    for entry in walker {
        let entry = entry.map_err(|e| format!("traversal error: {e}"))?;

        if !entry_kind_matches(&cli.r#type, &entry) {
            continue;
        }
        if let Some(set) = &name_set {
            let nm = entry.file_name().to_string_lossy();
            if !set.iter().any(|r| r.is_match(&nm)) {
                continue;
            }
        }
        if let Some(cmp) = &size_pred {
            if !entry.file_type().is_file() {
                continue;
            }
            let len = entry
                .metadata()
                .map_err(|e| format!("stat {}: {e}", entry.path().display()))?
                .len();
            if !size_matches(cmp, len) {
                continue;
            }
        }

        let mut lines: Vec<(usize, String)> = Vec::new();
        if let Some(re) = &grep_re {
            if !entry.file_type().is_file() {
                continue;
            }
            let bytes = match std::fs::read(entry.path()) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let content = String::from_utf8_lossy(&bytes);
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

        matched += 1;
        total_lines += lines.len();

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

        if let Some(limit) = cli.limit
            && matched >= limit
        {
            break;
        }
    }

    if let Mode::Summary = mode {
        if grep_re.is_some() {
            println!("{matched} file(s) matched, {total_lines} matching line(s)");
        } else {
            println!("{matched} match(es)");
        }
    }

    Ok(if matched > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
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
            eprintln!("ctsearch: {msg}");
            ExitCode::from(2)
        }
    }
}
