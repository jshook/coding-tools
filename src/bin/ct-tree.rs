// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-tree` — annotated file-tree report.
//!
//! Walk a directory for chosen file types and report the effective tree with
//! per-file line/word/character counts, filtered by metric and per-folder
//! predicates, sorted, and summarised at the level you ask for (`--tree`,
//! `--flat`, or `--summary`). It is `tree` + `wc` with predicates, reachable
//! directly or as `ct tree`. The canonical reference is `docs/explain/ct-tree.md`;
//! `docs/explain/ct-tree.json` is the MCP tool-use definition. Both are embedded
//! below.

use std::collections::BTreeMap;
use std::process::ExitCode;

use clap::Parser;
use coding_tools::cli::ct_tree::{Cli, GroupBy, SortKey};
use coding_tools::explain::Format;
use coding_tools::pulse::{self, PulseState};
use coding_tools::tree::{metrics, parent_dir, within};
use coding_tools::walk::{self, EntryType};
use serde_json::json;

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-tree.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-tree.json");

/// One reported file with its counts and display path.
#[derive(Debug, Clone)]
struct FileRow {
    rel: String,
    name: String,
    ext: String,
    lines: u64,
    words: u64,
    chars: u64,
    bytes: u64,
}

/// Order `rows` in place by the sort key and direction.
fn sort_rows(rows: &mut [FileRow], key: SortKey, desc: bool) {
    rows.sort_by(|a, b| {
        let ord = match key {
            SortKey::Lines => a.lines.cmp(&b.lines),
            SortKey::Words => a.words.cmp(&b.words),
            SortKey::Chars => a.chars.cmp(&b.chars),
            SortKey::Bytes => a.bytes.cmp(&b.bytes),
            SortKey::Name => a.name.cmp(&b.name),
            SortKey::Ext => a.ext.cmp(&b.ext).then_with(|| a.rel.cmp(&b.rel)),
            SortKey::Path => a.rel.cmp(&b.rel),
        };
        if desc { ord.reverse() } else { ord }
    });
}

/// Decimal width of `n` (at least 1).
fn digits(n: u64) -> usize {
    n.to_string().len()
}

/// Grand totals over a flat row slice.
fn grand_totals(rows: &[FileRow]) -> Totals {
    rows.iter().fold(Totals::default(), |mut t, r| {
        t.files += 1;
        t.lines += r.lines;
        t.words += r.words;
        t.chars += r.chars;
        t.bytes += r.bytes;
        t
    })
}

/// The right-aligned widths of the lines/words/chars/bytes columns.
type Widths = (usize, usize, usize, usize);

/// Column widths for the count columns, sized to their headers and `t`'s values.
fn count_widths(t: &Totals) -> Widths {
    (
        "lines".len().max(digits(t.lines)),
        "words".len().max(digits(t.words)),
        "chars".len().max(digits(t.chars)),
        "bytes".len().max(digits(t.bytes)),
    )
}

/// The right-aligned count-column header, with `bytes` appended when shown.
fn count_headers(show_bytes: bool, (wl, ww, wc, wb): Widths) -> String {
    if show_bytes {
        format!(
            "{:>wl$} {:>ww$} {:>wc$} {:>wb$}",
            "lines", "words", "chars", "bytes"
        )
    } else {
        format!("{:>wl$} {:>ww$} {:>wc$}", "lines", "words", "chars")
    }
}

/// One row's right-aligned count columns, with `bytes` appended when shown.
fn count_cols(
    show_bytes: bool,
    lines: u64,
    words: u64,
    chars: u64,
    bytes: u64,
    w: Widths,
) -> String {
    let (wl, ww, wc, wb) = w;
    if show_bytes {
        format!("{lines:>wl$} {words:>ww$} {chars:>wc$} {bytes:>wb$}")
    } else {
        format!("{lines:>wl$} {words:>ww$} {chars:>wc$}")
    }
}

// ----- Tree model -------------------------------------------------------------

#[derive(Default)]
struct Dir {
    subdirs: BTreeMap<String, Dir>,
    files: Vec<FileRow>,
}

