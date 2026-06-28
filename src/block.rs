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
    /// Total number of lines in the find block (for self-diagnosing output).
    pub block_len: usize,
}

impl NearestMiss {
    /// A diagnostic note when the divergence is likely a *stray blank line* in
    /// the find payload — the expected block line is empty, which an editor's
    /// trailing newline (or a hand-pasted blank line) commonly produces — else
    /// `None`. Block anchors taken from `file:` payloads have their trailing
    /// blank lines trimmed, so this remains useful mainly for inline/`text:`
    /// payloads and interior empty lines.
    pub fn blank_line_hint(&self) -> Option<String> {
        self.expected.is_empty().then(|| {
            format!(
                "the find block's line {} (of {}) is empty — likely a stray blank or \
                 trailing line in the payload; trim it, or pass the anchor via text:",
                self.first_diverging_line, self.block_len
            )
        })
    }
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

/// Whether a line counts as *blank* for blank-run squeezing: empty or
/// whitespace-only. Squeezing deliberately treats a `"   "` line as blank.
fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// Align `block` against `lines` from source index `start`, *squeezing blank
/// runs*: a maximal run of blank lines in `block` matches a run of one or more
/// blank lines in the source, and non-blank block lines must match the source
/// byte-for-byte. On success returns the number of source lines consumed; on
/// failure returns `(block index that first diverged, source index reached)`.
fn align_squeezed<S: AsRef<str>>(
    lines: &[S],
    block: &[String],
    start: usize,
) -> Result<usize, (usize, usize)> {
    let mut bi = 0usize;
    let mut li = start;
    while bi < block.len() {
        if is_blank(&block[bi]) {
            let run_start = bi;
            while bi < block.len() && is_blank(&block[bi]) {
                bi += 1;
            }
            // A blank run in the block requires at least one source blank line.
            if li >= lines.len() || !is_blank(lines[li].as_ref()) {
                return Err((run_start, li));
            }
            while li < lines.len() && is_blank(lines[li].as_ref()) {
                li += 1;
            }
        } else {
            if li >= lines.len() || lines[li].as_ref() != block[bi] {
                return Err((bi, li));
            }
            bi += 1;
            li += 1;
        }
    }
    Ok(li - start)
}

/// Find every non-overlapping *squeezed* match of `block` in `lines`, scanning
/// forward (see [`align_squeezed`]). Returns each match's `(0-based start,
/// source-line count)` span — the span can be longer than the block when the
/// source has wider blank runs than the anchor.
///
/// # Examples
///
/// ```
/// use coding_tools::block::find_spans_squeezed;
///
/// // The anchor's single blank line absorbs the source's two blank lines.
/// let lines = ["foo()", "", "", "bar()"];
/// let block = ["foo()".to_string(), String::new(), "bar()".to_string()];
/// assert_eq!(find_spans_squeezed(&lines, &block), vec![(0, 4)]);
/// ```
pub fn find_spans_squeezed<S: AsRef<str>>(lines: &[S], block: &[String]) -> Vec<(usize, usize)> {
    if block.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if let Ok(len) = align_squeezed(lines, block, i) {
            spans.push((i, len));
            i += len.max(1); // non-overlapping
        } else {
            i += 1;
        }
    }
    spans
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
            block_len: block.len(),
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
            block_len: block.len(),
        })
}

/// [`nearest_miss`], selecting the exact or blank-run-squeezing matcher by
/// `squeeze`. Used so the diagnostic agrees with how the edit actually matched.
pub fn nearest_miss_with<S: AsRef<str>>(
    lines: &[S],
    block: &[String],
    squeeze: bool,
) -> Option<NearestMiss> {
    if squeeze {
        nearest_miss_squeezed(lines, block)
    } else {
        nearest_miss(lines, block)
    }
}

/// The squeeze-aware partial alignment: the anchorable start that consumed the
/// longest run of leading block lines before diverging (ties go to the
/// earliest), with blank runs squeezed exactly as [`find_spans_squeezed`] does.
/// Falls back to the same whitespace-trim scan of the first line as the exact
/// matcher when no start anchors.
fn nearest_miss_squeezed<S: AsRef<str>>(lines: &[S], block: &[String]) -> Option<NearestMiss> {
    if block.is_empty() || lines.is_empty() {
        return None;
    }
    let first_anchors = |src: &str| {
        if is_blank(&block[0]) {
            is_blank(src)
        } else {
            src == block[0]
        }
    };
    // best = (block lines consumed before divergence, start, source index there)
    let mut best: Option<(usize, usize, usize)> = None;
    for start in 0..lines.len() {
        if !first_anchors(lines[start].as_ref()) {
            continue;
        }
        if let Err((bi, li)) = align_squeezed(lines, block, start)
            && best.is_none_or(|(blen, _, _)| bi > blen)
        {
            best = Some((bi, start, li));
        }
    }
    if let Some((bi, start, li)) = best {
        let found = lines
            .get(li)
            .map(|l| l.as_ref().to_string())
            .unwrap_or_default();
        return Some(NearestMiss {
            line: start + 1,
            first_diverging_line: bi + 1,
            expected: block.get(bi).cloned().unwrap_or_default(),
            found,
            block_len: block.len(),
        });
    }
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
            block_len: block.len(),
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
    edit_blocks_with(path, content, block, replacement, false)
}

