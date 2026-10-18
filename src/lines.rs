// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Line-preserving splices: the one place the suite's editors turn a set of
//! line-range changes back into file bytes.
//!
//! A file is split into `(body, terminator)` segments, where the terminator is
//! the whole line ending (`\r\n`, `\n`, a lone `\r`, or nothing at EOF). Every
//! editor — block replace, single-line replace, insert, drop, append — states
//! its change as a [`Splice`] over those segments, and [`render`] writes the
//! result with every untouched byte intact and every *new* line ending in the
//! style of the lines around it. So a CRLF file stays CRLF, an LF file stays
//! LF, and a file without a final newline keeps not having one, whichever
//! editor touched it and whether the batch cascaded or not.

/// A file split into line bodies and their terminators.
#[derive(Debug, Clone)]
pub struct Segments<'a> {
    /// `(body, terminator)` per line; bodies carry no `\r`.
    pub segs: Vec<(&'a str, &'a str)>,
    /// The file's dominant newline: `\r\n` if any line ends that way, else `\n`.
    pub nl: &'static str,
}

impl<'a> Segments<'a> {
    /// Split `content`. An empty file has zero segments.
    ///
    /// # Examples
    ///
    /// ```
    /// use coding_tools::lines::Segments;
    ///
    /// let s = Segments::split("a\r\nb\n c");
    /// assert_eq!(s.segs, vec![("a", "\r\n"), ("b", "\n"), (" c", "")]);
    /// assert_eq!(s.nl, "\r\n");
    /// assert!(Segments::split("").segs.is_empty());
    /// ```
    pub fn split(content: &'a str) -> Self {
        let segs = content
            .split_inclusive('\n')
            .map(|seg| {
                if let Some(b) = seg.strip_suffix("\r\n") {
                    (b, "\r\n")
                } else if let Some(b) = seg.strip_suffix('\n') {
                    (b, "\n")
                } else if let Some(b) = seg.strip_suffix('\r') {
                    (b, "\r")
                } else {
                    (seg, "")
                }
            })
            .collect();
        let nl = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        Segments { segs, nl }
    }

    /// The line bodies alone, for matching.
    pub fn bodies(&self) -> Vec<&'a str> {
        self.segs.iter().map(|(b, _)| *b).collect()
    }

    /// Number of lines.
    pub fn len(&self) -> usize {
        self.segs.len()
    }

    /// Whether the file is empty.
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }
}

/// One line-range change: replace `len` lines starting at 0-based `start`
/// with `lines`. `len == 0` is a pure insertion before line `start` (`start ==
/// number of lines` appends at EOF); empty `lines` with `len > 0` deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    pub start: usize,
    pub len: usize,
    pub lines: Vec<String>,
}

/// Render `segs` with `splices` applied. Splices must be sorted by `start`
/// and non-overlapping; several at the same `start` apply in the order given
/// (an insertion listed before a replacement at the same line lands above
/// it). Line endings for new lines:
///
/// * a **replacement** (`len > 0`) takes the replaced span's newline style for
///   its interior lines, and its last line inherits the span's last
///   terminator — so a block ending at EOF without a newline stays that way;
/// * an **insertion** (`len == 0`) takes the newline of the line before it
///   (else after it, else the file's dominant style) for every line; an
///   insertion past a final line that has no terminator first terminates that
///   line, and its own last line then inherits the missing terminator, so the
///   file still ends without a newline.
///
/// # Examples
///
/// ```
/// use coding_tools::lines::{render, Segments, Splice};
///
/// let s = Segments::split("a\r\nb\r\nc");
/// let out = render(&s, &[
///     Splice { start: 1, len: 0, lines: vec!["x".into()] },      // insert before b
///     Splice { start: 2, len: 1, lines: vec!["C1".into(), "C2".into()] }, // replace c
/// ]);
/// assert_eq!(out, "a\r\nx\r\nb\r\nC1\r\nC2");
///
/// // Appending to a file with no final newline keeps it that way.
/// let s = Segments::split("a\nb");
/// let out = render(&s, &[Splice { start: 2, len: 0, lines: vec!["c".into()] }]);
/// assert_eq!(out, "a\nb\nc");
/// ```
pub fn render(segs: &Segments<'_>, splices: &[Splice]) -> String {
    let n = segs.len();
    let mut out = String::new();
    let mut next = splices.iter().peekable();
    let mut i = 0usize;
    // Whether the previously emitted line ended without a terminator (only
    // possible for the file's last line); an insertion after it must
    // terminate it first.
    let mut dangling = false;
    while i <= n {
        while let Some(sp) = next.next_if(|sp| sp.start == i) {
            debug_assert!(sp.start + sp.len <= n, "splice past EOF");
            if sp.len == 0 {
                let nl = if i > 0 && !segs.segs[i - 1].1.is_empty() {
                    segs.segs[i - 1].1
                } else if i < n && !segs.segs[i].1.is_empty() {
                    segs.segs[i].1
                } else {
                    segs.nl
                };
                if dangling {
                    out.push_str(nl);
                    dangling = false;
                }
                // Past a final unterminated line, the insertion inherits the
                // missing terminator; elsewhere every inserted line is ended.
                let inherit_none = i == n && i > 0 && segs.segs[i - 1].1.is_empty();
                for (r, l) in sp.lines.iter().enumerate() {
                    out.push_str(l);
                    if r + 1 == sp.lines.len() && inherit_none {
                        dangling = true;
                    } else {
                        out.push_str(nl);
                    }
                }
            } else {
                let span = &segs.segs[i..i + sp.len];
                let last_term = span[sp.len - 1].1;
                let nl = span
                    .iter()
                    .map(|(_, t)| *t)
                    .find(|t| !t.is_empty())
                    .unwrap_or(segs.nl);
                for (r, l) in sp.lines.iter().enumerate() {
                    out.push_str(l);
                    out.push_str(if r + 1 == sp.lines.len() {
                        last_term
                    } else {
                        nl
                    });
                }
                dangling = !sp.lines.is_empty() && last_term.is_empty();
                i += sp.len;
            }
        }
        if i < n {
            let (body, term) = segs.segs[i];
            out.push_str(body);
            out.push_str(term);
            dangling = term.is_empty();
            i += 1;
        } else {
            break;
        }
    }
    debug_assert!(next.peek().is_none(), "splice past EOF");
    out
}

