// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Pure line-selection helpers behind `ct-view`'s bounded, context-aware reads:
//! parsing a `--range` spec, expanding `--match` hits into context windows, and
//! grouping the kept line indices into contiguous runs for display.

/// Parse a `--range` spec into a 0-based inclusive `[start, end]`, clamped to a
/// file of `total` lines. Returns `None` when the range selects nothing.
///
/// Accepts `A:B` (1-based, inclusive), `A:` (to end), `:B` (from start), and a
/// bare `A` (one line). Lines are 1-based; `0` is an error.
///
/// # Examples
///
/// ```
/// use coding_tools::view::parse_range;
///
/// assert_eq!(parse_range("2:4", 10).unwrap(), Some((1, 3))); // 0-based, inclusive
/// assert_eq!(parse_range("3", 10).unwrap(), Some((2, 2)));   // a single line
/// assert_eq!(parse_range("8:", 10).unwrap(), Some((7, 9)));  // open end
/// assert_eq!(parse_range(":3", 10).unwrap(), Some((0, 2)));  // open start
/// assert_eq!(parse_range("9:99", 10).unwrap(), Some((8, 9))); // end clamps to EOF
/// assert_eq!(parse_range("20:30", 10).unwrap(), None);        // wholly past EOF
/// assert!(parse_range("0:3", 10).is_err());                   // lines are 1-based
/// ```
pub fn parse_range(spec: &str, total: usize) -> Result<Option<(usize, usize)>, String> {
    if total == 0 {
        return Ok(None);
    }
    let parse_one = |s: &str, what: &str| -> Result<usize, String> {
        s.trim()
            .parse::<usize>()
            .map_err(|_| format!("invalid {what} in range '{spec}'"))
    };

    let (start1, end1): (usize, usize) = match spec.split_once(':') {
        Some((a, b)) => {
            let start = if a.trim().is_empty() {
                1
            } else {
                parse_one(a, "start")?
            };
            let end = if b.trim().is_empty() {
                total
            } else {
                parse_one(b, "end")?
            };
            (start, end)
        }
        None => {
            let n = parse_one(spec, "line")?;
            (n, n)
        }
    };

    if start1 == 0 {
        return Err(format!("range lines are 1-based; got 0 in '{spec}'"));
    }
    if start1 > total || end1 < start1 {
        return Ok(None);
    }
    Ok(Some((
        start1 - 1,
        end1.min(total).saturating_sub(1).max(start1 - 1),
    )))
}

/// Expand each hit index by `ctx` lines and merge overlapping/adjacent windows
/// into a sorted, de-duplicated list of line indices, clamped to `total` lines.
///
/// # Examples
///
/// ```
/// use coding_tools::view::expand_and_merge;
///
/// // hits at 2 and 3 with 1 line of context overlap into a single run.
/// assert_eq!(expand_and_merge(&[2, 3], 1, 10), vec![1, 2, 3, 4]);
/// // distant hits stay separate.
/// assert_eq!(expand_and_merge(&[1, 8], 0, 10), vec![1, 8]);
/// // context clamps to the file bounds.
/// assert_eq!(expand_and_merge(&[0], 2, 3), vec![0, 1, 2]);
/// ```
pub fn expand_and_merge(hits: &[usize], ctx: usize, total: usize) -> Vec<usize> {
    if hits.is_empty() || total == 0 {
        return Vec::new();
    }
    let mut idx: Vec<usize> = Vec::new();
    let last = total - 1;
    for &h in hits {
        let lo = h.saturating_sub(ctx);
        let hi = (h + ctx).min(last);
        for i in lo..=hi {
            if idx.last() != Some(&i) {
                idx.push(i);
            }
        }
    }
    idx.sort_unstable();
    idx.dedup();
    idx
}

/// Group sorted indices into contiguous `[start, end]` runs (the display groups
/// separated by `--` in text output).
///
/// # Examples
///
/// ```
/// use coding_tools::view::segments;
///
/// assert_eq!(segments(&[1, 2, 3]), vec![(1, 3)]);
/// assert_eq!(segments(&[1, 2, 5, 6]), vec![(1, 2), (5, 6)]);
/// assert_eq!(segments(&[]), vec![]);
/// ```
pub fn segments(idx: &[usize]) -> Vec<(usize, usize)> {
    let mut segs: Vec<(usize, usize)> = Vec::new();
    for &i in idx {
        match segs.last_mut() {
            Some(seg) if i == seg.1 + 1 => seg.1 = i,
            _ => segs.push((i, i)),
        }
    }
    segs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing_handles_open_and_closed_forms() {
        assert_eq!(parse_range("2:4", 10).unwrap(), Some((1, 3)));
        assert_eq!(parse_range("3", 10).unwrap(), Some((2, 2)));
        assert_eq!(parse_range("8:", 10).unwrap(), Some((7, 9)));
        assert_eq!(parse_range(":3", 10).unwrap(), Some((0, 2)));
        assert_eq!(parse_range("20:30", 10).unwrap(), None);
        assert_eq!(parse_range("9:99", 10).unwrap(), Some((8, 9)));
        assert!(parse_range("0:3", 10).is_err());
        assert!(parse_range("x", 10).is_err());
    }

    #[test]
    fn windows_merge_when_they_overlap() {
        assert_eq!(expand_and_merge(&[2, 3], 1, 10), vec![1, 2, 3, 4]);
        assert_eq!(expand_and_merge(&[1, 8], 0, 10), vec![1, 8]);
        assert_eq!(expand_and_merge(&[0], 2, 3), vec![0, 1, 2]);
    }

    #[test]
    fn segments_split_on_gaps() {
        assert_eq!(segments(&[1, 2, 3]), vec![(1, 3)]);
        assert_eq!(segments(&[1, 2, 5, 6]), vec![(1, 2), (5, 6)]);
        assert_eq!(segments(&[]), vec![]);
    }
}
