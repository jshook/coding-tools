// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The per-file replacement engine behind `ct-edit`: a line-scoped find/replace
//! that preserves every untouched byte (line terminators, indentation, and
//! surrounding text) and records the changed lines.

use regex::Regex;

/// One line that an edit changed, captured before/after for preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub path: String,
    pub line: usize,
    pub before: String,
    pub after: String,
}

/// Which of the sites an anchor matched the edit actually applies to.
///
/// The suite's answer to a non-unique anchor has been "make it unique" — widen
/// the `--find` until it spans a distinguishing line. That is the right default
/// (an anchor that names its site survives the file changing around it), but it
/// is overkill when the caller can already see the list and just wants the
/// second one. [`Nth`](Pick::Nth) and [`At`](Pick::At) say so directly, without
/// giving up the `--expect` gate: a pick still has to land, and a pick that
/// lands on nothing fails as loudly as an anchor that matched nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pick {
    /// Every matched site (the default, and what `--expect` has always judged).
    #[default]
    All,
    /// Only the Nth match (1-based) in selection order: files in walk order,
    /// and within a file, top to bottom.
    Nth(usize),
    /// Only a match that starts on `line` of `file`.
    At { file: String, line: usize },
}

/// How many match sightings a [`Picker`] remembers for its diagnosis. A pick
/// that missed needs to show where the anchor *did* land; a handful is enough
/// to redirect it, and the cap keeps a broad anchor from collecting a file's
/// worth of positions nobody will read.
const SIGHTING_CAP: usize = 8;

/// A [`Pick`] in progress across a whole selection.
///
/// One picker is threaded through every file the edit scans, so
/// [`Nth`](Pick::Nth) counts in selection order rather than restarting per
/// file, and the tally it accumulates is what separates *"the anchor matched
/// nothing"* from *"the anchor matched, and the pick chose none of it"* — two
/// zero-replacement runs with completely different fixes.
#[derive(Debug)]
pub struct Picker<'a> {
    pick: &'a Pick,
    seen: usize,
    taken: usize,
    sightings: Vec<(String, usize)>,
}

impl<'a> Picker<'a> {
    /// A picker for `pick`, positioned before the first match.
    pub fn new(pick: &'a Pick) -> Self {
        Self {
            pick,
            seen: 0,
            taken: 0,
            sightings: Vec::new(),
        }
    }

    /// Offer the next match, at `line` of `path`, and answer whether the edit
    /// applies to it. Every match must be offered exactly once, in order, even
    /// when the answer is obviously "no": the running count is the ordinal
    /// [`Pick::Nth`] selects on.
    pub fn accept(&mut self, path: &str, line: usize) -> bool {
        self.seen += 1;
        if self.sightings.len() < SIGHTING_CAP {
            self.sightings.push((path.to_string(), line));
        }
        let hit = match self.pick {
            Pick::All => true,
            Pick::Nth(n) => self.seen == *n,
            Pick::At { file, line: want } => *want == line && path_matches(path, file),
        };
        if hit {
            self.taken += 1;
        }
        hit
    }

    /// How many matches the anchor found, whether or not the pick took them.
    pub fn seen(&self) -> usize {
        self.seen
    }

    /// How many matches the pick took.
    pub fn taken(&self) -> usize {
        self.taken
    }

    /// The note for a pick that took nothing from an anchor that *did* match,
    /// or `None` when there is no such mismatch to explain (the pick landed,
    /// or the anchor found nothing and the ordinary nearest-miss diagnosis
    /// applies instead).
    ///
    /// # Examples
    ///
    /// ```
    /// use coding_tools::edit::{Pick, Picker};
    ///
    /// let pick = Pick::Nth(3);
    /// let mut p = Picker::new(&pick);
    /// p.accept("a.rs", 4);
    /// p.accept("a.rs", 9);
    /// let note = p.miss_note().unwrap();
    /// assert!(note.starts_with("--nth 3, but the selection holds 2 match(es)"));
    /// assert!(note.contains("a.rs:4, a.rs:9"));
    ///
    /// // A pick that landed has nothing to explain.
    /// let all = Pick::All;
    /// let mut p = Picker::new(&all);
    /// p.accept("a.rs", 4);
    /// assert_eq!(p.miss_note(), None);
    /// ```
    pub fn miss_note(&self) -> Option<String> {
        if self.taken > 0 || self.seen == 0 || matches!(self.pick, Pick::All) {
            return None;
        }
        let head = match self.pick {
            Pick::Nth(n) => format!("--nth {n}, but the selection holds {} match(es)", self.seen),
            Pick::At { file, line } => format!(
                "--at {file}:{line} matched no site, though the anchor matched {} time(s)",
                self.seen
            ),
            Pick::All => unreachable!("Pick::All takes every match"),
        };
        Some(format!(
            "{head}; the anchor matched at {}",
            self.sightings_label()
        ))
    }

