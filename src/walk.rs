// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Shared file-selection traversal.
//!
//! The predicate vocabulary every tool uses to choose *which* entries to act on
//! — search root, name, kind, size, and whether to descend dot-entries or follow
//! symlinks — lives here so it is identical across the suite: what you learn
//! about targeting from `ct-search` transfers verbatim to `ct-edit`. A
//! [`Selector`] holds the resolved predicates; [`Selector::walk`] yields the
//! entries that pass them, leaving content-level work (grep, replace) to the
//! caller.

use std::ffi::OsStr;
use std::path::PathBuf;

use ignore::{DirEntry, WalkBuilder};
use regex::Regex;

/// Entry-kind selector for `--type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EntryType {
    /// Regular file.
    F,
    /// Directory.
    D,
    /// Symbolic link.
    L,
}

/// A parsed `--size` predicate, in bytes.
#[derive(Debug, Clone, Copy)]
pub enum SizeCmp {
    /// Strictly larger than N bytes (`+N`).
    Gt(u64),
    /// Strictly smaller than N bytes (`-N`).
    Lt(u64),
    /// At least N bytes (bare `N`).
    Ge(u64),
}

/// Parse a `--size` spec `[+|-]N[k|m|g|b]` into a [`SizeCmp`].
///
/// `+N` is "larger than", `-N` is "smaller than", a bare `N` is "at least N";
/// a trailing `k`/`m`/`g` multiplies by 1024/1024²/1024³.
///
/// # Examples
///
/// ```
/// use coding_tools::walk::{parse_size, size_matches, SizeCmp};
///
/// let cmp = parse_size("+4k").unwrap();        // larger than 4 KiB
/// assert!(matches!(cmp, SizeCmp::Gt(4096)));
/// assert!(size_matches(&cmp, 5000));
/// assert!(!size_matches(&cmp, 4096));
///
/// assert!(matches!(parse_size("10").unwrap(), SizeCmp::Ge(10)));
/// assert!(parse_size("+x").is_err());
/// ```
pub fn parse_size(spec: &str) -> Result<SizeCmp, String> {
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

/// Whether a byte length satisfies a [`SizeCmp`].
pub fn size_matches(cmp: &SizeCmp, len: u64) -> bool {
    match *cmp {
        SizeCmp::Gt(n) => len > n,
        SizeCmp::Lt(n) => len < n,
        SizeCmp::Ge(n) => len >= n,
    }
}

/// Whether an entry's kind is among `types` (empty `types` means "any kind").
fn entry_kind_matches(types: &[EntryType], entry: &DirEntry) -> bool {
    if types.is_empty() {
        return true;
    }
    let Some(ft) = entry.file_type() else {
        return false; // only stdin has no file type; never matches a kind
    };
    types.iter().any(|t| match t {
        EntryType::F => ft.is_file(),
        EntryType::D => ft.is_dir(),
        EntryType::L => ft.is_symlink(),
    })
}

/// Resolved file-selection predicates. Build one, then iterate [`walk`].
///
/// [`walk`]: Selector::walk
pub struct Selector {
    /// Traversal root (a file yields just itself; a directory is descended).
    pub base: PathBuf,
    /// Whole-name alternatives; `None` matches any name.
    pub names: Option<Vec<Regex>>,
    /// Allowed entry kinds; empty matches any kind.
    pub types: Vec<EntryType>,
    /// Size predicate (applies to regular files only).
    pub size: Option<SizeCmp>,
    /// Include dot-entries and descend dot-directories.
    pub hidden: bool,
    /// Follow symlinks while traversing.
    pub follow: bool,
    /// Walk every file, ignoring `.gitignore`/`.ignore` rules (the `.git`
    /// directory is always skipped regardless). Default `false`: like git, the
    /// walk skips what the project has chosen to ignore.
    pub no_ignore: bool,
}

impl Selector {
    /// Yield every entry under [`base`](Selector::base) that passes the
    /// structural predicates (kind, name, size, hidden). By default the walk
    /// honors `.gitignore`/`.ignore` (and always skips `.git`), so a build tree
    /// like `target/` is not descended; `no_ignore` disables that filtering.
    /// Traversal errors and per-entry `stat` failures surface as `Err` items
    /// rather than panicking.
    pub fn walk(&self) -> impl Iterator<Item = Result<DirEntry, String>> + '_ {
        let respect = !self.no_ignore;
        WalkBuilder::new(&self.base)
            .follow_links(self.follow)
            .hidden(!self.hidden) // hidden(true) = skip dot-entries
            .ignore(respect)
            .git_ignore(respect)
            .git_global(respect)
            .git_exclude(respect)
            .parents(respect)
            // The VCS directory is never useful to these tools; skip it even
            // under --hidden / --no-ignore.
            .filter_entry(|e| e.file_name() != OsStr::new(".git"))
            .build()
            .filter_map(move |res| self.evaluate(res))
    }

    /// Apply the structural predicates to one raw traversal result. `None` drops
    /// the entry; `Some(Ok)` keeps it; `Some(Err)` reports a hard failure.
    fn evaluate(&self, res: Result<DirEntry, ignore::Error>) -> Option<Result<DirEntry, String>> {
        let entry = match res {
            Ok(e) => e,
            Err(e) => return Some(Err(format!("traversal error: {e}"))),
        };
        if !entry_kind_matches(&self.types, &entry) {
            return None;
        }
        if let Some(names) = &self.names {
            let nm = entry.file_name().to_string_lossy();
            if !names.iter().any(|r| r.is_match(&nm)) {
                return None;
            }
        }
        if let Some(cmp) = &self.size {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return None;
            }
            match entry.metadata() {
                Ok(m) => {
                    if !size_matches(cmp, m.len()) {
                        return None;
                    }
                }
                Err(e) => return Some(Err(format!("stat {}: {e}", entry.path().display()))),
            }
        }
        Some(Ok(entry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_grammar_directions() {
        assert!(matches!(parse_size("+4k").unwrap(), SizeCmp::Gt(4096)));
        assert!(matches!(parse_size("-2m").unwrap(), SizeCmp::Lt(2097152)));
        assert!(matches!(parse_size("10").unwrap(), SizeCmp::Ge(10)));
        assert!(parse_size("+x").is_err());
    }

    #[test]
    fn size_matches_compares() {
        assert!(size_matches(&SizeCmp::Gt(10), 11));
        assert!(!size_matches(&SizeCmp::Gt(10), 10));
        assert!(size_matches(&SizeCmp::Ge(10), 10));
        assert!(size_matches(&SizeCmp::Lt(10), 9));
    }
}
