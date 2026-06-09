// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-test`'s command allow-gate.
//!
//! `ct-test` can run an arbitrary program, so it runs **only** commands on a
//! fixed, compiled-in list of read-only commands ([`BUILTIN`]). The list is
//! intentionally **static and immutable**: nothing a caller does at run time can
//! extend it, so an agent driving `ct-test` cannot grant itself new commands. A
//! command that is not on the list is refused, and nothing runs.
//!
//! Gating is by **program name** (the file-name component of `--cmd`, or `sh`
//! under `--shell`, since a shell line can run anything). It is a guard against
//! unintended side effects, not a sandbox: it does not inspect arguments or
//! resolve which binary a name ultimately runs.

use std::path::Path;

/// Commands trusted as read-only — the entire, fixed allowlist.
///
/// Deliberately small and conservative: names whose ordinary use has no side
/// effects. (`find` is excluded: `-delete`/`-exec` make it not read-only; the
/// umbrella `ct` and the mutating `ct-test`/`ct-edit`/`ct-patch` are excluded
/// because they can change state — the read-only `ct-search`, `ct-tree`, and
/// `ct-view` are included.) There is no run-time mechanism to add to this list.
pub const BUILTIN: &[&str] = &[
    "cat",
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

/// The program name the gate checks for a given `--cmd` / `--shell` pairing.
///
/// Under `--shell` the program is always `sh` (the shell line itself is opaque);
/// otherwise it is the file-name component of `cmd`, so `ls`, `/bin/ls`, and
/// `./ls` all gate on `ls`.
///
/// # Examples
///
/// ```
/// use coding_tools::allowlist::gated_name;
///
/// assert_eq!(gated_name("/bin/ls", false), "ls");
/// assert_eq!(gated_name("./parse", false), "parse");
/// assert_eq!(gated_name("grep x | wc -l", true), "sh"); // shell line gates on sh
/// ```
pub fn gated_name(cmd: &str, shell: bool) -> String {
    if shell {
        return "sh".to_string();
    }
    Path::new(cmd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| cmd.to_string())
}

/// Whether `name` is on the fixed allowlist.
///
/// # Examples
///
/// ```
/// use coding_tools::allowlist::is_allowed;
///
/// assert!(is_allowed("grep"));       // a built-in read-only command
/// assert!(is_allowed("ct-search"));  // the suite's own read-only tools
/// assert!(!is_allowed("rm"));        // not read-only, never runnable
/// assert!(!is_allowed("sh"));        // shell is excluded, so --shell is gated off
/// ```
pub fn is_allowed(name: &str) -> bool {
    BUILTIN.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gated_name_uses_basename_or_sh() {
        assert_eq!(gated_name("ls", false), "ls");
        assert_eq!(gated_name("/bin/ls", false), "ls");
        assert_eq!(gated_name("./parse", false), "parse");
        assert_eq!(gated_name("anything --here", true), "sh");
    }

    #[test]
    fn builtins_allowed_everything_else_refused() {
        assert!(is_allowed("grep"));
        assert!(is_allowed("ct-search"));
        // Not read-only, never runnable, and unextendable at run time.
        assert!(!is_allowed("parse"));
        assert!(!is_allowed("sh"));
        assert!(!is_allowed("ct-edit"));
    }
}