    /// The remembered match positions as `path:line`, comma-joined and elided
    /// once the cap is reached.
    fn sightings_label(&self) -> String {
        let shown: Vec<String> = self
            .sightings
            .iter()
            .map(|(p, l)| format!("{p}:{l}"))
            .collect();
        if self.seen > shown.len() {
            format!(
                "{} (and {} more)",
                shown.join(", "),
                self.seen - shown.len()
            )
        } else {
            shown.join(", ")
        }
    }
}

/// Whether `path` is the file `spec` names: the same path, or a
/// whole-component suffix of it, with `/` and `\` treated as equivalent so a
/// spec written with forward slashes matches the OS-native paths the walker
/// yields on Windows.
///
/// # Examples
///
/// ```
/// use coding_tools::edit::path_matches;
///
/// assert!(path_matches("src/edit.rs", "src/edit.rs"));
/// assert!(path_matches("src/edit.rs", "edit.rs"));
/// assert!(path_matches(r"src\cli\ct_edit.rs", "cli/ct_edit.rs"));
/// // A suffix must start at a component boundary.
/// assert!(!path_matches("src/my_edit.rs", "edit.rs"));
/// ```
pub fn path_matches(path: &str, spec: &str) -> bool {
    let norm = |p: &str| p.replace('\\', "/");
    let (path, spec) = (norm(path), norm(spec));
    path == spec || path.ends_with(&format!("/{spec}"))
}

/// Apply `re`/`replacement` to one file's `content`, per line, preserving line
/// terminators and every untouched byte. Returns the new content, the number of
/// occurrences replaced, and the changed lines. `literal` selects literal
/// replacement (no `$` capture expansion), used for literal/glob finds.
///
/// # Examples
///
/// ```
/// use coding_tools::edit::edit_content;
/// use coding_tools::pattern::compile;
///
/// let re = compile("foo").unwrap();
/// let (out, n, sites) = edit_content("f.rs", "a\nfoo bar\n  foo\n", &re, "X", true);
/// assert_eq!(n, 2);
/// // Untouched lines and the indentation on the changed line are preserved.
/// assert_eq!(out, "a\nX bar\n  X\n");
/// assert_eq!(sites.len(), 2);
/// assert_eq!(sites[1].after, "  X");
///
/// // A literal find does not expand `$` in the replacement.
/// let key = compile("KEY").unwrap();
/// assert_eq!(edit_content("f", "KEY\n", &key, "$1 cost", true).0, "$1 cost\n");
/// ```
pub fn edit_content(
    path: &str,
    content: &str,
    re: &Regex,
    replacement: &str,
    literal: bool,
) -> (String, usize, Vec<Site>) {
    edit_content_picked(
        path,
        content,
        re,
        replacement,
        literal,
        &mut Picker::new(&Pick::All),
    )
}

