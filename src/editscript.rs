// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-edit --script` engine: a batch of edits applied under the
//! prepare/confirm/write standard.
//!
//! Phase 1 simulates the whole script in memory over [`FileBuf`]s — in script
//! order under cascade (each edit matches the buffer as transformed by
//! earlier edits), or against pristine content with an overlap check under
//! `--no-cascade`. Every edit's expectation is judged in the simulation;
//! the caller writes the final buffers only when every edit passed. Nothing
//! here touches the filesystem.

use crate::block::{self, NearestMiss};
use crate::blockdoc::Item;
use crate::edit::{Site, edit_content};
use crate::pattern::{self, Mode};
use crate::payload;
use crate::verdict::{Expect, Verdict};
use regex::Regex;

/// One selected file, held in memory for the whole simulation.
#[derive(Debug, Clone)]
pub struct FileBuf {
    pub path: String,
    pub content: String,
}

/// A compiled edit operation.
pub enum Op {
    /// Line-anchored literal block find/replace (empty `replace` deletes).
    Block {
        find: Vec<String>,
        replace: Vec<String>,
    },
    /// Single-line find/replace, as the argv form does it.
    Line {
        re: Regex,
        literal: bool,
        replace: String,
    },
}

/// One edit from the script, compiled and ready to run.
pub struct EditSpec {
    /// 1-based position in the script.
    pub ordinal: usize,
    /// 1-based script line of the opening `edit` directive.
    pub line: usize,
    pub expect: Expect,
    pub expect_label: String,
    pub mode_label: String,
    pub op: Op,
    /// Optional `file=` narrowing within the invocation's selection.
    pub file: Option<String>,
}

/// One edit's simulated outcome.
#[derive(Debug)]
pub struct EditOutcome {
    pub ordinal: usize,
    pub expect: String,
    pub mode: String,
    pub replacements: usize,
    pub verdict: Verdict,
    pub sites: Vec<Site>,
    /// Best partial alignment when a literal block matched nothing: (path, miss).
    pub miss: Option<(String, NearestMiss)>,
}

/// The attribute and section vocabulary of an `edit` item.
const EDIT_ATTRS: [&str; 3] = ["expect", "mode", "file"];
const EDIT_SECTIONS: [&str; 2] = ["find", "replace"];

/// Compile one parsed `edit` item into an [`EditSpec`]. Defaults inside a
/// script: `expect "=1"` (an anchored structural edit means *exactly here*,
/// and the stricter default is the safer one inside an atomic batch) and
/// `mode literal` (promotion is off in scripts; the author states intent).
pub fn compile_item(item: &Item, ordinal: usize) -> Result<EditSpec, String> {
    let at = |msg: String| format!("edit {ordinal} (script line {}): {msg}", item.line);

    for (k, _) in &item.attrs {
        if !EDIT_ATTRS.contains(&k.as_str()) {
            return Err(at(format!("unknown attribute '{k}'")));
        }
    }
    for (k, _) in &item.sections {
        if !EDIT_SECTIONS.contains(&k.as_str()) {
            return Err(at(format!("unknown section '{k}'")));
        }
    }

    let expect_label = item.attr("expect").unwrap_or("=1").to_string();
    let expect = Expect::parse(&expect_label).map_err(|e| at(format!("invalid expect: {e}")))?;
    let mode_label = item.attr("mode").unwrap_or("literal").to_string();
    let mode = match mode_label.as_str() {
        "literal" => Mode::Literal,
        "glob" => Mode::Glob,
        "regex" => Mode::Regex,
        other => return Err(at(format!("invalid mode '{other}' (literal|glob|regex)"))),
    };

    let find_payload = item
        .section("find")
        .ok_or_else(|| at("missing 'find' section".to_string()))?;
    let replace_payload = item
        .section("replace")
        .ok_or_else(|| at("missing 'replace' section".to_string()))?;
    let find_lines = payload::to_lines(find_payload);
    if find_lines.is_empty() {
        return Err(at("empty 'find' section".to_string()));
    }

    let op = if find_lines.len() > 1 {
        if mode != Mode::Literal {
            return Err(at(
                "a multi-line find matches as a literal block; mode glob/regex is reserved"
                    .to_string(),
            ));
        }
        Op::Block {
            find: find_lines,
            replace: payload::to_lines(replace_payload),
        }
    } else {
        let single = find_lines.into_iter().next().unwrap();
        let re = pattern::compile_with(&single, Some(mode))
            .map_err(|e| at(format!("invalid find pattern: {e}")))?;
        Op::Line {
            re,
            literal: mode != Mode::Regex,
            replace: replace_payload
                .strip_suffix('\n')
                .unwrap_or(replace_payload)
                .to_string(),
        }
    };

    Ok(EditSpec {
        ordinal,
        line: item.line,
        expect,
        expect_label,
        mode_label,
        op,
        file: item.attr("file").map(str::to_string),
    })
}

