// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Substring → glob → regex pattern promotion, shared by every tool option that
//! accepts a *pattern*.
//!
//! The rule lets a caller write the simplest thing that expresses intent and
//! have the tool infer how literally it was meant:
//!
//! * a string with no metacharacters is a [`Literal`](PatternKind::Literal)
//!   substring (regex-escaped, matched verbatim);
//! * a string with only glob metacharacters (`*`, `?`, `[ … ]`) that is *not* a
//!   valid regex is a [`Glob`](PatternKind::Glob), converted to an equivalent
//!   regex;
//! * anything else carrying regex metacharacters and forming a valid expression
//!   is a [`Regex`](PatternKind::Regex), used exactly as written.

use regex::Regex;

/// How a raw pattern string was interpreted by [`classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    /// No metacharacters: matched verbatim (regex-escaped).
    Literal,
    /// Glob metacharacters only: converted to an equivalent regex.
    Glob,
    /// Explicit, valid regex: used as written.
    Regex,
}

/// Glob metacharacters that, on their own, signal a glob pattern.
const GLOB_META: [char; 3] = ['*', '?', '['];

/// Regex metacharacters that signal explicit regular-expression intent.
const REGEX_META: [char; 10] = ['^', '$', '(', ')', '|', '+', '{', '}', '\\', '.'];

/// Classify a raw pattern according to the suite's promotion rule.
///
/// A pattern is [`Regex`](PatternKind::Regex) only when it both carries explicit
/// regex metacharacters *and* compiles, so an invalid-as-regex string such as
/// `*.java` (leading quantifier) falls back to [`Glob`](PatternKind::Glob).
///
/// # Examples
///
/// ```
/// use coding_tools::pattern::{classify, PatternKind};
///
/// assert_eq!(classify("ERROR:"), PatternKind::Literal); // no metacharacters
/// assert_eq!(classify("*.java"), PatternKind::Glob);     // a leading `*` is not a valid regex
/// assert_eq!(classify(r"\d+"), PatternKind::Regex);      // valid regex with metacharacters
/// ```
pub fn classify(pat: &str) -> PatternKind {
    let has_glob = pat.chars().any(|c| GLOB_META.contains(&c));
    let has_regex_meta = pat.chars().any(|c| REGEX_META.contains(&c));
    if !has_glob && !has_regex_meta {
        PatternKind::Literal
    } else if has_regex_meta && Regex::new(pat).is_ok() {
        PatternKind::Regex
    } else {
        PatternKind::Glob
    }
}

/// Convert a glob pattern into an (unanchored) regular-expression *source*.
///
/// `*` and `?` do not cross a path separator (`/`), mirroring shell glob
/// semantics; `[ … ]` character classes are passed through (with a leading `!`
/// rewritten to the regex negation `^`); every other regex metacharacter is
/// escaped to a literal.
///
/// # Examples
///
/// ```
/// use coding_tools::pattern::glob_to_regex;
///
/// assert_eq!(glob_to_regex("*.rs"), r"[^/]*\.rs");
/// assert_eq!(glob_to_regex("data_[0-9]"), "data_[0-9]");
/// ```
pub fn glob_to_regex(glob: &str) -> String {
    let mut out = String::new();
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '[' => {
                out.push('[');
                if matches!(chars.peek(), Some('!')) {
                    out.push('^');
                    chars.next();
                }
                for cc in chars.by_ref() {
                    out.push(cc);
                    if cc == ']' {
                        break;
                    }
                }
            }
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Produce the regex *source* a raw pattern promotes to, without anchoring.
///
/// # Examples
///
/// ```
/// use coding_tools::pattern::promote;
///
/// assert_eq!(promote("a.b"), "a.b"); // valid regex: used as written
/// assert_eq!(promote("a+b"), "a+b"); // valid regex
/// assert_eq!(promote("*.rs"), r"[^/]*\.rs"); // glob -> regex
/// assert_eq!(promote("v1.0"), "v1.0"); // '.' is a regex metachar, kept as-is
/// ```
pub fn promote(pat: &str) -> String {
    match classify(pat) {
        PatternKind::Literal => regex::escape(pat),
        PatternKind::Glob => glob_to_regex(pat),
        PatternKind::Regex => pat.to_string(),
    }
}

/// Compile a pattern for *content / unanchored* matching (e.g. `ct-search --grep`
/// or any `ct-test` matcher): the result matches anywhere in the haystack.
///
/// # Examples
///
/// ```
/// use coding_tools::pattern::compile;
///
/// let re = compile("ERROR:").unwrap();
/// assert!(re.is_match("first line\nERROR: bad input\n"));
/// assert!(!re.is_match("all good here"));
/// ```
pub fn compile(pat: &str) -> Result<Regex, regex::Error> {
    Regex::new(&promote(pat))
}

/// Compile a pattern for *whole-name* matching: anchored to the full string, so
/// `*.java` means "the name ends in `.java`", not merely "contains".
///
/// # Examples
///
/// ```
/// use coding_tools::pattern::compile_anchored;
///
/// let re = compile_anchored("*.rs").unwrap();
/// assert!(re.is_match("main.rs"));
/// assert!(!re.is_match("main.rs.bak")); // anchored: must end in .rs
/// ```
pub fn compile_anchored(pat: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("^(?:{})$", promote(pat)))
}