/// [`edit_content`] under a running [`Picker`]: every occurrence is offered to
/// the picker in selection order, and only the accepted ones are replaced.
/// The returned count is what was *applied*, so an `--expect` gate keeps
/// judging the edit's real effect.
///
/// # Examples
///
/// ```
/// use coding_tools::edit::{edit_content_picked, Pick, Picker};
/// use coding_tools::pattern::compile;
///
/// let re = compile("foo").unwrap();
/// let pick = Pick::Nth(2);
/// let mut picker = Picker::new(&pick);
/// let (out, n, _) = edit_content_picked("f.rs", "foo\nfoo\nfoo\n", &re, "X", true, &mut picker);
/// assert_eq!((out.as_str(), n), ("foo\nX\nfoo\n", 1));
/// // The picker still saw every match, which is what lets a missed pick
/// // report where the anchor did land.
/// assert_eq!(picker.seen(), 3);
/// ```
pub fn edit_content_picked(
    path: &str,
    content: &str,
    re: &Regex,
    replacement: &str,
    literal: bool,
    picker: &mut Picker<'_>,
) -> (String, usize, Vec<Site>) {
    let mut out = String::with_capacity(content.len());
    let mut count = 0usize;
    let mut sites = Vec::new();

    // The terminator is the whole line ending (`\r\n`, `\n`, or none at EOF),
    // so the body a pattern sees carries no `\r`: a `$`-anchored regex matches
    // a CRLF line, a site's before/after text is clean, and the ending goes
    // back exactly as it was.
    let segments = crate::lines::Segments::split(content);
    for (idx, (body, nl)) in segments.segs.iter().copied().enumerate() {
        let line = idx + 1;
        // Assembled occurrence by occurrence rather than by `replace_all`, so a
        // pick can decline one match on a line and keep the rest of that line
        // byte-identical.
        let mut new_body = String::new();
        let mut last = 0usize;
        let mut hits = 0usize;
        for caps in re.captures_iter(body) {
            let m = caps.get(0).expect("capture group 0 always participates");
            if !picker.accept(path, line) {
                continue;
            }
            hits += 1;
            new_body.push_str(&body[last..m.start()]);
            if literal {
                new_body.push_str(replacement);
            } else {
                caps.expand(replacement, &mut new_body);
            }
            last = m.end();
        }
        if hits == 0 {
            out.push_str(body);
            out.push_str(nl);
            continue;
        }
        new_body.push_str(&body[last..]);
        count += hits;
        if new_body != body {
            sites.push(Site {
                path: path.to_string(),
                line,
                before: body.to_string(),
                after: new_body.clone(),
            });
        }
        out.push_str(&new_body);
        out.push_str(nl);
    }

    (out, count, sites)
}

/// Why a **single-line** `--find` matched nothing, located in the source.
///
/// A multi-line find already reports a positioned [`crate::block::NearestMiss`];
/// a single-line find used to fail with a bare `0 replacement(s) -> ERROR` and
/// no hint at all, which is the one place the suite went quiet exactly when the
/// caller needed a diagnosis. [`diagnose_line_miss`] fills that gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMiss {
    /// File holding the best candidate line.
    pub path: String,
    /// 1-based line number of that candidate.
    pub line: usize,
    /// The `--find` text as written, for the side-by-side row.
    pub pattern: String,
    /// The candidate line's text.
    pub found: String,
    /// What went wrong.
    pub cause: LineMissCause,
}

/// The diagnosed cause behind a [`LineMiss`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineMissCause {
    /// The find text is present verbatim, but was compiled as a glob/regex
    /// (it carries metacharacters) that did not match it. Reachable only
    /// under an explicit `--mode glob`/`regex`/`auto`, since a bare `--find`
    /// is literal; the payload names the kind it was read as and how many
    /// lines hold the literal text — dropping the `--mode` is the fix.
    Promoted {
        kind: &'static str,
        literal_hits: usize,
    },
    /// A line matches once leading/trailing whitespace is ignored: indentation
    /// drift, the most common single-line anchor failure.
    Whitespace,
}

impl LineMiss {
    /// The one-line explanation of the cause, for the `note:` row.
    pub fn note(&self) -> String {
        match &self.cause {
            LineMissCause::Promoted { kind, literal_hits } => format!(
                "the --find text carries {kind} metacharacters, and the stated --mode read it \
                 as a {kind}, which did not match; the text is present verbatim on \
                 {literal_hits} line(s) — drop the --mode (a bare --find is already literal), \
                 or pass the anchor as file:PATH or stdin: to pin only --find"
            ),
            LineMissCause::Whitespace => "the line differs only in leading/trailing whitespace; \
                 a single-line --find is matched against the line as written, so copy its exact \
                 text (or pass it via file:/stdin:)"
                .to_string(),
        }
    }

    /// The headline describing what was found, for the `nearest miss:` row.
    pub fn headline(&self) -> &'static str {
        match self.cause {
            LineMissCause::Promoted { .. } => {
                "the --find text is here, but the pattern was promoted"
            }
            LineMissCause::Whitespace => "line differs only in surrounding whitespace",
        }
    }
}

