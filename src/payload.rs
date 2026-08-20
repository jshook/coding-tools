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
//! * `stdin:` — the value is everything on standard input, read verbatim, with
//!   the same never-promoted semantics as `file:`. This is the shell-safe way
//!   to pass a payload containing backticks, `$`, or quotes: the text never
//!   passes through the shell's word expansion at all, and no temp file is
//!   needed. Standard input is read **once** per process, so several `stdin:`
//!   payloads in one invocation all resolve to that same text.
//! * `text:VALUE` — the remainder is the literal value; the escape hatch for
//!   a payload that genuinely begins with `file:`, `stdin:`, or `text:`.
//!
//! Only these three exact prefixes are recognised. Everything else is literal
//! as-is — there is no general `scheme:` reservation, so values like
//! `http://…` and `std::fmt` are unaffected.

/// A payload value with its origin, after scheme resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The resolved value text.
    pub text: String,
    /// True when the value came from a **verbatim** source (`file:` or
    /// `stdin:`) rather than an argv literal — such payloads are never
    /// promoted, and are matched literally by default.
    pub from_file: bool,
}

/// Standard input, read at most once per process. Every `stdin:` payload in an
/// invocation resolves to this same text — a pipe can only be drained once, and
/// a second read would silently hand back the empty string.
static STDIN_PAYLOAD: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();

/// All of standard input, as the `stdin:` scheme reads it.
///
/// Public so a tool with its own stdin-reading flag (`ct-each --stdin`) can
/// share the same single read: a process has one standard input, and two
/// independent reads of it would leave the second holding an empty string.
pub fn stdin_text() -> Result<String, String> {
    read_stdin()
}

/// Read all of standard input as the payload text. Cached, so `--find stdin:`
/// and `--replace stdin:` in one call see the same bytes instead of the second
/// one seeing an already-drained pipe.
fn read_stdin() -> Result<String, String> {
    STDIN_PAYLOAD
        .get_or_init(|| {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map(|_| buf)
                .map_err(|e| format!("reading the stdin: payload: {e}"))
        })
        .clone()
}

/// Resolve a raw option value through the `file:` / `stdin:` / `text:` schemes.
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
    } else if raw == "stdin:" || raw == "stdin:-" {
        Ok(Resolved {
            text: read_stdin()?,
            from_file: true,
        })
    } else if let Some(rest) = raw.strip_prefix("stdin:") {
        Err(format!(
            "the stdin: payload scheme takes no argument (got 'stdin:{rest}'); write \
             `stdin:` on its own and pipe the value in, or use `text:stdin:{rest}` for a \
             literal value that starts with 'stdin:'"
        ))
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
    fn stdin_scheme_rejects_an_argument_with_the_escape_hatch_named() {
        let e = resolve("stdin:some/path").unwrap_err();
        assert!(e.contains("takes no argument"), "{e}");
        assert!(e.contains("text:stdin:some/path"), "{e}");
        // The escape hatch itself stays literal, and is not a verbatim source.
        let r = resolve("text:stdin:some/path").unwrap();
        assert_eq!(r.text, "stdin:some/path");
        assert!(!r.from_file);
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
