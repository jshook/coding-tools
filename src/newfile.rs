// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-new`'s file-creation planning: what an existing target means under the
//! chosen collision policy, how a payload becomes the exact bytes written, and
//! the `.ctb` batch vocabulary behind `ct-new --script`.
//!
//! Nothing here touches the filesystem — the caller reports whether a target
//! exists and performs the writes — so every decision the tool makes is a pure
//! function, testable (and doctested) on its own. That split is what lets a
//! batch run under the suite's prepare/confirm/write standard: the whole plan
//! is decided in memory, and a single blocked target means zero writes.

use std::path::{Path, PathBuf};

use crate::blockdoc::Item;

/// What `ct-new` does when the target file already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OnExists {
    /// Refuse: the target is left exactly as it was and the run reports ERROR.
    /// The default, so a create can never silently clobber existing work.
    #[default]
    Fail,
    /// Accept the existing file as-is and write nothing — an idempotent
    /// "make sure this file is there".
    Skip,
    /// Replace the file's contents.
    Overwrite,
}

impl OnExists {
    /// Parse a policy from its `.ctb` attribute spelling.
    ///
    /// # Examples
    ///
    /// ```
    /// use coding_tools::newfile::OnExists;
    ///
    /// assert_eq!(OnExists::parse("skip").unwrap(), OnExists::Skip);
    /// assert!(OnExists::parse("clobber").is_err());
    /// ```
    pub fn parse(s: &str) -> Result<OnExists, String> {
        match s {
            "fail" => Ok(OnExists::Fail),
            "skip" => Ok(OnExists::Skip),
            "overwrite" => Ok(OnExists::Overwrite),
            other => Err(format!("invalid on-exists '{other}' (fail|skip|overwrite)")),
        }
    }
}

/// What one target's fate is, once its existence has been checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The file does not exist and will be written.
    Created,
    /// The file exists and will be replaced (`--on-exists overwrite`).
    Overwritten,
    /// The file exists and is accepted as-is (`--on-exists skip`).
    Skipped,
    /// The file exists and the policy refuses it (`--on-exists fail`).
    Blocked,
}

impl Action {
    /// The word used for this action in text and JSON output.
    pub fn label(self) -> &'static str {
        match self {
            Action::Created => "created",
            Action::Overwritten => "overwritten",
            Action::Skipped => "skipped",
            Action::Blocked => "blocked",
        }
    }

    /// Whether this action writes bytes to disk.
    pub fn writes(self) -> bool {
        matches!(self, Action::Created | Action::Overwritten)
    }

    /// Whether the target ends up in the state the caller asked for — the
    /// quantity `--expect` frames. A skipped file counts: it is already there.
    pub fn ensured(self) -> bool {
        !matches!(self, Action::Blocked)
    }
}

/// Decide one target's [`Action`] from whether it exists and the policy.
///
/// # Examples
///
/// ```
/// use coding_tools::newfile::{Action, OnExists, action_for};
///
/// // A fresh path is always a create, whatever the policy.
/// assert_eq!(action_for(false, OnExists::Fail), Action::Created);
///
/// // An existing one is refused by default, and only ever replaced on request.
/// assert_eq!(action_for(true, OnExists::Fail), Action::Blocked);
/// assert_eq!(action_for(true, OnExists::Skip), Action::Skipped);
/// assert_eq!(action_for(true, OnExists::Overwrite), Action::Overwritten);
/// ```
pub fn action_for(exists: bool, on_exists: OnExists) -> Action {
    match (exists, on_exists) {
        (false, _) => Action::Created,
        (true, OnExists::Fail) => Action::Blocked,
        (true, OnExists::Skip) => Action::Skipped,
        (true, OnExists::Overwrite) => Action::Overwritten,
    }
}

/// The exact bytes a body becomes: a non-empty body gets the final newline a
/// text file is supposed to end with, unless the caller opted out.
///
/// # Examples
///
/// ```
/// use coding_tools::newfile::finalize;
///
/// // The common case: an argv payload with no terminator gains one.
/// assert_eq!(finalize("fn main() {}", true), "fn main() {}\n");
/// // Already terminated (a heredoc, a file: payload): unchanged, never doubled.
/// assert_eq!(finalize("a\nb\n", true), "a\nb\n");
/// // An empty body is an empty file, not a lone newline.
/// assert_eq!(finalize("", true), "");
/// // Opted out: the bytes are exactly what was given.
/// assert_eq!(finalize("no terminator", false), "no terminator");
/// ```
pub fn finalize(content: &str, final_newline: bool) -> String {
    if !final_newline || content.is_empty() || content.ends_with('\n') {
        content.to_string()
    } else {
        format!("{content}\n")
    }
}

