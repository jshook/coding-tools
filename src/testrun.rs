// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Pure helpers behind `ct-test`'s output handling — currently the `--focus`
//! distiller, which reduces a captured stream to the lines that matter.

/// Distil `text` to the lines matching `re`, each with `ctx` lines of context,
/// merging overlapping windows; non-contiguous windows are separated by a `--`
/// line and every kept line is prefixed with its 1-based number. Returns `None`
/// when nothing matches.
///
/// # Examples
///
/// ```
/// use coding_tools::testrun::focus_block;
/// use coding_tools::pattern::compile;
///
/// let re = compile("ERROR").unwrap();
/// let log = "ok\nERROR: a\nok\nok\nok\nERROR: b\ntail\n";
/// // ctx 0 keeps only the matching lines; the two windows are separated by `--`.
/// assert_eq!(focus_block(log, &re, 0).unwrap(), "2: ERROR: a\n--\n6: ERROR: b");
///
/// assert!(focus_block("all clean here", &re, 0).is_none());
/// ```
pub fn focus_block(text: &str, re: &regex::Regex, ctx: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![false; lines.len()];
    let mut any = false;
    for (i, l) in lines.iter().enumerate() {
        if re.is_match(l) {
            any = true;
            let lo = i.saturating_sub(ctx);
            let hi = (i + ctx).min(lines.len().saturating_sub(1));
            keep[lo..=hi].iter_mut().for_each(|k| *k = true);
        }
    }
    if !any {
        return None;
    }
    let mut out = String::new();
    let mut prev: Option<usize> = None;
    for (i, &k) in keep.iter().enumerate() {
        if !k {
            continue;
        }
        if let Some(p) = prev
            && i > p + 1
        {
            out.push_str("--\n");
        }
        out.push_str(&format!("{}: {}\n", i + 1, lines[i]));
        prev = Some(i);
    }
    Some(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::compile;

    #[test]
    fn focus_keeps_matching_windows_with_context() {
        let re = compile("ERROR").unwrap();
        let log = "a\nb\nERROR x\nd\ne\nf\nERROR y\nh\n";
        // ctx 1 -> windows [2..4] and [6..8] (1-based), non-contiguous.
        let block = focus_block(log, &re, 1).unwrap();
        assert_eq!(block, "2: b\n3: ERROR x\n4: d\n--\n6: f\n7: ERROR y\n8: h");
    }

    #[test]
    fn focus_none_when_no_match() {
        let re = compile("ERROR").unwrap();
        assert!(focus_block("nothing relevant\n", &re, 2).is_none());
    }
}