/// [`edit_blocks`], with optional blank-run `squeeze`ing of the match (see
/// [`find_spans_squeezed`]). Under squeeze the replaced source span can be
/// longer than the block, so each [`Site::before`] carries the *actual* matched
/// source lines (identical to the block in the exact path).
pub fn edit_blocks_with(
    path: &str,
    content: &str,
    block: &[String],
    replacement: &[String],
    squeeze: bool,
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
    // Each match is a (start, source-line count) span. Exact matching always
    // spans exactly `block.len()` lines; squeezing can span more.
    let spans: Vec<(usize, usize)> = if squeeze {
        find_spans_squeezed(&bodies, block)
    } else {
        find_starts(&bodies, block)
            .into_iter()
            .map(|s| (s, block.len()))
            .collect()
    };
    if spans.is_empty() {
        return (content.to_string(), 0, Vec::new());
    }

    let mut out = String::with_capacity(content.len());
    let mut sites = Vec::new();
    let mut next = spans.iter().peekable();
    let mut i = 0usize;
    while i < segments.len() {
        if next.peek().is_some_and(|(s, _)| *s == i) {
            let (_, span) = *next.next().unwrap();
            // The terminator after the block: taken from its last line, so a
            // block ending at EOF-without-newline stays unterminated.
            let last_nl = segments[i + span - 1].1;
            for (r, rl) in replacement.iter().enumerate() {
                out.push_str(rl);
                out.push_str(if r + 1 == replacement.len() {
                    last_nl
                } else {
                    "\n"
                });
            }
            let before = segments[i..i + span]
                .iter()
                .map(|(b, _)| *b)
                .collect::<Vec<_>>()
                .join("\n");
            sites.push(Site {
                path: path.to_string(),
                line: i + 1,
                before,
                after: replacement.join("\n"),
            });
            i += span;
        } else {
            out.push_str(segments[i].0);
            out.push_str(segments[i].1);
            i += 1;
        }
    }

    (out, spans.len(), sites)
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
        assert_eq!(m.block_len, 2);
    }

    #[test]
    fn nearest_miss_carries_block_len_and_blank_line_hint() {
        // A phantom empty line 3 in the find block diverging against real source
        // is exactly the trailing-newline failure mode — the hint should fire.
        let lines = ["a", "fn x(", "    body,"];
        let b = block(&["a", "fn x(", ""]);
        let m = nearest_miss(&lines, &b).unwrap();
        assert_eq!(m.first_diverging_line, 3);
        assert_eq!(m.block_len, 3);
        assert_eq!(m.expected, "");
        let hint = m
            .blank_line_hint()
            .expect("empty expected line yields a hint");
        assert!(hint.contains("line 3 (of 3)"), "{hint}");

        // A non-empty divergence is an ordinary mismatch: no blank-line hint.
        let b2 = block(&["a", "fn y("]);
        let m2 = nearest_miss(&lines, &b2).unwrap();
        assert!(m2.blank_line_hint().is_none());
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

    #[test]
    fn squeeze_matches_blank_runs_of_any_length() {
        // Anchor with one blank line; source has two — squeezing aligns them.
        let lines = ["foo()", "", "", "bar()"];
        let b = block(&["foo()", "", "bar()"]);
        // Exact matching misses (1 blank != 2 blanks).
        assert!(find_starts(&lines, &b).is_empty());
        // Squeezed matching spans all four source lines from index 0.
        assert_eq!(find_spans_squeezed(&lines, &b), vec![(0, 4)]);
        // The reverse also holds: a 2-blank anchor matches a 1-blank source.
        let lines2 = ["foo()", "", "bar()"];
        let b2 = block(&["foo()", "", "", "bar()"]);
        assert_eq!(find_spans_squeezed(&lines2, &b2), vec![(0, 3)]);
        // Whitespace-only lines count as blank.
        let lines3 = ["a", "   ", "\t", "b"];
        let b3 = block(&["a", "", "b"]);
        assert_eq!(find_spans_squeezed(&lines3, &b3), vec![(0, 4)]);
    }

    #[test]
    fn squeeze_still_requires_at_least_one_blank_and_exact_nonblank() {
        // A blank run in the anchor needs a blank in the source: none here.
        let lines = ["a", "b"];
        let b = block(&["a", "", "b"]);
        assert!(find_spans_squeezed(&lines, &b).is_empty());
        // Non-blank lines are still byte-exact.
        let lines2 = ["a", "", "B"];
        let b2 = block(&["a", "", "b"]);
        assert!(find_spans_squeezed(&lines2, &b2).is_empty());
    }

    #[test]
    fn squeeze_edit_replaces_the_full_source_span() {
        // Two source blanks collapse into whatever the replacement specifies;
        // the matched span (4 lines) is what gets replaced, and the site's
        // `before` reflects the real source, not the anchor.
        let b = block(&["foo()", "", "bar()"]);
        let repl = block(&["foo()", "", "bar()"]);
        let (out, n, sites) = edit_blocks_with("f", "foo()\n\n\nbar()\nrest\n", &b, &repl, true);
        assert_eq!(n, 1);
        assert_eq!(out, "foo()\n\nbar()\nrest\n");
        assert_eq!(sites[0].before, "foo()\n\n\nbar()");
    }

    #[test]
    fn squeeze_nearest_miss_diverges_on_the_nonblank_line() {
        // foo() and the blank run align; `baz()` diverges from `bar()`.
        let lines = ["foo()", "", "", "bar()"];
        let b = block(&["foo()", "", "baz()"]);
        let m = nearest_miss_with(&lines, &b, true).unwrap();
        assert_eq!(m.first_diverging_line, 3);
        assert_eq!(m.expected, "baz()");
        assert_eq!(m.found, "bar()");
        assert_eq!(m.line, 1);
    }
}