#[derive(Default, Clone, Copy)]
struct Totals {
    files: u64,
    lines: u64,
    words: u64,
    chars: u64,
    bytes: u64,
}

impl Dir {
    fn insert(&mut self, components: &[&str], row: FileRow) {
        match components {
            [_file] => self.files.push(row),
            [dir, rest @ ..] => self
                .subdirs
                .entry(dir.to_string())
                .or_default()
                .insert(rest, row),
            [] => {}
        }
    }

    fn totals(&self) -> Totals {
        let mut t = Totals::default();
        for f in &self.files {
            t.files += 1;
            t.lines += f.lines;
            t.words += f.words;
            t.chars += f.chars;
            t.bytes += f.bytes;
        }
        for d in self.subdirs.values() {
            let s = d.totals();
            t.files += s.files;
            t.lines += s.lines;
            t.words += s.words;
            t.chars += s.chars;
            t.bytes += s.bytes;
        }
        t
    }
}

/// A rendered tree row: its label (indent + connectors + name) and counts.
struct TreeLine {
    label: String,
    lines: u64,
    words: u64,
    chars: u64,
    bytes: u64,
}

/// Append the ordered tree lines for `dir`'s children under `prefix`.
fn tree_lines(dir: &Dir, prefix: &str, key: SortKey, desc: bool, out: &mut Vec<TreeLine>) {
    let mut subdirs: Vec<(&String, &Dir)> = dir.subdirs.iter().collect();
    subdirs.sort_by(|a, b| {
        let (ta, tb) = (a.1.totals(), b.1.totals());
        let ord = match key {
            SortKey::Lines => ta.lines.cmp(&tb.lines),
            SortKey::Words => ta.words.cmp(&tb.words),
            SortKey::Chars => ta.chars.cmp(&tb.chars),
            SortKey::Bytes => ta.bytes.cmp(&tb.bytes),
            _ => a.0.cmp(b.0),
        };
        if desc { ord.reverse() } else { ord }
    });
    let mut files = dir.files.clone();
    sort_rows(&mut files, key, desc);

    let total = subdirs.len() + files.len();
    let mut i = 0;
    for (name, sub) in subdirs {
        let last = i == total - 1;
        let connector = if last { "└─ " } else { "├─ " };
        let t = sub.totals();
        out.push(TreeLine {
            label: format!("{prefix}{connector}{name}/"),
            lines: t.lines,
            words: t.words,
            chars: t.chars,
            bytes: t.bytes,
        });
        let child_prefix = format!("{prefix}{}", if last { "   " } else { "│  " });
        tree_lines(sub, &child_prefix, key, desc, out);
        i += 1;
    }
    for f in &files {
        let last = i == total - 1;
        let connector = if last { "└─ " } else { "├─ " };
        out.push(TreeLine {
            label: format!("{prefix}{connector}{}", f.name),
            lines: f.lines,
            words: f.words,
            chars: f.chars,
            bytes: f.bytes,
        });
        i += 1;
    }
}

// ----- Rendering --------------------------------------------------------------

fn render_flat(rows: &[FileRow], show_bytes: bool) {
    let grand = grand_totals(rows);
    let w = count_widths(&grand);
    println!("{}  file", count_headers(show_bytes, w));
    for r in rows {
        println!(
            "{}  {}",
            count_cols(show_bytes, r.lines, r.words, r.chars, r.bytes, w),
            r.rel
        );
    }
    println!(
        "{}  {} file(s)",
        count_cols(
            show_bytes,
            grand.lines,
            grand.words,
            grand.chars,
            grand.bytes,
            w
        ),
        grand.files
    );
}

fn render_tree(base: &str, root: &Dir, key: SortKey, desc: bool, show_bytes: bool) {
    let grand = root.totals();
    let mut lines = vec![TreeLine {
        label: format!("{base}/"),
        lines: grand.lines,
        words: grand.words,
        chars: grand.chars,
        bytes: grand.bytes,
    }];
    tree_lines(root, "", key, desc, &mut lines);

    let label_w = lines
        .iter()
        .map(|l| l.label.chars().count())
        .max()
        .unwrap_or(0);
    let w = count_widths(&grand);
    println!("{:<label_w$}  {}", "", count_headers(show_bytes, w));
    for l in &lines {
        println!(
            "{:<label_w$}  {}",
            l.label,
            count_cols(show_bytes, l.lines, l.words, l.chars, l.bytes, w)
        );
    }
    println!("{} file(s) total", grand.files);
}