/// The file indices an edit applies to, honouring its `file=` narrowing
/// (exact path or whole-component suffix within the selection). The match is
/// separator-agnostic — `/` and `\` are treated as equivalent — so narrowing
/// works against the OS-native paths the walker yields on Windows too.
fn candidates(spec: &EditSpec, files: &[FileBuf]) -> Result<Vec<usize>, String> {
    let Some(f) = &spec.file else {
        return Ok((0..files.len()).collect());
    };
    let norm = |p: &str| p.replace('\\', "/");
    let target = norm(f);
    let suffix = format!("/{target}");
    let cand: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, fb)| {
            let p = norm(&fb.path);
            p == target || p.ends_with(&suffix)
        })
        .map(|(i, _)| i)
        .collect();
    if cand.is_empty() {
        return Err(format!(
            "edit {} (script line {}): file={f} matches no selected file",
            spec.ordinal, spec.line
        ));
    }
    Ok(cand)
}

impl Op {
    /// Apply this operation to one file's content: new content, occurrence
    /// count, changed sites. Shared by the script engine and the argv form.
    pub fn apply(&self, path: &str, content: &str) -> (String, usize, Vec<Site>) {
        match self {
            Op::Block { find, replace } => block::edit_blocks(path, content, find, replace),
            Op::Line {
                re,
                literal,
                replace,
            } => edit_content(path, content, re, replace, *literal),
        }
    }
}

/// Track the deepest-diverging nearest miss across candidate files.
fn track_miss(
    best: &mut Option<(String, NearestMiss)>,
    path: &str,
    content: &str,
    find: &[String],
) {
    let lines: Vec<&str> = content.lines().collect();
    if let Some(m) = block::nearest_miss(&lines, find)
        && best
            .as_ref()
            .is_none_or(|(_, b)| m.first_diverging_line > b.first_diverging_line)
    {
        *best = Some((path.to_string(), m));
    }
}

/// Run the script with cascade: edits run in script order, each matching the
/// buffers as already transformed by earlier edits, exactly as the final
/// write would have it. Buffers are updated even past a failing edit so the
/// remaining diagnostics stay meaningful; the caller writes nothing unless
/// every outcome is `SUCCESS`.
pub fn run_cascade(specs: &[EditSpec], files: &mut [FileBuf]) -> Result<Vec<EditOutcome>, String> {
    let mut outcomes = Vec::with_capacity(specs.len());
    for spec in specs {
        let cand = candidates(spec, files)?;
        let mut total = 0usize;
        let mut sites: Vec<Site> = Vec::new();
        let mut miss: Option<(String, NearestMiss)> = None;
        for &i in &cand {
            let f = &mut files[i];
            let (new_content, hits, s) = spec.op.apply(&f.path, &f.content);
            if hits > 0 {
                f.content = new_content;
                total += hits;
                sites.extend(s);
            } else if let Op::Block { find, .. } = &spec.op {
                track_miss(&mut miss, &f.path, &f.content, find);
            }
        }
        let verdict = spec.expect.eval(total as u64);
        outcomes.push(EditOutcome {
            ordinal: spec.ordinal,
            expect: spec.expect_label.clone(),
            mode: spec.mode_label.clone(),
            replacements: total,
            verdict,
            sites,
            miss: (verdict != Verdict::Success && total == 0)
                .then_some(miss)
                .flatten(),
        });
    }
    Ok(outcomes)
}

/// A pending line-range replacement located against pristine content.
struct Splice {
    file: usize,
    start: usize,
    len: usize,
    replacement: Vec<String>,
}

