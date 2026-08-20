// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! "Did you mean" hints for a mistyped long flag.
//!
//! clap's own suggestion is edit-distance based, so it catches typos
//! (`--quite` -> `--quiet`) but never *synonyms*: an agent that reaches for
//! `--content` on `ct-search`, or `--base` on `ct-view`, gets a correct-but-
//! unhelpful "unexpected argument". Both misses are guessable — `--content`
//! and `--grep` mean the same thing, and `ct-view`'s target is a positional.
//!
//! [`hint`] maps an unknown flag to the flag the tool actually has, resolved
//! **against that tool's live grammar** rather than a per-tool table: a synonym
//! is only suggested when the target flag really exists on the command being
//! run, so the table cannot drift into naming a flag that was renamed away.
//! When no flag fits but the tool takes the thing positionally (`ct-view`,
//! `ct-outline`, `ct-survey`), the hint says so instead.

/// Candidate canonical flags for each known synonym, most likely first. A
/// candidate is only ever *offered* when the tool being run actually accepts
/// it, so one table serves every tool.
const SYNONYMS: &[(&str, &[&str])] = &[
    // "what to look for"
    ("content", &["grep", "find", "match"]),
    ("contents", &["grep", "find", "match"]),
    ("pattern", &["grep", "match", "find", "name"]),
    ("regex", &["grep", "match", "find"]),
    ("regexp", &["grep", "match", "find"]),
    ("search", &["grep", "match", "find"]),
    ("text", &["grep", "match", "find"]),
    ("query", &["grep", "match", "find"]),
    ("needle", &["grep", "match", "find"]),
    // "where to look" — see `TARGET_ALIASES` for the positional fallback
    ("base", &["base"]),
    ("path", &["base"]),
    ("dir", &["base"]),
    ("directory", &["base"]),
    ("root", &["base"]),
    ("file", &["base"]),
    ("target", &["base"]),
    // "which files"
    ("glob", &["name"]),
    ("include", &["name"]),
    ("filename", &["name"]),
    // edits
    ("replacement", &["replace"]),
    ("replace-with", &["replace"]),
    ("to", &["replace"]),
    ("new", &["replace"]),
    ("from", &["find"]),
    ("old", &["find"]),
    // verdicts and bounds
    ("expected", &["expect"]),
    ("assert", &["expect"]),
    ("preview", &["dry-run"]),
    ("no-write", &["dry-run"]),
    ("silent", &["quiet"]),
    ("deadline", &["timeout"]),
    ("max-time", &["timeout"]),
    ("interval", &["every"]),
    ("poll", &["every"]),
    ("period", &["every"]),
    // dispatch
    ("command", &["cmd"]),
    ("exec", &["cmd"]),
    ("run", &["cmd"]),
    ("program", &["cmd"]),
    // views
    ("lines", &["range", "context"]),
    ("before", &["context"]),
    ("after", &["context"]),
    ("max-count", &["limit"]),
];

/// Synonyms that name *where to look*. When none of their candidate flags
/// exists on the tool, the tool takes its target positionally, and the hint
/// says that instead of going silent.
const TARGET_ALIASES: &[&str] = &["base", "path", "dir", "directory", "root", "file", "target"];

/// Strip an unknown-argument token down to its long-flag name: `--content=x`
/// and `--content` both reduce to `content`. Returns `None` for anything that
/// is not a long flag (a short flag, a stray positional).
fn long_name(token: &str) -> Option<&str> {
    let rest = token.strip_prefix("--")?;
    let name = rest.split('=').next().unwrap_or(rest);
    (!name.is_empty()).then_some(name)
}