/// The line-ending policy of `--eol`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Eol {
    /// Preserve each file's own endings (the default).
    #[default]
    Keep,
    /// Write every line ending as `\n`.
    Lf,
    /// Write every line ending as `\r\n`.
    Crlf,
}

/// Rewrite every line terminator of `content` per `eol`; `Keep` returns it
/// unchanged. A missing final newline stays missing.
///
/// # Examples
///
/// ```
/// use coding_tools::lines::{normalize_eol, Eol};
///
/// assert_eq!(normalize_eol("a\r\nb\nc", Eol::Lf), "a\nb\nc");
/// assert_eq!(normalize_eol("a\r\nb\nc\n", Eol::Crlf), "a\r\nb\r\nc\r\n");
/// assert_eq!(normalize_eol("a\r\nb\n", Eol::Keep), "a\r\nb\n");
/// ```
pub fn normalize_eol(content: &str, eol: Eol) -> String {
    let want = match eol {
        Eol::Keep => return content.to_string(),
        Eol::Lf => "\n",
        Eol::Crlf => "\r\n",
    };
    let segs = Segments::split(content);
    let mut out = String::with_capacity(content.len() + segs.len());
    for (body, term) in &segs.segs {
        out.push_str(body);
        if !term.is_empty() {
            out.push_str(want);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(start: usize, len: usize, lines: &[&str]) -> Splice {
        Splice {
            start,
            len,
            lines: lines.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn render_without_splices_round_trips_every_byte() {
        for c in ["", "a", "a\n", "a\r\nb\n", "a\n\n", "x\ry\n", "a\r\nb"] {
            assert_eq!(render(&Segments::split(c), &[]), c, "{c:?}");
        }
    }

    #[test]
    fn insertions_take_their_neighbours_newline_style() {
        let s = Segments::split("a\r\nb\r\n");
        assert_eq!(render(&s, &[sp(0, 0, &["x"])]), "x\r\na\r\nb\r\n"); // prepend
        assert_eq!(render(&s, &[sp(2, 0, &["x", "y"])]), "a\r\nb\r\nx\r\ny\r\n"); // append
        assert_eq!(render(&s, &[sp(1, 0, &["x"])]), "a\r\nx\r\nb\r\n");
    }

    #[test]
    fn appending_to_an_empty_or_unterminated_file() {
        let empty = Segments::split("");
        assert_eq!(render(&empty, &[sp(0, 0, &["x", "y"])]), "x\ny\n");
        let bare = Segments::split("a");
        assert_eq!(render(&bare, &[sp(1, 0, &["x", "y"])]), "a\nx\ny");
        // Prepending to an unterminated single line leaves it unterminated.
        assert_eq!(render(&bare, &[sp(0, 0, &["x"])]), "x\na");
    }

    #[test]
    fn replacement_inherits_the_last_terminator_and_deletes_when_empty() {
        let s = Segments::split("a\nb\nc");
        assert_eq!(render(&s, &[sp(2, 1, &["C"])]), "a\nb\nC");
        assert_eq!(render(&s, &[sp(1, 1, &[])]), "a\nc");
        assert_eq!(render(&s, &[sp(0, 3, &["one"])]), "one");
        // Deleting the unterminated last line leaves the previous terminator.
        assert_eq!(render(&s, &[sp(2, 1, &[])]), "a\nb\n");
    }

    #[test]
    fn same_start_splices_apply_in_order() {
        let s = Segments::split("a\nb\n");
        // Insert before b, then replace b: the insertion lands above.
        let out = render(&s, &[sp(1, 0, &["x"]), sp(1, 1, &["B"])]);
        assert_eq!(out, "a\nx\nB\n");
        // Two insertions at the same point keep their order.
        let out = render(&s, &[sp(1, 0, &["x"]), sp(1, 0, &["y"])]);
        assert_eq!(out, "a\nx\ny\nb\n");
        // Replace a, then insert after it (start 1): the insertion follows.
        let out = render(&s, &[sp(0, 1, &["A"]), sp(1, 0, &["x"])]);
        assert_eq!(out, "A\nx\nb\n");
    }

    #[test]
    fn insert_after_an_unterminated_last_line_terminates_it_first() {
        let s = Segments::split("a\r\nb");
        assert_eq!(render(&s, &[sp(2, 0, &["x"])]), "a\r\nb\r\nx");
        // Replacing the last line with nothing, then appending: the append
        // follows the remaining terminated line normally.
        assert_eq!(render(&s, &[sp(1, 1, &[]), sp(2, 0, &["x"])]), "a\r\nx");
    }

    #[test]
    fn eol_normalization() {
        // A lone `\r` inside a line is not a terminator anywhere in the suite,
        // so it is left alone.
        assert_eq!(normalize_eol("a\r\nb\rc\n", Eol::Lf), "a\nb\rc\n");
        assert_eq!(normalize_eol("a\nb", Eol::Crlf), "a\r\nb");
        assert_eq!(normalize_eol("", Eol::Crlf), "");
    }
}
