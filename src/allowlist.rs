// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The command allow-gates behind the dispatching tools.
//!
//! `ct-test` and `ct-each` can run another program, so each runs **only**
//! commands on a fixed, compiled-in list. The lists are intentionally **static
//! and immutable**: nothing a caller does at run time can extend them, so an
//! agent driving these tools cannot grant itself new commands. A command that
//! is not on the relevant list is refused, and nothing runs. There is no shell
//! mode anywhere in the suite — every dispatch is a direct argv launch.
//!
//! * `ct-test` gates on [`BUILTIN`]: read-only commands only.
//! * `ct-each` gates through [`is_allowed_for_each`]: [`BUILTIN`] plus
//!   `ct-test` (itself gated, so still read-only), and — only behind an
//!   explicit `--mutating` flag — the suite's own [`MUTATING_SUITE`] tools,
//!   which carry their own `--expect`/`--dry-run` safety gates.
//!
//! Gating is by **program name** (the file-name component of the command). It
//! is a guard against unintended side effects, not a sandbox: it does not
//! inspect arguments or resolve which binary a name ultimately runs.

use std::path::Path;

/// Commands trusted as read-only — `ct-test`'s entire, fixed allowlist.
///
/// Deliberately small and conservative: names whose ordinary use has no side
/// effects. (`find` is excluded: `-delete`/`-exec` make it not read-only; the
/// umbrella `ct` and the dispatching/mutating `ct-test`/`ct-each`/`ct-edit`/
/// `ct-patch`/`ct-rules`/`ct-await` are excluded because they can change
/// state or dispatch — the read-only `ct-search`, `ct-outline`, `ct-tree`,
/// `ct-view`, `ct-check`, and `ct-deps` (whose `cargo metadata` source is
/// forced `--locked --offline`) are included.) There is no run-time
/// mechanism to add to this list.
pub const BUILTIN: &[&str] = &[
    "cat",
    "ct-check",
    "ct-deps",
    "ct-outline",
    "ct-search",
    "ct-tree",
    "ct-view",
    "echo",
    "false",
    "file",
    "grep",
    "head",
    "ls",
    "pwd",
    "stat",
    "tail",
    "true",
    "wc",
];

/// The suite's mutating tools, runnable by `ct-each` only behind its explicit
/// `--mutating` flag. Each carries its own `--expect`/`--dry-run` gates, so a
/// dispatched edit still has to assert its own effect before writing.
pub const MUTATING_SUITE: &[&str] = &["ct-edit", "ct-patch"];

/// The program name the gates check for a command: its file-name component,
/// so `ls`, `/bin/ls`, and `./ls` all gate on `ls`.
///
/// # Examples
///
/// ```
/// use coding_tools::allowlist::gated_name;
///
/// assert_eq!(gated_name("/bin/ls"), "ls");
/// assert_eq!(gated_name("./parse"), "parse");
/// ```
pub fn gated_name(cmd: &str) -> String {
    Path::new(cmd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| cmd.to_string())
}

/// Whether `name` is on `ct-test`'s fixed read-only allowlist.
///
/// # Examples
///
/// ```
/// use coding_tools::allowlist::is_allowed;
///
/// assert!(is_allowed("grep"));       // a built-in read-only command
/// assert!(is_allowed("ct-search"));  // the suite's own read-only tools
/// assert!(!is_allowed("rm"));        // not read-only, never runnable
/// assert!(!is_allowed("sh"));        // no shell, ever
/// ```
pub fn is_allowed(name: &str) -> bool {
    BUILTIN.contains(&name)
}

/// Whether `name` is a permitted `ct-each` dispatch target.
///
/// The base set is [`BUILTIN`] plus `ct-test` (which only runs read-only
/// commands itself, so dispatching it stays read-only). With `mutating`, the
/// suite's [`MUTATING_SUITE`] tools are also permitted — and nothing else:
/// arbitrary mutating commands are never runnable.
///
/// # Examples
///
/// ```
/// use coding_tools::allowlist::is_allowed_for_each;
///
/// assert!(is_allowed_for_each("ct-view", false));
/// assert!(is_allowed_for_each("ct-test", false));  // itself gated read-only
/// assert!(!is_allowed_for_each("ct-edit", false)); // needs --mutating
/// assert!(is_allowed_for_each("ct-edit", true));
/// assert!(!is_allowed_for_each("rm", true));       // never, even with --mutating
/// assert!(!is_allowed_for_each("sh", true));       // no shell, ever
/// ```
pub fn is_allowed_for_each(name: &str, mutating: bool) -> bool {
    is_allowed(name) || name == "ct-test" || (mutating && MUTATING_SUITE.contains(&name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gated_name_uses_basename() {
        assert_eq!(gated_name("ls"), "ls");
        assert_eq!(gated_name("/bin/ls"), "ls");
        assert_eq!(gated_name("./parse"), "parse");
    }

    #[test]
    fn builtins_allowed_everything_else_refused() {
        assert!(is_allowed("grep"));
        assert!(is_allowed("ct-search"));
        // Not read-only, never runnable, and unextendable at run time.
        assert!(!is_allowed("parse"));
        assert!(!is_allowed("sh"));
        assert!(!is_allowed("ct-edit"));
        assert!(!is_allowed("ct-each"));
    }

    #[test]
    fn each_gate_extends_only_to_suite_tools() {
        assert!(is_allowed_for_each("grep", false));
        assert!(is_allowed_for_each("ct-test", false));
        assert!(!is_allowed_for_each("ct-each", false)); // no self-nesting
        assert!(!is_allowed_for_each("ct-each", true));
        assert!(!is_allowed_for_each("ct-edit", false));
        assert!(is_allowed_for_each("ct-patch", true));
        assert!(!is_allowed_for_each("mvn", true)); // external commands never
    }
}
