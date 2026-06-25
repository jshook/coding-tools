// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Line-anchored literal block matching, shared by `ct-search`, `ct-view`,
//! and `ct-edit`.
//!
//! A multi-line pattern matches as a *block*: a find block of K lines matches
//! K consecutive source lines exactly, byte-for-byte, leading and trailing
//! whitespace significant. When a block fails to match, [`nearest_miss`]
//! reports the best partial alignment — the candidate with the longest
//! matching prefix and the first diverging line — so the author sees *why*
//! the anchor missed (whitespace drift, a comment edit, an already-applied
//! change) without bisecting by hand.

/// The best partial alignment of a block that did not match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NearestMiss {
    /// 1-based source line where the best candidate alignment starts.
    pub line: usize,
    /// 1-based index *into the block* of the first diverging line.
    pub first_diverging_line: usize,
    /// The block line that was expected at the divergence.
    pub expected: String,
    /// The source line actually found there (empty past end of file).
    pub found: String,
}

/// Find every non-overlapping occurrence of `block` in `lines`, scanning
/// forward. Returns the 0-based start indices.
///
/// # Examples
///
/// ```
/// use coding_tools::block::find_starts;
///
/// let lines = ["a", "b", "c", "a", "b"];
/// let block = ["a".to_string(), "b".to_string()];
/// assert_eq!(find_starts(&lines, &block), vec![0, 3]);
/// ```
pub fn find_starts<S: AsRef<str>>(lines: &[S], block: &[String]) -> Vec<usize> {
    let k = block.len();
    if k == 0 || lines.len() < k {
        return Vec::new();
    }
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + k <= lines.len() {
        if block
            .iter()
            .zip(&lines[i..i + k])
            .all(|(b, l)| b == l.as_ref())
        {
            starts.push(i);
            i += k; // non-overlapping: continue past the match
        } else {
            i += 1;
        }
    }
    starts
}

/// Report the best partial alignment of an unmatched `block` against `lines`:
/// the start with the longest run of matching leading block lines (ties go to
/// the earliest). When no line equals the block's first line at all, falls
/// back to a whitespace-insensitive scan of that first line, so indentation
/// drift — the most common anchor failure — is still diagnosed.
pub fn nearest_miss<S: AsRef<str>>(lines: &[S], block: &[String]) -> Option<NearestMiss> {
    if block.is_empty() || lines.is_empty() {
        return None;
    }
    let mut best: Option<(usize, usize)> = None; // (matched_prefix_len, start)
    for start in 0..lines.len() {
        if lines[start].as_ref() != block[0] {
            continue;
        }
        let mut len = 0usize;
        while len < block.len()
            && start + len < lines.len()
            && lines[start + len].as_ref() == block[len]
        {
            len += 1;
        }
        if best.is_none_or(|(blen, _)| len > blen) {
            best = Some((len, start));
        }
    }
    if let Some((len, start)) = best {
        // len == block.len() would have been a match; here it is a prefix.
        let found = lines
            .get(start + len)
            .map(|l| l.as_ref().to_string())
            .unwrap_or_default();
        return Some(NearestMiss {
            line: start + 1,
            first_diverging_line: len + 1,
            expected: block.get(len).cloned().unwrap_or_default(),
            found,
        });
    }
    // No exact first-line anchor anywhere: diagnose whitespace drift on the
    // first line if a trim-equal candidate exists.
    let want = block[0].trim();
    if want.is_empty() {
        return None;
    }
    lines
        .iter()
        .position(|l| l.as_ref().trim() == want)
        .map(|i| NearestMiss {
            line: i + 1,
            first_diverging_line: 1,
            expected: block[0].clone(),
            found: lines[i].as_ref().to_string(),
        })
}

use crate::edit::Site;

