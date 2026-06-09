// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Token substitution for `--emit` verdict templates, shared by every tool.
//!
//! A template carries `{TOKEN}` placeholders; [`render`] expands the ones it
//! recognises from a `(name, value)` table and leaves anything else — including
//! unknown `{TOKEN}`s and stray braces — untouched. The substitution is a single
//! left-to-right pass, so replacement text is **never rescanned**: a value that
//! happens to contain `{RESULT}` (e.g. a command's captured stdout) is emitted
//! verbatim rather than re-expanded.

/// Expand recognised `{TOKEN}` placeholders in `template` from `tokens`.
///
/// Tokens are matched by exact name between a `{` and the next `}`. An unknown
/// token, or a `{` with no closing `}`, is copied through unchanged.
///
/// # Examples
///
/// ```
/// use coding_tools::template::render;
///
/// let tokens = [("RESULT", "SUCCESS"), ("CODE", "0")];
/// assert_eq!(render("{RESULT} (exit {CODE})", &tokens), "SUCCESS (exit 0)");
///
/// // Unknown tokens pass through, and replacement text is never re-scanned.
/// assert_eq!(render("{MISSING}", &[]), "{MISSING}");
/// assert_eq!(render("{X}", &[("X", "{CODE}")]), "{CODE}");
/// ```
pub fn render(template: &str, tokens: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close_rel) => {
                let name = &after[..close_rel];
                match tokens.iter().find(|(n, _)| *n == name) {
                    Some((_, value)) => out.push_str(value),
                    None => {
                        // Unknown token: keep the braces and name verbatim.
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close_rel + 1..];
            }
            None => {
                // Unbalanced '{': nothing left to substitute.
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_known_tokens() {
        let out = render(
            "{QUESTION} -> {RESULT}",
            &[("QUESTION", "ok?"), ("RESULT", "SUCCESS")],
        );
        assert_eq!(out, "ok? -> SUCCESS");
    }

    #[test]
    fn leaves_unknown_tokens_verbatim() {
        let out = render("{RESULT} {MISSING}", &[("RESULT", "ERROR")]);
        assert_eq!(out, "ERROR {MISSING}");
    }

    #[test]
    fn does_not_rescan_substituted_values() {
        // A value containing a token name must not be re-expanded.
        let out = render(
            "{STDOUT}|{CODE}",
            &[("STDOUT", "see {CODE}"), ("CODE", "0")],
        );
        assert_eq!(out, "see {CODE}|0");
    }

    #[test]
    fn copies_unbalanced_brace_through() {
        let out = render("a {RESULT} b {dangling", &[("RESULT", "SUCCESS")]);
        assert_eq!(out, "a SUCCESS b {dangling");
    }
}
