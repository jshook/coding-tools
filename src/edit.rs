// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The per-file replacement engine behind `ct-edit`: a line-scoped find/replace
//! that preserves every untouched byte (line terminators, indentation, and
//! surrounding text) and records the changed lines.

use regex::{NoExpand, Regex};

/// One line that an edit changed, captured before/after for preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub path: String,
    pub line: usize,
    pub before: String,
    pub after: String,
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
    let mut out = String::with_capacity(content.len());
    let mut count = 0usize;
    let mut sites = Vec::new();

    for (idx, segment) in content.split_inclusive('\n').enumerate() {
        let (body, nl) = match segment.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (segment, ""),
        };
        let hits = re.find_iter(body).count();
        if hits == 0 {
            out.push_str(segment);
            continue;
        }
        count += hits;
        let new_body = if literal {
            re.replace_all(body, NoExpand(replacement))
        } else {
            re.replace_all(body, replacement)
        };
        if new_body.as_ref() != body {
            sites.push(Site {
                path: path.to_string(),
                line: idx + 1,
                before: body.to_string(),
                after: new_body.to_string(),
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