/// Replace every non-overlapping occurrence of `block` in `content` with
/// `replacement` lines, preserving every untouched byte (including a missing
/// final newline). An empty `replacement` deletes the matched lines entirely.
/// Returns the new content, the occurrence count, and the changed sites
/// (`line` is the block's 1-based start; `before`/`after` are newline-joined).
///
/// # Examples
///
/// ```
/// use coding_tools::block::edit_blocks;
///
/// let block = vec!["b".to_string(), "c".to_string()];
/// let repl = vec!["X".to_string()];
/// let (out, n, sites) = edit_blocks("f", "a\nb\nc\nd\n", &block, &repl);
/// assert_eq!(out, "a\nX\nd\n");
/// assert_eq!(n, 1);
/// assert_eq!(sites[0].line, 2);
///
/// // Empty replacement deletes the block's lines.
/// let (out, _, _) = edit_blocks("f", "a\nb\nc\nd\n", &block, &[]);
/// assert_eq!(out, "a\nd\n");
/// ```
pub fn edit_blocks(
    path: &str,
    content: &str,
    block: &[String],
    replacement: &[String],
) -> (String, usize, Vec<Site>) {
    // Split into (body, terminator) per line so untouched bytes round-trip.
    let segments: Vec<(&str, &str)> = content
        .split_inclusive('\n')
        .map(|seg| match seg.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (seg, ""),
        })
        .collect();
    let bodies: Vec<&str> = segments.iter().map(|(b, _)| *b).collect();
    let starts = find_starts(&bodies, block);
    if starts.is_empty() {
        return (content.to_string(), 0, Vec::new());
    }

    let mut out = String::with_capacity(content.len());
    let mut sites = Vec::new();
    let mut next = starts.iter().peekable();
    let mut i = 0usize;
    while i < segments.len() {
        if next.peek() == Some(&&i) {
            next.next();
            // The terminator after the block: taken from its last line, so a
            // block ending at EOF-without-newline stays unterminated.
            let last_nl = segments[i + block.len() - 1].1;
            for (r, rl) in replacement.iter().enumerate() {
                out.push_str(rl);
                out.push_str(if r + 1 == replacement.len() {
                    last_nl
                } else {
                    "\n"
                });
            }
            sites.push(Site {
                path: path.to_string(),
                line: i + 1,
                before: block.join("\n"),
                after: replacement.join("\n"),
            });
            i += block.len();
        } else {
            out.push_str(segments[i].0);
            out.push_str(segments[i].1);
            i += 1;
        }
    }

    (out, starts.len(), sites)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn matches_are_byte_exact_and_non_overlapping() {
        let lines = ["a", "a", "a"];
        assert_eq!(find_starts(&lines, &block(&["a", "a"])), vec![0]);
        // Whitespace is significant.
        assert!(find_starts(&["  x"], &block(&["x"])).is_empty());
    }

    #[test]
    fn nearest_miss_reports_first_divergence() {
        let lines = ["fn a() {", "    one();", "    two();", "}"];
        let b = block(&["fn a() {", "    one();", "    three();"]);
        let m = nearest_miss(&lines, &b).unwrap();
        assert_eq!(m.line, 1);
        assert_eq!(m.first_diverging_line, 3);
        assert_eq!(m.expected, "    three();");
        assert_eq!(m.found, "    two();");
    }

    #[test]
    fn nearest_miss_diagnoses_whitespace_drift_on_the_anchor_line() {
        let lines = ["\tindented();"];
        let b = block(&["    indented();"]);
        let m = nearest_miss(&lines, &b).unwrap();
        assert_eq!(m.line, 1);
        assert_eq!(m.first_diverging_line, 1);
        assert_eq!(m.found, "\tindented();");
    }

    #[test]
    fn nearest_miss_past_eof_reports_empty_found() {
        let lines = ["a"];
        let b = block(&["a", "b"]);
        let m = nearest_miss(&lines, &b).unwrap();
        assert_eq!((m.line, m.first_diverging_line), (1, 2));
        assert_eq!(m.found, "");
    }

    #[test]
    fn block_edit_preserves_missing_final_newline() {
        let b = block(&["x"]);
        let (out, n, _) = edit_blocks("f", "a\nx", &b, &block(&["y", "z"]));
        assert_eq!(out, "a\ny\nz");
        assert_eq!(n, 1);
    }

    #[test]
    fn block_edit_replaces_multiple_sites() {
        let b = block(&["x"]);
        let (out, n, sites) = edit_blocks("f", "x\nm\nx\n", &b, &block(&["y"]));
        assert_eq!(out, "y\nm\ny\n");
        assert_eq!(n, 2);
        assert_eq!(sites.iter().map(|s| s.line).collect::<Vec<_>>(), vec![1, 3]);
    }
}