/// A bounded look at a body: at most `max` lines, plus the number withheld.
/// The preview is what `--dry-run` shows, so a large new file cannot flood the
/// caller's context.
///
/// # Examples
///
/// ```
/// use coding_tools::newfile::preview;
///
/// let (shown, more) = preview("a\nb\nc\n", 2);
/// assert_eq!(shown, ["a", "b"]);
/// assert_eq!(more, 1);
///
/// // Nothing withheld when it fits.
/// assert_eq!(preview("a\n", 10), (vec!["a"], 0));
/// ```
pub fn preview(content: &str, max: usize) -> (Vec<&str>, usize) {
    let lines: Vec<&str> = content.lines().collect();
    let shown = lines.len().min(max);
    (lines[..shown].to_vec(), lines.len() - shown)
}

/// One planned file: where it goes, the exact bytes, and what will happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub path: PathBuf,
    pub content: String,
    pub action: Action,
    /// 1-based `.ctb` line of the item that produced this plan; `0` for the
    /// single-file argv form.
    pub line: usize,
}

impl Plan {
    /// Bytes the file will hold.
    pub fn bytes(&self) -> usize {
        self.content.len()
    }

    /// Lines the file will hold.
    pub fn lines(&self) -> usize {
        self.content.lines().count()
    }
}

/// The per-action totals a run reports.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub created: usize,
    pub overwritten: usize,
    pub skipped: usize,
    pub blocked: usize,
}

impl Tally {
    /// Count a plan set.
    ///
    /// # Examples
    ///
    /// ```
    /// use coding_tools::newfile::{Action, Plan, Tally};
    ///
    /// let plan = |action| Plan { path: "f".into(), content: String::new(), action, line: 0 };
    /// let tally = Tally::of(&[plan(Action::Created), plan(Action::Skipped), plan(Action::Blocked)]);
    ///
    /// // Ensured = the targets that end up as asked; a blocked one never counts.
    /// assert_eq!(tally.ensured(), 2);
    /// assert_eq!(tally.blocked, 1);
    /// ```
    pub fn of(plans: &[Plan]) -> Tally {
        let mut t = Tally::default();
        for p in plans {
            match p.action {
                Action::Created => t.created += 1,
                Action::Overwritten => t.overwritten += 1,
                Action::Skipped => t.skipped += 1,
                Action::Blocked => t.blocked += 1,
            }
        }
        t
    }

    /// Targets that end up in the requested state — the count `--expect` frames.
    pub fn ensured(&self) -> usize {
        self.created + self.overwritten + self.skipped
    }
}

// ----- The `.ctb` batch vocabulary ---------------------------------------------

/// Attributes a `new` item accepts.
const NEW_ATTRS: [&str; 2] = ["file", "on-exists"];
/// Payload sections a `new` item accepts.
const NEW_SECTIONS: [&str; 1] = ["content"];

/// One file to create, compiled from a `.ctb` `new` item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSpec {
    /// 1-based position in the script.
    pub ordinal: usize,
    /// 1-based script line of the opening `new` directive.
    pub line: usize,
    /// The target path, from `file=`.
    pub file: String,
    /// Per-item collision policy, overriding the invocation's `--on-exists`.
    pub on_exists: Option<OnExists>,
    /// The body, verbatim from the `content` section (empty when absent).
    pub content: String,
}

/// Compile one parsed `new` item into a [`NewSpec`]. The body is taken exactly
/// as written — a `.ctb` payload section is verbatim text, so code pastes in
/// with no escaping at all.
pub fn compile_item(item: &Item, ordinal: usize) -> Result<NewSpec, String> {
    let at = |msg: String| format!("new {ordinal} (script line {}): {msg}", item.line);

    if item.directive != "new" {
        return Err(at(format!("unknown directive '{}'", item.directive)));
    }
    for (k, _) in &item.attrs {
        if !NEW_ATTRS.contains(&k.as_str()) {
            return Err(at(format!("unknown attribute '{k}' (file|on-exists)")));
        }
    }
    for (k, _) in &item.sections {
        if !NEW_SECTIONS.contains(&k.as_str()) {
            return Err(at(format!("unknown section '{k}' (content)")));
        }
    }

    let file = item
        .attr("file")
        .ok_or_else(|| at("missing file= attribute".to_string()))?;
    if file.is_empty() {
        return Err(at("empty file= attribute".to_string()));
    }
    let on_exists = match item.attr("on-exists") {
        Some(s) => Some(OnExists::parse(s).map_err(at)?),
        None => None,
    };

    Ok(NewSpec {
        ordinal,
        line: item.line,
        file: file.to_string(),
        on_exists,
        content: item.section("content").unwrap_or_default().to_string(),
    })
}

