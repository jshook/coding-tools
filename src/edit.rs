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