/// The one-line "did you mean" for an unknown flag on `command`, or `None`
/// when nothing better than clap's own message can be offered.
///
/// # Examples
///
/// ```
/// use clap::{Arg, Command};
/// use coding_tools::flaghint::hint;
///
/// let search = Command::new("ct-search").arg(Arg::new("grep").long("grep"));
/// assert_eq!(
///     hint(&search, "--content").as_deref(),
///     Some("did you mean --grep? (ct-search names content matching --grep)"),
/// );
///
/// // A synonym whose target the tool does not have is not offered.
/// let tree = Command::new("ct-tree").arg(Arg::new("base").long("base"));
/// assert_eq!(hint(&tree, "--content"), None);
/// ```
pub fn hint(command: &clap::Command, unknown: &str) -> Option<String> {
    let name = long_name(unknown)?;
    let tool = command.get_name().to_string();
    let has_flag = |f: &str| command.get_arguments().any(|a| a.get_long() == Some(f));

    let candidates = SYNONYMS
        .iter()
        .find(|(alias, _)| *alias == name)
        .map(|(_, c)| *c)?;

    if let Some(best) = candidates.iter().copied().find(|c| has_flag(c)) {
        // Only reachable when the alias and the target differ: a tool that
        // really has `--base` never gets "did you mean --base?".
        if best == name {
            return None;
        }
        return Some(format!(
            "did you mean --{best}? ({tool} names {} --{best})",
            role(best)
        ));
    }

    // No matching flag. If the alias named the target and the tool takes it
    // positionally, say so — the `--base` on `ct-view` case.
    if TARGET_ALIASES.contains(&name)
        && let Some(pos) = command
            .get_positionals()
            .find(|a| matches!(a.get_id().as_str(), "path" | "base"))
    {
        let placeholder = pos
            .get_value_names()
            .and_then(|v| v.first())
            .map(|v| v.to_string())
            .unwrap_or_else(|| pos.get_id().as_str().to_ascii_uppercase());
        return Some(format!(
            "{tool} takes its target as a positional argument, not --{name}: \
             `{tool} <{placeholder}> ...`"
        ));
    }
    None
}

/// The human role a canonical flag plays, for the parenthetical in [`hint`].
fn role(flag: &str) -> &'static str {
    match flag {
        "grep" => "content matching",
        "match" => "the pattern to show",
        "find" => "the pattern to replace",
        "replace" => "the replacement text",
        "name" => "the file-name filter",
        "base" => "the search root",
        "expect" => "the verdict expectation",
        "dry-run" => "preview-without-writing",
        "quiet" => "output suppression",
        "timeout" => "the time bound",
        "every" => "the poll interval",
        "cmd" => "the program to run",
        "range" => "the line range",
        "context" => "context lines",
        "limit" => "the output cap",
        _ => "that",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Arg, Command};

    fn cmd(name: &'static str, longs: &[&'static str]) -> Command {
        longs
            .iter()
            .fold(Command::new(name), |c, l| c.arg(Arg::new(*l).long(*l)))
    }

    #[test]
    fn reported_synonyms_resolve_to_the_real_flag() {
        let search = cmd("ct-search", &["grep", "base", "name"]);
        assert!(hint(&search, "--content").unwrap().contains("--grep"));
        assert!(hint(&search, "--content=foo").unwrap().contains("--grep"));
        let edit = cmd("ct-edit", &["find", "replace", "base"]);
        assert!(hint(&edit, "--pattern").unwrap().contains("--find"));
        assert!(hint(&edit, "--replacement").unwrap().contains("--replace"));
    }

    #[test]
    fn a_positional_target_is_named_instead_of_a_flag() {
        let view = Command::new("ct-view")
            .arg(Arg::new("path").value_name("PATH"))
            .arg(Arg::new("match").long("match"));
        let h = hint(&view, "--base").unwrap();
        assert!(h.contains("positional"), "{h}");
        assert!(h.contains("<PATH>"), "{h}");
    }

    #[test]
    fn nothing_is_invented_when_the_tool_lacks_the_target() {
        // ct-tree has no content matcher and no positional target.
        let tree = cmd("ct-tree", &["base", "name"]);
        assert_eq!(hint(&tree, "--content"), None);
        // Unknown-to-the-table flags fall through to clap's own message.
        assert_eq!(hint(&tree, "--frobnicate"), None);
        // Not a long flag at all.
        assert_eq!(hint(&tree, "-x"), None);
    }

    #[test]
    fn a_flag_the_tool_already_has_is_never_suggested_to_itself() {
        let search = cmd("ct-search", &["base"]);
        assert_eq!(hint(&search, "--base"), None);
    }
}
