// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Pure helpers behind `ct-tree`'s reporting: per-file line/word/character
//! counts, the metric-bound predicate, and the immediate-parent grouping used by
//! the per-folder predicate and the directory summary.

/// Count lines, words, and characters of `content`.
///
/// Lines are counted `wc`-style (a final line without a trailing newline still
/// counts); words are whitespace-separated; characters are Unicode scalar values
/// and include newlines (like `wc -m`).
///
/// # Examples
///
/// ```
/// use coding_tools::tree::metrics;
///
/// // 2 lines; words a, b, cd; 7 chars including both newlines.
/// assert_eq!(metrics("a b\ncd\n"), (2, 3, 7));
/// // a final line without a newline still counts as a line.
/// assert_eq!(metrics("one two"), (1, 2, 7));
/// assert_eq!(metrics(""), (0, 0, 0));
/// ```
pub fn metrics(content: &str) -> (u64, u64, u64) {
    let lines = content.lines().count() as u64;
    let words = content.split_whitespace().count() as u64;
    let chars = content.chars().count() as u64;
    (lines, words, chars)
}

/// Whether a value satisfies an optional `>= min` / `<= max` pair (an absent
/// bound never constrains).
///
/// # Examples
///
/// ```
/// use coding_tools::tree::within;
///
/// assert!(within(10, Some(5), Some(20)));
/// assert!(!within(3, Some(5), None));   // below min
/// assert!(!within(30, None, Some(20))); // above max
/// assert!(within(10, None, None));      // unbounded
/// ```
pub fn within(value: u64, min: Option<u64>, max: Option<u64>) -> bool {
    min.is_none_or(|m| value >= m) && max.is_none_or(|m| value <= m)
}

/// The immediate parent directory of a relative path (`"."` for a root-level
/// file). Used to group files for the per-folder predicate and `--summary dir`.
///
/// # Examples
///
/// ```
/// use coding_tools::tree::parent_dir;
///
/// assert_eq!(parent_dir("a/b/c.rs"), "a/b");
/// assert_eq!(parent_dir("top.rs"), ".");
/// ```
pub fn parent_dir(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => ".".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_count_lines_words_chars() {
        // chars counts newlines too (like `wc -m`): a, space, b, \n, c, d, \n = 7.
        assert_eq!(metrics("a b\ncd\n"), (2, 3, 7));
        assert_eq!(metrics("one two"), (1, 2, 7));
        assert_eq!(metrics(""), (0, 0, 0));
    }

    #[test]
    fn within_bounds() {
        assert!(within(10, Some(5), Some(20)));
        assert!(!within(3, Some(5), None));
        assert!(!within(30, None, Some(20)));
        assert!(within(10, None, None));
    }

    #[test]
    fn parent_dir_of_paths() {
        assert_eq!(parent_dir("a/b/c.rs"), "a/b");
        assert_eq!(parent_dir("top.rs"), ".");
    }
}