/// Compile a `'|'`-separated set of whole-name alternatives, each promoted and
/// anchored. An entry name matches the set when it matches *any* alternative,
/// mirroring `find`'s `-name a -o -name b`.
///
/// # Examples
///
/// ```
/// use coding_tools::pattern::compile_name_set;
///
/// let set = compile_name_set("*.rs|*.toml").unwrap();
/// let matches = |name: &str| set.iter().any(|r| r.is_match(name));
/// assert!(matches("lib.rs"));
/// assert!(matches("Cargo.toml"));
/// assert!(!matches("README.md"));
/// ```
pub fn compile_name_set(spec: &str) -> Result<Vec<Regex>, regex::Error> {
    spec.split('|')
        .filter(|s| !s.is_empty())
        .map(compile_anchored)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_literal_when_no_metacharacters() {
        assert_eq!(classify("ERROR:"), PatternKind::Literal);
        assert_eq!(classify("knn_entries"), PatternKind::Literal);
    }

    #[test]
    fn classifies_glob_when_not_valid_regex() {
        assert_eq!(classify("*.java"), PatternKind::Glob);
        assert_eq!(classify("data_[0-9]"), PatternKind::Glob);
    }

    #[test]
    fn classifies_regex_when_explicit_and_valid() {
        assert_eq!(classify("^ERROR"), PatternKind::Regex);
        assert_eq!(classify("foo|bar"), PatternKind::Regex);
        assert_eq!(classify(r"\d+"), PatternKind::Regex);
        // A bare '.' is a regex specifier, so this is a regex, not a literal.
        assert_eq!(classify("foo.bar"), PatternKind::Regex);
    }

    #[test]
    fn literal_matches_as_unanchored_substring() {
        let re = compile("ERROR:").unwrap();
        assert!(re.is_match("first line\nERROR: bad input\n"));
        assert!(!re.is_match("all good here"));
    }

    #[test]
    fn name_set_anchors_each_glob_alternative() {
        let set = compile_name_set("*.java|*.kt").unwrap();
        let matches = |name: &str| set.iter().any(|r| r.is_match(name));
        assert!(matches("Widget.java"));
        assert!(matches("Widget.kt"));
        assert!(!matches("Widget.javax"));
        assert!(!matches("Widget.java.bak"));
    }

    #[test]
    fn regex_alternation_is_preserved_for_content() {
        let re = compile("SimpleMFD|knn_entries").unwrap();
        assert!(re.is_match("...knn_entries..."));
        assert!(!re.is_match("nothing relevant"));
    }
}