/// Run the script without cascade: every edit matches pristine content, any
/// two edits touching the same line is a usage error, and the located
/// splices are applied positionally so the result is exactly what was
/// verified.
pub fn run_no_cascade(
    specs: &[EditSpec],
    files: &mut [FileBuf],
) -> Result<Vec<EditOutcome>, String> {
    let pristine: Vec<String> = files.iter().map(|f| f.content.clone()).collect();
    let mut outcomes = Vec::with_capacity(specs.len());
    let mut splices: Vec<(usize, Splice)> = Vec::new(); // (ordinal, splice)

    for spec in specs {
        let cand = candidates(spec, files)?;
        let mut total = 0usize;
        let mut sites: Vec<Site> = Vec::new();
        let mut miss: Option<(String, NearestMiss)> = None;
        for &i in &cand {
            let (_, hits, s) = spec.op.apply(&files[i].path, &pristine[i]);
            if hits == 0 {
                if let Op::Block { find, .. } = &spec.op {
                    track_miss(&mut miss, &files[i].path, &pristine[i], find);
                }
                continue;
            }
            total += hits;
            for site in &s {
                let (len, replacement) = match &spec.op {
                    Op::Block { find, replace } => (find.len(), replace.clone()),
                    Op::Line { .. } => (1, site.after.split('\n').map(str::to_string).collect()),
                };
                splices.push((
                    spec.ordinal,
                    Splice {
                        file: i,
                        start: site.line - 1,
                        len,
                        replacement,
                    },
                ));
            }
            sites.extend(s);
        }
        let verdict = spec.expect.eval(total as u64);
        outcomes.push(EditOutcome {
            ordinal: spec.ordinal,
            expect: spec.expect_label.clone(),
            mode: spec.mode_label.clone(),
            replacements: total,
            verdict,
            sites,
            miss: (verdict != Verdict::Success && total == 0)
                .then_some(miss)
                .flatten(),
        });
    }

    // Overlap check: without cascade, two edits touching the same line are
    // ambiguous by construction.
    splices.sort_by_key(|(_, s)| (s.file, s.start));
    for pair in splices.windows(2) {
        let (ord_a, a) = &pair[0];
        let (ord_b, b) = &pair[1];
        if a.file == b.file && b.start < a.start + a.len && ord_a != ord_b {
            return Err(format!(
                "edits {ord_a} and {ord_b} overlap at {}:{} (no-cascade requires disjoint edits)",
                files[a.file].path,
                b.start + 1
            ));
        }
    }

    // Apply positionally, bottom-up per file, so earlier indices stay valid.
    for (_, s) in splices.iter().rev() {
        let f = &mut files[s.file];
        f.content = splice_lines(&f.content, s.start, s.len, &s.replacement);
    }
    Ok(outcomes)
}