/// (group label, totals) pairs for the summary mode.
fn summary_groups(rows: &[FileRow], group: GroupBy) -> Vec<(String, Totals)> {
    if matches!(group, GroupBy::None) {
        return vec![("(all)".to_string(), grand_totals(rows))];
    }
    let mut map: BTreeMap<String, Totals> = BTreeMap::new();
    for r in rows {
        let key = match group {
            GroupBy::Ext => {
                if r.ext.is_empty() {
                    "(none)".to_string()
                } else {
                    format!(".{}", r.ext)
                }
            }
            GroupBy::Dir => parent_dir(&r.rel),
            GroupBy::None => unreachable!(),
        };
        let t = map.entry(key).or_default();
        t.files += 1;
        t.lines += r.lines;
        t.words += r.words;
        t.chars += r.chars;
        t.bytes += r.bytes;
    }
    map.into_iter().collect()
}

fn render_summary(rows: &[FileRow], group: GroupBy, show_bytes: bool) {
    let groups = summary_groups(rows, group);
    let grand = grand_totals(rows);
    let label_w = groups
        .iter()
        .map(|(k, _)| k.chars().count())
        .chain(["total".len()])
        .max()
        .unwrap_or(0);
    let wf = "files".len().max(digits(grand.files));
    let w = count_widths(&grand);
    println!(
        "{:<label_w$}  {:>wf$} {}",
        "",
        "files",
        count_headers(show_bytes, w)
    );
    for (k, t) in &groups {
        println!(
            "{:<label_w$}  {:>wf$} {}",
            k,
            t.files,
            count_cols(show_bytes, t.lines, t.words, t.chars, t.bytes, w)
        );
    }
    println!(
        "{:<label_w$}  {:>wf$} {}",
        "total",
        grand.files,
        count_cols(
            show_bytes,
            grand.lines,
            grand.words,
            grand.chars,
            grand.bytes,
            w
        )
    );
}

fn render_json(cli: &Cli, rows: &[FileRow]) {
    let grand = grand_totals(rows);
    let files: Vec<_> = rows
        .iter()
        .map(|r| {
            json!({ "path": r.rel, "ext": r.ext, "lines": r.lines, "words": r.words, "chars": r.chars, "bytes": r.bytes })
        })
        .collect();
    let by_ext: Vec<_> = summary_groups(rows, GroupBy::Ext)
        .into_iter()
        .map(|(k, t)| json!({ "group": k, "files": t.files, "lines": t.lines, "words": t.words, "chars": t.chars, "bytes": t.bytes }))
        .collect();
    let obj = json!({
        "tool": "ct-tree",
        "base": cli.base.display().to_string(),
        "files": files,
        "by_ext": by_ext,
        "totals": { "files": grand.files, "lines": grand.lines, "words": grand.words, "chars": grand.chars, "bytes": grand.bytes },
    });
    coding_tools::jsonout::print(&obj, cli.json_pretty);
}

