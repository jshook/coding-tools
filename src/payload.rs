// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `file:` / `text:` value schemes shared by every payload-typed option.
//!
//! Payload-typed options (patterns, replacements, structured values, stdin
//! text, prose) accept a scheme prefix that says where the value comes from:
//!
//! * `file:PATH` — the value is the file's contents, read verbatim (exact
//!   bytes, UTF-8). A `file:`-sourced pattern is never promoted: its match
//!   mode defaults to literal.
//! * `text:VALUE` — the remainder is the literal value; the escape hatch for
//!   a payload that genuinely begins with `file:` or `text:`.
//!
//! Only these two exact prefixes are recognised. Everything else is literal
//! as-is — there is no general `scheme:` reservation, so values like
//! `http://…` and `std::fmt` are unaffected.

/// A payload value with its origin, after scheme resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The resolved value text.
    pub text: String,
    /// True when the value was read from a `file:` source — such payloads
    /// are verbatim: never promoted, matched literally by default.
    pub from_file: bool,
}

/// Resolve a raw option value through the `file:` / `text:` schemes.
///
/// # Examples
///
/// ```
/// use coding_tools::payload::resolve;
///
/// // No recognised prefix: the value is literal as-is.
/// assert_eq!(resolve("http://example.com").unwrap().text, "http://example.com");
/// assert!(!resolve("std::fmt").unwrap().from_file);
///
/// // text: strips the prefix and nothing else.
/// assert_eq!(resolve("text:file:not-a-path").unwrap().text, "file:not-a-path");
/// ```
pub fn resolve(raw: &str) -> Result<Resolved, String> {
    if let Some(path) = raw.strip_prefix("file:") {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading payload file '{path}': {e}"))?;
        Ok(Resolved {
            text,
            from_file: true,
        })
    } else if let Some(rest) = raw.strip_prefix("text:") {
        Ok(Resolved {
            text: rest.to_string(),
            from_file: false,
        })
    } else {
        Ok(Resolved {
            text: raw.to_string(),
            from_file: false,
        })
    }
}

/// Split a payload into its lines for line-anchored matching. A single final
/// terminating newline ends the last line — it does not add an empty trailing
/// line. A trailing `\r` on each line is dropped, so a CRLF-terminated payload
/// (e.g. an anchor file saved by a Windows editor) matches LF source. An empty
/// payload has zero lines.
///
/// # Examples
///
/// ```
/// use coding_tools::payload::to_lines;
///
/// assert_eq!(to_lines("foo\n"), vec!["foo"]);          // final newline ends the line
/// assert_eq!(to_lines("a\nb"), vec!["a", "b"]);
/// assert_eq!(to_lines("a\n\n"), vec!["a", ""]);        // an intentional blank line stays
/// assert_eq!(to_lines("a\r\nb\r\n"), vec!["a", "b"]);  // CRLF normalized to LF
/// assert!(to_lines("").is_empty());                     // empty payload: zero lines
/// ```
pub fn to_lines(payload: &str) -> Vec<String> {
    if payload.is_empty() {
        return Vec::new();
    }
    let body = payload.strip_suffix('\n').unwrap_or(payload);
    body.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect()
}

/// Split a `find`/anchor payload into the lines a block match anchors on:
/// [`to_lines`], then drop any trailing empty lines.
///
/// Editors and file-writers terminate files with a newline and frequently leave
/// a final blank line; without trimming, that becomes a phantom empty line at
/// the tail of the anchor and a K-line block fails to match as a K+1-line one.
/// Interior blank lines and whitespace-only lines are preserved (whitespace is
/// significant in a block match); only exactly-empty (`""`) trailing lines are
/// removed. An anchor that is entirely blank therefore reduces to zero lines.
///
/// # Examples
///
/// ```
/// use coding_tools::payload::to_find_lines;
///
/// // A trailing blank line (e.g. a file ending in two newlines) is dropped.
/// assert_eq!(to_find_lines("a\nb\n\n"), vec!["a", "b"]);
/// // A single terminator already collapses, same as `to_lines`.
/// assert_eq!(to_find_lines("a\nb\n"), vec!["a", "b"]);
/// // An interior blank line is significant and kept.
/// assert_eq!(to_find_lines("a\n\nb\n"), vec!["a", "", "b"]);
/// // An all-blank anchor reduces to nothing.
/// assert!(to_find_lines("\n\n").is_empty());
/// ```
pub fn to_find_lines(payload: &str) -> Vec<String> {
    let mut lines = to_lines(payload);
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unprefixed_values_pass_through_verbatim() {
        for raw in ["plain", "http://x/y", "std::fmt", "a file: in the middle"] {
            let r = resolve(raw).unwrap();
            assert_eq!(r.text, raw);
            assert!(!r.from_file);
        }
    }

    #[test]
    fn text_prefix_strips_once_and_only_once() {
        assert_eq!(resolve("text:text:x").unwrap().text, "text:x");
        assert_eq!(resolve("text:").unwrap().text, "");
    }

    #[test]
    fn file_prefix_reads_exact_bytes() {
        let dir = std::env::temp_dir().join("ct-payload-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("payload.block");
        std::fs::write(&p, "  indented(line),\nnext\n").unwrap();
        let r = resolve(&format!("file:{}", p.display())).unwrap();
        assert!(r.from_file);
        assert_eq!(r.text, "  indented(line),\nnext\n");
    }

    #[test]
    fn missing_payload_file_is_an_error() {
        assert!(resolve("file:/no/such/payload").is_err());
    }

    #[test]
    fn to_lines_treats_one_trailing_newline_as_a_terminator() {
        assert_eq!(to_lines("foo\n"), vec!["foo"]);
        assert_eq!(to_lines("a\nb"), vec!["a", "b"]);
        // A second trailing newline is an intentional blank line, kept here.
        assert_eq!(to_lines("a\n\n"), vec!["a", ""]);
        assert!(to_lines("").is_empty());
    }

    #[test]
    fn to_lines_normalizes_crlf_to_lf() {
        assert_eq!(to_lines("a\r\nb\r\n"), vec!["a", "b"]);
        assert_eq!(to_lines("solo\r\n"), vec!["solo"]);
        // A lone CR that is not a line terminator is left untouched.
        assert_eq!(to_lines("a\rb\n"), vec!["a\rb"]);
        // CRLF plus a trailing blank line: terminators stripped, blank kept.
        assert_eq!(to_lines("a\r\n\r\n"), vec!["a", ""]);
    }

    #[test]
    fn to_find_lines_drops_trailing_blank_lines() {
        // The phantom-trailing-line case: a 2-line anchor + an extra blank line.
        assert_eq!(to_find_lines("a\nb\n\n"), vec!["a", "b"]);
        // Several trailing blanks all go.
        assert_eq!(to_find_lines("a\nb\n\n\n"), vec!["a", "b"]);
        // CRLF anchor with a trailing blank line: normalized and trimmed.
        assert_eq!(to_find_lines("a\r\nb\r\n\r\n"), vec!["a", "b"]);
        // A single terminator behaves exactly like `to_lines`.
        assert_eq!(to_find_lines("a\nb\n"), vec!["a", "b"]);
        // Interior blanks and whitespace-only lines are significant, so kept.
        assert_eq!(to_find_lines("a\n\nb\n"), vec!["a", "", "b"]);
        assert_eq!(to_find_lines("a\n   \n"), vec!["a", "   "]);
        // An all-blank anchor reduces to nothing.
        assert!(to_find_lines("\n\n").is_empty());
        assert!(to_find_lines("").is_empty());
    }
}
