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
//! The allowlist is **platform-aware**, so the tools are usable both on Unix /
//! MSYS2 and on native Windows (no MSYS2 required): [`CORE`] is the suite's own
//! read-only `ct-*` tools, present on every OS, and [`NATIVE`] adds the host
//! OS's stock read-only utilities (coreutils on Unix; `findstr`/`where`/… on
//! Windows). [`builtin`] is their union for the current platform. This changes
//! *which names resolve per OS*, not the no-shell, direct-argv guarantee.
//!
//! * `ct-test` gates on [`builtin`]: read-only commands only.
//! * `ct-each` gates through [`is_allowed_for_each`]: [`builtin`] plus
//!   `ct-test` (itself gated, so still read-only), and — only behind an
//!   explicit `--mutating` flag — the suite's own [`MUTATING_SUITE`] tools,
//!   which carry their own `--expect`/`--dry-run` safety gates.
//!
//! Gating is by **program name** (the file-name component of the command, with
//! a Windows executable suffix like `.exe` stripped). It is a guard against
//! unintended side effects, not a sandbox: it does not inspect arguments or
//! resolve which binary a name ultimately runs.

use std::path::Path;

/// The suite's own read-only tools — the cross-platform core of the allowlist,
/// present and resolvable on every OS.
///
/// The mutating/dispatching tools (`ct-edit`/`ct-patch`/`ct-rules`/`ct-test`/
/// `ct-each`) and the umbrella `ct` are excluded because they change state or
/// dispatch; `ct-await` is included as a read-only **observer** (it only polls
/// other read-only probes), which also lets it serve as a portable, bounded
/// long-running command. The crate-/module-graph checks (`deps`/`mods`) are not
/// dispatch targets — they are built-in checks the rule layer runs in-process.
pub const CORE: &[&str] = &[
    "ct-await",
    "ct-check",
    "ct-outline",
    "ct-search",
    "ct-tree",
    "ct-view",
];

/// The host OS's stock read-only utilities, added to [`CORE`]. Deliberately
/// small and conservative: names whose ordinary use has no side effects.
/// (`find` is excluded: `-delete`/`-exec` make it not read-only.) There is no
/// run-time mechanism to add to this list.
#[cfg(unix)]
pub const NATIVE: &[&str] = &[
    "cat", "echo", "false", "file", "grep", "head", "ls", "pwd", "stat", "tail", "true", "wc",
];
/// Stock read-only programs that exist on a bare Windows install (real `.exe`s,
/// launched directly — still no shell). `findstr` covers grep- and cat-style
/// needs (it can read a file or stdin); `more`/`where`/`whoami`/`hostname` round
/// out the read-only set.
#[cfg(windows)]
pub const NATIVE: &[&str] = &["findstr", "hostname", "more", "where", "whoami"];
#[cfg(not(any(unix, windows)))]
pub const NATIVE: &[&str] = &[];

/// `ct-test`'s entire read-only allowlist for the current platform: the
/// cross-platform [`CORE`] plus the OS's [`NATIVE`] utilities. Returned as an
/// owned list so callers can `join`/iterate it in messages.
pub fn builtin() -> Vec<&'static str> {
    CORE.iter().chain(NATIVE).copied().collect()
}

/// The suite's mutating tools, runnable by `ct-each` only behind its explicit
/// `--mutating` flag. Each carries its own `--expect`/`--dry-run` gates, so a
/// dispatched edit still has to assert its own effect before writing.
pub const MUTATING_SUITE: &[&str] = &["ct-edit", "ct-patch"];

/// The program name the gates check for a command: its file-name component,
/// so `ls`, `/bin/ls`, and `./ls` all gate on `ls`. On Windows a trailing
/// executable suffix (`.exe`/`.com`/`.bat`/`.cmd`, case-insensitive) is
/// stripped, so an absolute or sibling path like `...\ct-search.exe` gates as
/// `ct-search`.
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
    let base = Path::new(cmd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| cmd.to_string());
    strip_exe_suffix(&base)
}

/// Strip a Windows executable suffix from a program's file name. A no-op on
/// non-Windows, where a file may legitimately be named e.g. `foo.exe`.
#[cfg(windows)]
fn strip_exe_suffix(name: &str) -> String {
    const EXTS: &[&str] = &[".exe", ".com", ".bat", ".cmd"];
    let lower = name.to_ascii_lowercase();
    for ext in EXTS {
        if lower.ends_with(ext) {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    name.to_string()
}
#[cfg(not(windows))]
fn strip_exe_suffix(name: &str) -> String {
    name.to_string()
}

/// Whether `name` is on `ct-test`'s fixed read-only allowlist for the current
/// platform ([`CORE`] plus the OS's [`NATIVE`] utilities).
///
/// # Examples
///
/// ```
/// use coding_tools::allowlist::is_allowed;
///
/// assert!(is_allowed("ct-search"));  // a suite read-only tool, on every platform
/// assert!(!is_allowed("rm"));        // not read-only, never runnable
/// assert!(!is_allowed("sh"));        // no shell, ever
/// ```
pub fn is_allowed(name: &str) -> bool {
    CORE.contains(&name) || NATIVE.contains(&name)
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
    #[cfg(windows)]
    fn gated_name_strips_windows_exe_suffix() {
        assert_eq!(gated_name("C:\\tools\\ct-search.exe"), "ct-search");
        assert_eq!(gated_name("findstr.exe"), "findstr");
        assert_eq!(gated_name("Foo.CMD"), "Foo");
        assert_eq!(gated_name("noext"), "noext");
    }

    #[test]
    fn builtins_allowed_everything_else_refused() {
        // The cross-platform core is allowed everywhere.
        assert!(is_allowed("ct-search"));
        assert!(is_allowed("ct-await"));
        // A native utility for this platform.
        #[cfg(unix)]
        assert!(is_allowed("grep"));
        #[cfg(windows)]
        assert!(is_allowed("findstr"));
        // Not read-only, never runnable, and unextendable at run time.
        assert!(!is_allowed("parse"));
        assert!(!is_allowed("sh"));
        assert!(!is_allowed("ct-edit"));
        assert!(!is_allowed("ct-each"));
    }

    #[test]
    fn each_gate_extends_only_to_suite_tools() {
        assert!(is_allowed_for_each("ct-search", false));
        assert!(is_allowed_for_each("ct-test", false));
        assert!(!is_allowed_for_each("ct-each", false)); // no self-nesting
        assert!(!is_allowed_for_each("ct-each", true));
        assert!(!is_allowed_for_each("ct-edit", false));
        assert!(is_allowed_for_each("ct-patch", true));
        assert!(!is_allowed_for_each("mvn", true)); // external commands never
    }

    #[test]
    fn builtin_unions_core_and_native() {
        let b = builtin();
        assert!(b.contains(&"ct-search")); // core
        assert!(b.iter().any(|n| NATIVE.contains(n))); // at least one native
        assert!(!b.contains(&"rm"));
    }
}