/// Diagnose a single-line `--find` that matched nothing, over the same files
/// the edit scanned. `promoted_as` is the non-literal kind the pattern was
/// compiled as (`"glob"`/`"regex"`), or `None` when it was already literal.
///
/// Two causes are recognised, in order of confidence: the text is present
/// **verbatim** but the pattern was promoted past it (the reported failure —
/// a registration line full of `(`/`.`/`[` read as a regex), or a line matches
/// once **whitespace** is trimmed. Anything else returns `None`: the caller
/// reports the scan's shape rather than inventing a cause.
///
/// # Examples
///
/// ```
/// use coding_tools::edit::{diagnose_line_miss, LineMissCause};
///
/// // A `[..]` in the text makes it a glob, which then fails to match itself.
/// let src = concat!("ok\n", "    reg::<Foo[T]>();\n");
/// let files = [("a.rs", src)];
/// let m = diagnose_line_miss(files.iter().copied(), "reg::<Foo[T]>();", Some("glob")).unwrap();
/// assert_eq!((m.line, m.path.as_str()), (2, "a.rs"));
/// assert!(matches!(m.cause, LineMissCause::Promoted { literal_hits: 1, .. }));
///
/// // Indentation drift on an otherwise literal find.
/// let files = [("a.rs", "let x = 1;\n")];
/// let m = diagnose_line_miss(files.iter().copied(), "  let x = 1;  ", None).unwrap();
/// assert_eq!(m.cause, LineMissCause::Whitespace);
/// ```
pub fn diagnose_line_miss<'a, I>(
    files: I,
    needle: &str,
    promoted_as: Option<&'static str>,
) -> Option<LineMiss>
where
    I: IntoIterator<Item = (&'a str, &'a str)> + Clone,
{
    // 1. The find text present verbatim, but the pattern was promoted past it.
    if let Some(kind) = promoted_as {
        let mut first: Option<(String, usize, String)> = None;
        let mut hits = 0usize;
        for (path, content) in files.clone() {
            for (i, line) in content.lines().enumerate() {
                if line.contains(needle) {
                    hits += 1;
                    if first.is_none() {
                        first = Some((path.to_string(), i + 1, line.to_string()));
                    }
                }
            }
        }
        if let Some((path, line, found)) = first {
            return Some(LineMiss {
                path,
                line,
                pattern: needle.to_string(),
                found,
                cause: LineMissCause::Promoted {
                    kind,
                    literal_hits: hits,
                },
            });
        }
    }

    // 2. Whitespace drift: a line equal to the find text once both are trimmed.
    let want = needle.trim();
    if want.is_empty() {
        return None;
    }
    for (path, content) in files {
        for (i, line) in content.lines().enumerate() {
            if line.trim() == want {
                return Some(LineMiss {
                    path: path.to_string(),
                    line: i + 1,
                    pattern: needle.to_string(),
                    found: line.to_string(),
                    cause: LineMissCause::Whitespace,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{self, PatternKind};

    fn re(p: &str) -> (Regex, bool) {
        (
            pattern::compile(p).unwrap(),
            !matches!(pattern::classify(p), PatternKind::Regex),
        )
    }

    #[test]
    fn preserves_untouched_lines_and_terminators() {
        let (r, lit) = re("foo");
        let (out, n, sites) = edit_content("f", "a\nfoo bar\n  foo\n", &r, "X", lit);
        assert_eq!(n, 2);
        assert_eq!(out, "a\nX bar\n  X\n");
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[1].after, "  X");
    }

    #[test]
    fn missing_final_newline_is_preserved() {
        let (r, lit) = re("a");
        let (out, n, _) = edit_content("f", "a", &r, "b", lit);
        assert_eq!((out.as_str(), n), ("b", 1));
    }

    #[test]
    fn literal_find_does_not_expand_dollar_in_replacement() {
        let (r, lit) = re("KEY");
        let (out, _, _) = edit_content("f", "KEY\n", &r, "$1 cost", lit);
        assert_eq!(out, "$1 cost\n");
    }

    #[test]
    fn regex_find_expands_captures() {
        let (r, lit) = re(r"v(\d+)");
        let (out, n, _) = edit_content("f", "v12\n", &r, "ver${1}", lit);
        assert_eq!((out.as_str(), n), ("ver12\n", 1));
    }
}