/// Reject a script that names the same target twice: which body would win is
/// exactly the kind of order-dependence the batch form exists to remove.
///
/// # Examples
///
/// ```
/// use coding_tools::newfile::{NewSpec, check_unique};
///
/// let spec = |ordinal, file: &str, line| NewSpec {
///     ordinal, line, file: file.to_string(), on_exists: None, content: String::new(),
/// };
/// assert!(check_unique(&[spec(1, "a.rs", 1), spec(2, "b.rs", 5)]).is_ok());
///
/// // The same path twice — however it is spelled — is a usage error.
/// let err = check_unique(&[spec(1, "src/a.rs", 1), spec(2, "src/a.rs", 5)]).unwrap_err();
/// assert!(err.contains("src/a.rs"), "{err}");
/// ```
pub fn check_unique(specs: &[NewSpec]) -> Result<(), String> {
    for (i, a) in specs.iter().enumerate() {
        for b in &specs[i + 1..] {
            if Path::new(&a.file) == Path::new(&b.file) {
                return Err(format!(
                    "new {} (script line {}) and new {} (script line {}) both target {}",
                    a.ordinal, a.line, b.ordinal, b.line, a.file
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockdoc;

    fn items(src: &str) -> Vec<Item> {
        blockdoc::parse(src, blockdoc::DEFAULT_FENCE, &["new"]).unwrap()
    }

    #[test]
    fn a_content_section_is_taken_verbatim() {
        let src = "#% new file=src/a.rs\n#% content\nfn a() {}\n\nfn b() {}\n#% end\n";
        let spec = compile_item(&items(src)[0], 1).unwrap();
        assert_eq!(spec.file, "src/a.rs");
        assert_eq!(spec.content, "fn a() {}\n\nfn b() {}\n");
        assert_eq!(spec.on_exists, None);
    }

    #[test]
    fn a_missing_content_section_is_an_empty_file() {
        let spec = compile_item(&items("#% new file=marker\n#% end\n")[0], 1).unwrap();
        assert_eq!(spec.content, "");
    }

    #[test]
    fn per_item_policy_overrides_the_invocation() {
        let spec = compile_item(&items("#% new file=a on-exists=skip\n#% end\n")[0], 1).unwrap();
        assert_eq!(spec.on_exists, Some(OnExists::Skip));
    }

    #[test]
    fn the_item_vocabulary_is_closed() {
        let bad_attr = compile_item(&items("#% new file=a expect==1\n#% end\n")[0], 1).unwrap_err();
        assert!(
            bad_attr.contains("unknown attribute 'expect'"),
            "{bad_attr}"
        );

        let bad_section =
            compile_item(&items("#% new file=a\n#% body\nx\n#% end\n")[0], 1).unwrap_err();
        assert!(
            bad_section.contains("unknown section 'body'"),
            "{bad_section}"
        );

        let no_file = compile_item(&items("#% new\n#% end\n")[0], 1).unwrap_err();
        assert!(no_file.contains("missing file="), "{no_file}");

        let bad_policy =
            compile_item(&items("#% new file=a on-exists=force\n#% end\n")[0], 1).unwrap_err();
        assert!(bad_policy.contains("fail|skip|overwrite"), "{bad_policy}");
    }

    #[test]
    fn errors_name_the_script_line() {
        let src = "#% new file=a\n#% end\n\n#% new file=b bogus=1\n#% end\n";
        let err = compile_item(&items(src)[1], 2).unwrap_err();
        assert!(err.contains("new 2 (script line 4)"), "{err}");
    }

    #[test]
    fn duplicate_targets_are_rejected() {
        let spec = |ordinal, file: &str, line| NewSpec {
            ordinal,
            line,
            file: file.to_string(),
            on_exists: None,
            content: String::new(),
        };
        assert!(check_unique(&[spec(1, "src/a.rs", 1), spec(2, "src/a.rs", 5)]).is_err());
        assert!(check_unique(&[spec(1, "src/a.rs", 1), spec(2, "src/b.rs", 5)]).is_ok());
        // On Windows the two separators spell one path, and the guard sees it.
        if cfg!(windows) {
            assert!(check_unique(&[spec(1, "src/a.rs", 1), spec(2, "src\\a.rs", 5)]).is_err());
        }
    }

    #[test]
    fn a_blocked_target_is_never_ensured() {
        assert!(!Action::Blocked.ensured());
        assert!(!Action::Blocked.writes());
        assert!(Action::Skipped.ensured());
        assert!(
            !Action::Skipped.writes(),
            "an accepted file is not rewritten"
        );
        assert!(Action::Overwritten.writes());
    }
}