/// Replace `len` lines starting at 0-based `start` with `replacement` lines,
/// preserving every untouched byte (including a missing final newline).
fn splice_lines(content: &str, start: usize, len: usize, replacement: &[String]) -> String {
    let segments: Vec<(&str, &str)> = content
        .split_inclusive('\n')
        .map(|seg| match seg.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (seg, ""),
        })
        .collect();
    let mut out = String::with_capacity(content.len());
    for (i, (body, term)) in segments.iter().enumerate() {
        if i == start {
            let last_term = segments[(start + len - 1).min(segments.len() - 1)].1;
            for (r, rl) in replacement.iter().enumerate() {
                out.push_str(rl);
                out.push_str(if r + 1 == replacement.len() {
                    last_term
                } else {
                    "\n"
                });
            }
        }
        if i < start || i >= start + len {
            out.push_str(body);
            out.push_str(term);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockdoc::{DEFAULT_FENCE, parse};

    fn bufs(files: &[(&str, &str)]) -> Vec<FileBuf> {
        files
            .iter()
            .map(|(p, c)| FileBuf {
                path: p.to_string(),
                content: c.to_string(),
            })
            .collect()
    }

    fn specs(doc: &str) -> Vec<EditSpec> {
        parse(doc, DEFAULT_FENCE, &["edit"])
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, it)| compile_item(it, i + 1).unwrap())
            .collect()
    }

    #[test]
    fn script_default_expect_is_exactly_one() {
        let s = specs("#% edit\n#% find\nx\n#% replace\ny\n#% end\n");
        assert_eq!(s[0].expect_label, "=1");
        let mut files = bufs(&[("a", "x\nx\n")]);
        let out = run_cascade(&s, &mut files).unwrap();
        // Two sites against expect =1: the edit fails.
        assert_eq!(out[0].replacements, 2);
        assert_eq!(out[0].verdict, Verdict::Error);
    }

    #[test]
    fn cascade_lets_a_later_edit_see_an_earlier_one() {
        let doc = "\
#% edit
#% find
base()
#% replace
base()
added()
#% edit
#% find
added()
#% replace
added(1)
#% end
";
        let s = specs(doc);
        let mut files = bufs(&[("a", "base()\n")]);
        let out = run_cascade(&s, &mut files).unwrap();
        assert!(out.iter().all(|o| o.verdict == Verdict::Success));
        assert_eq!(files[0].content, "base()\nadded(1)\n");
    }

    #[test]
    fn no_cascade_judges_pristine_and_rejects_overlap() {
        let doc = "\
#% edit
#% find
a
b
#% replace
A
#% edit
#% find
b
c
#% replace
C
#% end
";
        let s = specs(doc);
        let mut files = bufs(&[("f", "a\nb\nc\n")]);
        let err = run_no_cascade(&s, &mut files).unwrap_err();
        assert!(err.contains("overlap"), "{err}");
    }

    #[test]
    fn no_cascade_applies_disjoint_edits_positionally() {
        let doc = "\
#% edit
#% find
a
#% replace
A1
A2
#% edit
#% find
c
#% replace
#% end
";
        let s = specs(doc);
        let mut files = bufs(&[("f", "a\nb\nc")]);
        let out = run_no_cascade(&s, &mut files).unwrap();
        assert!(out.iter().all(|o| o.verdict == Verdict::Success));
        // Block growth above, deletion below, missing final newline preserved
        // on the spliced tail.
        assert_eq!(files[0].content, "A1\nA2\nb\n");
    }

    #[test]
    fn failing_block_edit_carries_a_nearest_miss() {
        let doc = "#% edit\n#% find\nfn a() {\n    three();\n#% replace\nx\n#% end\n";
        let s = specs(doc);
        let mut files = bufs(&[("f", "fn a() {\n    two();\n}\n")]);
        let out = run_cascade(&s, &mut files).unwrap();
        assert_eq!(out[0].verdict, Verdict::Error);
        let (path, m) = out[0].miss.as_ref().unwrap();
        assert_eq!(path, "f");
        assert_eq!(m.first_diverging_line, 2);
    }

    #[test]
    fn file_narrowing_limits_and_validates() {
        let doc = "#% edit file=b.rs\n#% find\nx\n#% replace\ny\n#% end\n";
        let s = specs(doc);
        let mut files = bufs(&[("./src/a.rs", "x\n"), ("./src/b.rs", "x\n")]);
        let out = run_cascade(&s, &mut files).unwrap();
        assert_eq!(out[0].replacements, 1);
        assert_eq!(files[0].content, "x\n");
        assert_eq!(files[1].content, "y\n");

        let missing = specs("#% edit file=zzz.rs\n#% find\nx\n#% replace\ny\n#% end\n");
        let mut files = bufs(&[("./src/a.rs", "x\n")]);
        assert!(run_cascade(&missing, &mut files).is_err());
    }

    #[test]
    fn file_narrowing_matches_backslash_paths() {
        // The walker yields OS-native paths; on Windows that means backslashes,
        // which the `/`-suffix match must still narrow against. A forward-slash
        // `file=` selects the right backslash path and leaves the others alone.
        let doc = "#% edit file=b.rs\n#% find\nx\n#% replace\ny\n#% end\n";
        let s = specs(doc);
        let mut files = bufs(&[
            ("C:\\proj\\src\\a.rs", "x\n"),
            ("C:\\proj\\src\\b.rs", "x\n"),
        ]);
        let out = run_cascade(&s, &mut files).unwrap();
        assert_eq!(out[0].replacements, 1);
        assert_eq!(files[0].content, "x\n"); // a.rs untouched
        assert_eq!(files[1].content, "y\n"); // b.rs edited
    }
}