fn run(mut cli: Cli) -> Result<ExitCode, String> {
    // --json-pretty enables JSON output on its own; treat it as --json
    // everywhere the text path is gated.
    if cli.json_pretty {
        cli.json = true;
    }
    let _watchdog = pulse::watchdog("ct-tree", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-tree", PulseState::new())?;
    // --ext is sugar for additional name alternatives.
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
            coding_tools::pattern::compile_name_set_with(&name_spec, cli.mode)
                .map_err(|e| format!("invalid --name/--ext pattern: {e}"))?,
        )
    };

    let selector = walk::Selector {
        base: cli.base.clone(),
        names,
        types: vec![EntryType::F],
        size: None,
        hidden: cli.hidden,
        follow: cli.follow,
        no_ignore: cli.no_ignore,
    };

    let base_disp = cli.base.display().to_string();
    let strip_prefix = format!("{}/", base_disp.trim_end_matches('/'));

    // Collect rows that pass the metric predicates.
    let mut rows: Vec<FileRow> = Vec::new();
    for entry in selector.walk() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let bytes = match std::fs::read(entry.path()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let byte_len = bytes.len() as u64;
        let content = String::from_utf8_lossy(&bytes);
        let (lines, words, chars) = metrics(&content);
        if !within(lines, cli.min_lines, cli.max_lines)
            || !within(words, cli.min_words, cli.max_words)
            || !within(chars, cli.min_chars, cli.max_chars)
            || !within(byte_len, cli.min_bytes, cli.max_bytes)
        {
            continue;
        }
        let full = entry.path().display().to_string();
        let rel = full
            .strip_prefix(&strip_prefix)
            .unwrap_or(&full)
            .to_string();
        let name = entry.file_name().to_string_lossy().into_owned();
        let ext = entry
            .path()
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        rows.push(FileRow {
            rel,
            name,
            ext,
            lines,
            words,
            chars,
            bytes: byte_len,
        });
    }

    // Per-folder predicates: count matching files by immediate parent directory.
    if cli.min_files_per_folder.is_some() || cli.max_files_per_folder.is_some() {
        let mut per_folder: BTreeMap<String, usize> = BTreeMap::new();
        for r in &rows {
            *per_folder.entry(parent_dir(&r.rel)).or_default() += 1;
        }
        rows.retain(|r| {
            let n = per_folder[&parent_dir(&r.rel)];
            within(
                n as u64,
                cli.min_files_per_folder.map(|v| v as u64),
                cli.max_files_per_folder.map(|v| v as u64),
            )
        });
    }

    let matched = rows.len();
    sort_rows(&mut rows, cli.sort, cli.desc);

    // The byte column shows when asked for, or when it's the sort key (so you
    // never sort by a column you can't see).
    let show_bytes = cli.bytes || matches!(cli.sort, SortKey::Bytes);

    if cli.json {
        render_json(&cli, &rows);
    } else if cli.flat {
        render_flat(&rows, show_bytes);
    } else if cli.summary {
        render_summary(&rows, cli.group, show_bytes);
    } else {
        // Default: the tree. Build it from the (already metric/folder-filtered) rows.
        let mut root = Dir::default();
        for r in &rows {
            let comps: Vec<&str> = r.rel.split('/').collect();
            root.insert(&comps, r.clone());
        }
        render_tree(
            base_disp.trim_end_matches('/'),
            &root,
            cli.sort,
            cli.desc,
            show_bytes,
        );
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
            eprintln!("ct-tree: {msg}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_desc_by_lines() {
        let mut rows = vec![
            FileRow {
                rel: "a".into(),
                name: "a".into(),
                ext: "rs".into(),
                lines: 10,
                words: 0,
                chars: 0,
                bytes: 0,
            },
            FileRow {
                rel: "b".into(),
                name: "b".into(),
                ext: "rs".into(),
                lines: 99,
                words: 0,
                chars: 0,
                bytes: 0,
            },
            FileRow {
                rel: "c".into(),
                name: "c".into(),
                ext: "rs".into(),
                lines: 50,
                words: 0,
                chars: 0,
                bytes: 0,
            },
        ];
        sort_rows(&mut rows, SortKey::Lines, true);
        assert_eq!(
            rows.iter().map(|r| r.lines).collect::<Vec<_>>(),
            vec![99, 50, 10]
        );
    }

    #[test]
    fn tree_inserts_into_nested_dirs() {
        let mut root = Dir::default();
        let row = |rel: &str, lines| FileRow {
            rel: rel.into(),
            name: rel.rsplit('/').next().unwrap().into(),
            ext: "rs".into(),
            lines,
            words: 0,
            chars: 0,
            bytes: 0,
        };
        root.insert(&["src", "main.rs"], row("src/main.rs", 10));
        root.insert(&["src", "util", "a.rs"], row("src/util/a.rs", 5));
        let t = root.totals();
        assert_eq!((t.files, t.lines), (2, 15));
        assert!(root.subdirs.contains_key("src"));
        assert!(root.subdirs["src"].subdirs.contains_key("util"));
    }
}
