// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The shared *framed-verdict* spine: the `SUCCESS`/`ERROR` outcome every tool
//! emits, its `0`/`1` exit-status mapping, and the [`Expect`]ation that turns a
//! search's match count into a [`Verdict`].
//!
//! Both binaries reduce to the same shape — frame a question, run a probe,
//! classify the result, emit a templated verdict — and this module carries the
//! pieces of that shape that are not specific to *what* the probe is. `ct-test`
//! classifies a command's streams into a [`Verdict`]; `ct-search` classifies its
//! match count through an [`Expect`]ation into the same [`Verdict`]; both map it
//! to an exit status the same way.

use std::process::ExitCode;

/// The outcome of a framed check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The check passed.
    Success,
    /// The check failed.
    Error,
}

impl Verdict {
    /// The token written for `{RESULT}` and shown in human output.
    ///
    /// # Examples
    ///
    /// ```
    /// use coding_tools::verdict::Verdict;
    ///
    /// assert_eq!(Verdict::Success.label(), "SUCCESS");
    /// assert_eq!(Verdict::Error.label(), "ERROR");
    /// ```
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Success => "SUCCESS",
            Verdict::Error => "ERROR",
        }
    }

    /// The process exit status carrying this verdict: `0` for [`Success`], `1`
    /// for [`Error`]. A `2` (usage/runtime failure) is a separate concern owned
    /// by each binary's `main`, never produced here.
    ///
    /// [`Success`]: Verdict::Success
    /// [`Error`]: Verdict::Error
    pub fn exit_code(self) -> ExitCode {
        match self {
            Verdict::Success => ExitCode::SUCCESS,
            Verdict::Error => ExitCode::from(1),
        }
    }
}

/// An expectation over a match count, classifying it into a [`Verdict`].
///
/// The numeric forms reuse the suite's `[+|-]N` threshold grammar (the same
/// `+` larger-than / `-` smaller-than / bare at-least convention as
/// `ct-search --size`), extended with an exact form and two keywords so the
/// common search-as-test assertions read plainly:
///
/// | Spec   | Passes when the count is | Meaning                          |
/// | ------ | ------------------------ | -------------------------------- |
/// | `any`  | `>= 1`                   | found something *(the default)*  |
/// | `none` | `== 0`                   | a negative assertion             |
/// | `N`    | `>= N`                   | at least `N`                     |
/// | `=N`   | `== N`                   | exactly `N`                      |
/// | `+N`   | `> N`                    | more than `N`                    |
/// | `-N`   | `< N`                    | fewer than `N`                   |
///
/// `any` is the default so a plain search gains framing without changing its
/// pass condition: `Expect::default().eval(count)` is `Success` exactly when the
/// search matched, reproducing `ct-search`'s historic `0`/`1` exit semantics.
///
/// # Examples
///
/// ```
/// use coding_tools::verdict::{Expect, Verdict};
///
/// // `none` is a negative assertion: passes only when nothing matched.
/// assert_eq!(Expect::parse("none").unwrap().eval(0), Verdict::Success);
/// assert_eq!(Expect::parse("none").unwrap().eval(2), Verdict::Error);
///
/// // The default `any` passes on one or more.
/// assert_eq!(Expect::default().eval(0), Verdict::Error);
/// assert_eq!(Expect::default().eval(3), Verdict::Success);
///
/// // Thresholds: bare N is ">= N", =N exact, +N more-than, -N fewer-than.
/// assert_eq!(Expect::parse("3").unwrap().eval(3), Verdict::Success);
/// assert_eq!(Expect::parse("=2").unwrap().eval(3), Verdict::Error);
/// assert_eq!(Expect::parse("+0").unwrap().eval(1), Verdict::Success);
/// assert_eq!(Expect::parse("-10").unwrap().eval(9), Verdict::Success);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// `>= N` — the bare-`N` and `any` (`>= 1`) forms.
    AtLeast(u64),
    /// `== N` — the `=N` and `none` (`== 0`) forms.
    Eq(u64),
    /// `> N` — the `+N` form.
    Gt(u64),
    /// `< N` — the `-N` form.
    Lt(u64),
}

impl Default for Expect {
    /// `any` — pass when at least one entry matched.
    fn default() -> Self {
        Expect::AtLeast(1)
    }
}

impl Expect {
    /// Parse an expectation spec; see the [type docs](Expect) for the grammar.
    pub fn parse(spec: &str) -> Result<Expect, String> {
        let spec = spec.trim();
        match spec {
            "any" => return Ok(Expect::AtLeast(1)),
            "none" => return Ok(Expect::Eq(0)),
            "" => return Err("empty --expect spec".to_string()),
            _ => {}
        }
        let (ctor, body): (fn(u64) -> Expect, &str) = if let Some(r) = spec.strip_prefix('=') {
            (Expect::Eq, r)
        } else if let Some(r) = spec.strip_prefix('+') {
            (Expect::Gt, r)
        } else if let Some(r) = spec.strip_prefix('-') {
            (Expect::Lt, r)
        } else {
            (Expect::AtLeast, spec)
        };
        let n: u64 = body
            .trim()
            .parse()
            .map_err(|_| format!("invalid count in --expect '{spec}'"))?;
        Ok(ctor(n))
    }

    /// Classify a match `count` into a [`Verdict`].
    pub fn eval(self, count: u64) -> Verdict {
        let pass = match self {
            Expect::AtLeast(n) => count >= n,
            Expect::Eq(n) => count == n,
            Expect::Gt(n) => count > n,
            Expect::Lt(n) => count < n,
        };
        if pass {
            Verdict::Success
        } else {
            Verdict::Error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_expectation_is_any() {
        assert_eq!(Expect::default(), Expect::AtLeast(1));
        assert_eq!(Expect::default().eval(0), Verdict::Error);
        assert_eq!(Expect::default().eval(3), Verdict::Success);
    }

    #[test]
    fn keywords_parse_to_numeric_forms() {
        assert_eq!(Expect::parse("any").unwrap(), Expect::AtLeast(1));
        assert_eq!(Expect::parse("none").unwrap(), Expect::Eq(0));
    }

    #[test]
    fn threshold_grammar_matches_size_conventions() {
        // bare N => at least N; +N => more than N; -N => fewer than N.
        assert_eq!(Expect::parse("3").unwrap(), Expect::AtLeast(3));
        assert_eq!(Expect::parse("+3").unwrap(), Expect::Gt(3));
        assert_eq!(Expect::parse("-3").unwrap(), Expect::Lt(3));
        assert_eq!(Expect::parse("=3").unwrap(), Expect::Eq(3));
    }

    #[test]
    fn none_passes_only_on_zero() {
        let none = Expect::parse("none").unwrap();
        assert_eq!(none.eval(0), Verdict::Success);
        assert_eq!(none.eval(1), Verdict::Error);
    }

    #[test]
    fn thresholds_classify_counts() {
        assert_eq!(Expect::Gt(0).eval(0), Verdict::Error);
        assert_eq!(Expect::Gt(0).eval(1), Verdict::Success);
        assert_eq!(Expect::Lt(10).eval(9), Verdict::Success);
        assert_eq!(Expect::Lt(10).eval(10), Verdict::Error);
        assert_eq!(Expect::Eq(1).eval(1), Verdict::Success);
        assert_eq!(Expect::Eq(1).eval(2), Verdict::Error);
    }

    #[test]
    fn rejects_non_numeric_specs() {
        assert!(Expect::parse("lots").is_err());
        assert!(Expect::parse("+").is_err());
        assert!(Expect::parse("").is_err());
    }
}
