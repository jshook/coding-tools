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
//!
//! The script vocabulary (see [`DIRECTIVES`]):
//!
//! | directive | sections | what it does |
//! | --- | --- | --- |
//! | `edit` | `find`, `replace` | replace the anchor (a block, or per line) |
//! | `insert-before` / `insert-after` | `find`, `text` | put `text` on the lines before/after the anchor, which is named once and left as it is |
//! | `drop` | `find` | delete the anchor's whole lines |
//! | `append` / `prepend` | `text` | put `text` at the end/start of the file |
//!
//! Every directive narrows with `file=` and states `expect=` (default `=1`);
//! the anchored ones take `mode=` and `squeeze=` like `edit`.

use crate::block::{self, NearestMiss};
use crate::blockdoc::Item;
use crate::edit::{Pick, Picker, Site, edit_content_picked};
use crate::lines::{Segments, Splice, render};
use crate::pattern::{self, Mode, PatternKind};
use crate::payload;
use crate::verdict::{Expect, Verdict};
use regex::Regex;

/// The item-opening directives of an edit script, as [`crate::blockdoc::parse`]
/// needs them declared.
pub const DIRECTIVES: &[&str] = &[
    "edit",
    "insert-before",
    "insert-after",
    "drop",
    "append",
    "prepend",
];

/// One selected file, held in memory for the whole simulation.
#[derive(Debug, Clone)]
pub struct FileBuf {
    pub path: String,
    pub content: String,
}

/// Where an insertion or deletion anchors.
pub enum Anchor {
    /// A multi-line literal block (optionally blank-run squeezed).
    Block { lines: Vec<String>, squeeze: bool },
    /// A single-line pattern; every line it matches is an anchor line.
    Line { re: Regex },
}

/// A compiled edit operation.
pub enum Op {
    /// Line-anchored literal block find/replace (empty `replace` deletes).
    /// `squeeze` makes a blank run in `find` match a run of 1+ blank source
    /// lines (see [`crate::block::find_spans_squeezed`]).
    Block {
        find: Vec<String>,
        replace: Vec<String>,
        squeeze: bool,
    },
    /// Single-line find/replace, as the argv form does it.
    Line {
        re: Regex,
        literal: bool,
        replace: String,
    },
    /// Insert `text` as whole lines before (or `after`) each anchor match,
    /// leaving the anchor untouched.
    Insert {
        anchor: Anchor,
        text: Vec<String>,
        after: bool,
    },
    /// Delete every whole line the pattern matches.
    DropLines { re: Regex },
    /// Add `text` as the file's last lines.
    Append { text: Vec<String> },
    /// Add `text` as the file's first lines.
    Prepend { text: Vec<String> },
}

/// One edit from the script, compiled and ready to run.
pub struct EditSpec {
    /// 1-based position in the script.
    pub ordinal: usize,
    /// 1-based script line of the opening directive.
    pub line: usize,
    /// The directive name (`edit`, `insert-after`, …).
    pub kind: &'static str,
    pub expect: Expect,
    pub expect_label: String,
    /// The match mode label; `None` for the unanchored `append`/`prepend`.
    pub mode_label: Option<String>,
    pub op: Op,
    /// Optional `file=` narrowing within the invocation's selection.
    pub file: Option<String>,
}

/// One edit's simulated outcome.
#[derive(Debug)]
pub struct EditOutcome {
    pub ordinal: usize,
    pub kind: &'static str,
    pub expect: String,
    pub mode: Option<String>,
    pub replacements: usize,
    pub verdict: Verdict,
    pub sites: Vec<Site>,
    /// Best partial alignment when a literal block matched nothing: (path, miss).
    pub miss: Option<(String, NearestMiss)>,
}

/// The attribute and section vocabulary per directive.
fn vocabulary(
    directive: &str,
) -> Option<(
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
)> {
    const ANCHORED: &[&str] = &["expect", "mode", "file", "squeeze"];
    Some(match directive {
        "edit" => ("edit", ANCHORED, &["find", "replace"]),
        "insert-before" => ("insert-before", ANCHORED, &["find", "text"]),
        "insert-after" => ("insert-after", ANCHORED, &["find", "text"]),
        "drop" => ("drop", ANCHORED, &["find"]),
        "append" => ("append", &["expect", "file"], &["text"]),
        "prepend" => ("prepend", &["expect", "file"], &["text"]),
        _ => return None,
    })
}

/// Compile one parsed item into an [`EditSpec`]. Defaults inside a script:
/// `expect "=1"` (an anchored structural edit means *exactly here*, and the
/// stricter default is the safer one inside an atomic batch) and `mode
/// literal` (promotion is off in scripts; the author states intent).
pub fn compile_item(item: &Item, ordinal: usize) -> Result<EditSpec, String> {
    let Some((kind, attrs, sections)) = vocabulary(&item.directive) else {
        return Err(format!(
            "item {ordinal} (script line {}): unknown directive '{}' (expected one of: {})",
            item.line,
            item.directive,
            DIRECTIVES.join(", ")
        ));
    };
    let at = |msg: String| format!("{kind} {ordinal} (script line {}): {msg}", item.line);

    for (k, _) in &item.attrs {
        if !attrs.contains(&k.as_str()) {
            return Err(at(format!(
                "unknown attribute '{k}' (a {kind} takes: {})",
                attrs.join(", ")
            )));
        }
    }
    for (k, _) in &item.sections {
        if !sections.contains(&k.as_str()) {
            return Err(at(format!(
                "unknown section '{k}' (a {kind} takes: {})",
                sections.join(", ")
            )));
        }
    }
    let section = |name: &str| {
        item.section(name)
            .ok_or_else(|| at(format!("missing '{name}' section")))
    };

    let expect_label = item.attr("expect").unwrap_or("=1").to_string();
    let expect = Expect::parse(&expect_label).map_err(|e| at(format!("invalid expect: {e}")))?;
    let file = item.attr("file").map(str::to_string);

    // The unanchored pair: text only, no mode.
    if matches!(kind, "append" | "prepend") {
        let text = payload::to_lines(section("text")?);
        if text.is_empty() {
            return Err(at("empty 'text' section".to_string()));
        }
        let op = if kind == "append" {
            Op::Append { text }
        } else {
            Op::Prepend { text }
        };
        return Ok(EditSpec {
            ordinal,
            line: item.line,
            kind,
            expect,
            expect_label,
            mode_label: None,
            op,
            file,
        });
    }

    let mode_label = item.attr("mode").unwrap_or("literal").to_string();
    let mode = match mode_label.as_str() {
        "literal" => Mode::Literal,
        "glob" => Mode::Glob,
        "regex" => Mode::Regex,
        "auto" => Mode::Auto,
        other => {
            return Err(at(format!(
                "invalid mode '{other}' (literal|glob|regex|auto)"
            )));
        }
    };
    let squeeze = match item.attr("squeeze") {
        None | Some("false") => false,
        Some("true") => true,
        Some(other) => return Err(at(format!("invalid squeeze '{other}' (true|false)"))),
    };

    // Anchor lines drop a phantom trailing blank line; a replacement or
    // inserted text keeps every line (see `payload::to_find_lines`).
    let find_lines = payload::to_find_lines(section("find")?);
    if find_lines.is_empty() {
        return Err(at("empty 'find' section".to_string()));
    }
    let is_block = find_lines.len() > 1;
    if is_block && !matches!(mode, Mode::Literal | Mode::Auto) {
        return Err(at(
            "a multi-line find matches as a literal block; mode glob/regex is reserved".to_string(),
        ));
    }
    let line_re = |single: &str| {
        pattern::compile_with(single, Some(mode))
            .map_err(|e| at(format!("invalid find pattern: {e}")))
    };

    let op = match kind {
        "edit" => {
            let replace_payload = section("replace")?;
            if is_block {
                Op::Block {
                    find: find_lines,
                    replace: payload::to_lines(replace_payload),
                    squeeze,
                }
            } else {
                let single = &find_lines[0];
                Op::Line {
                    re: line_re(single)?,
                    literal: !matches!(
                        pattern::classify_with(single, Some(mode)),
                        PatternKind::Regex
                    ),
                    replace: replace_payload
                        .strip_suffix('\n')
                        .unwrap_or(replace_payload)
                        .to_string(),
                }
            }
        }
        "insert-before" | "insert-after" => {
            let text = payload::to_lines(section("text")?);
            if text.is_empty() {
                return Err(at("empty 'text' section".to_string()));
            }
            let anchor = if is_block {
                Anchor::Block {
                    lines: find_lines,
                    squeeze,
                }
            } else {
                Anchor::Line {
                    re: line_re(&find_lines[0])?,
                }
            };
            Op::Insert {
                anchor,
                text,
                after: kind == "insert-after",
            }
        }
        "drop" => {
            if is_block {
                Op::Block {
                    find: find_lines,
                    replace: Vec::new(),
                    squeeze,
                }
            } else {
                Op::DropLines {
                    re: line_re(&find_lines[0])?,
                }
            }
        }
        _ => unreachable!("vocabulary() admits only known directives"),
    };

    Ok(EditSpec {
        ordinal,
        line: item.line,
        kind,
        expect,
        expect_label,
        mode_label: Some(mode_label),
        op,
        file,
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
    let cand: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, fb)| crate::edit::path_matches(&fb.path, f))
        .map(|(i, _)| i)
        .collect();
    if cand.is_empty() {
        return Err(format!(
            "{} {} (script line {}): file={f} matches no selected file",
            spec.kind, spec.ordinal, spec.line
        ));
    }
    Ok(cand)
}

impl Anchor {
    /// The `(start, line count)` spans this anchor matches in `bodies`, as
    /// accepted by `picker`.
    fn spans(&self, bodies: &[&str], path: &str, picker: &mut Picker<'_>) -> Vec<(usize, usize)> {
        match self {
            Anchor::Block { lines, squeeze } => {
                block::block_spans(bodies, lines, *squeeze, path, picker)
            }
            Anchor::Line { re } => line_spans(bodies, re, path, picker),
        }
    }
}

/// Every line of `bodies` that `re` matches, as a one-line span, filtered by
/// `picker`.
fn line_spans(
    bodies: &[&str],
    re: &Regex,
    path: &str,
    picker: &mut Picker<'_>,
) -> Vec<(usize, usize)> {
    bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| re.is_match(b))
        .filter(|(i, _)| picker.accept(path, i + 1))
        .map(|(i, _)| (i, 1))
        .collect()
}

impl Op {
    /// Apply this operation to one file's content: new content, occurrence
    /// count, changed sites. Shared by the script engine and the argv form.
    pub fn apply(&self, path: &str, content: &str) -> (String, usize, Vec<Site>) {
        self.apply_picked(path, content, &mut Picker::new(&Pick::All))
    }

    /// [`apply`](Op::apply) under a running [`Picker`], so the argv form's
    /// `--nth`/`--at` can take one site out of several. The picker is threaded
    /// across files by the caller: an ordinal counts over the whole selection,
    /// not per file.
    pub fn apply_picked(
        &self,
        path: &str,
        content: &str,
        picker: &mut Picker<'_>,
    ) -> (String, usize, Vec<Site>) {
        match self {
            Op::Block {
                find,
                replace,
                squeeze,
            } => block::edit_blocks_picked(path, content, find, replace, *squeeze, picker),
            Op::Line {
                re,
                literal,
                replace,
            } => edit_content_picked(path, content, re, replace, *literal, picker),
            Op::Insert {
                anchor,
                text,
                after,
            } => {
                let segs = Segments::split(content);
                let spans = anchor.spans(&segs.bodies(), path, picker);
                let splices: Vec<Splice> = spans
                    .iter()
                    .map(|&(s, k)| Splice {
                        start: if *after { s + k } else { s },
                        len: 0,
                        lines: text.clone(),
                    })
                    .collect();
                finish(path, &segs, splices)
            }
            Op::DropLines { re } => {
                let segs = Segments::split(content);
                let bodies = segs.bodies();
                let splices: Vec<Splice> = line_spans(&bodies, re, path, picker)
                    .into_iter()
                    .map(|(s, _)| Splice {
                        start: s,
                        len: 1,
                        lines: Vec::new(),
                    })
                    .collect();
                finish(path, &segs, splices)
            }
            Op::Append { text } => {
                let segs = Segments::split(content);
                let splices = vec![Splice {
                    start: segs.len(),
                    len: 0,
                    lines: text.clone(),
                }];
                finish(path, &segs, splices)
            }
            Op::Prepend { text } => {
                let segs = Segments::split(content);
                let splices = vec![Splice {
                    start: 0,
                    len: 0,
                    lines: text.clone(),
                }];
                finish(path, &segs, splices)
            }
        }
    }

    /// The literal block this op anchors on, if any, for nearest-miss
    /// diagnosis: `(lines, squeeze)`.
    fn block_anchor(&self) -> Option<(&[String], bool)> {
        match self {
            Op::Block { find, squeeze, .. } => Some((find, *squeeze)),
            Op::Insert {
                anchor: Anchor::Block { lines, squeeze },
                ..
            } => Some((lines, *squeeze)),
            _ => None,
        }
    }

    /// The positional change a site of this op stands for, against the
    /// content the site was located in (the `--no-cascade` path).
    fn splice_for(&self, site: &Site) -> Splice {
        let start = site.line - 1;
        match self {
            // The matched source span (from `site.before`) is what gets
            // replaced — under squeeze it can be wider than `find`.
            Op::Block { replace, .. } => Splice {
                start,
                len: site.before.split('\n').count(),
                lines: replace.clone(),
            },
            Op::Line { .. } => Splice {
                start,
                len: 1,
                lines: site.after.split('\n').map(str::to_string).collect(),
            },
            Op::Insert { text, .. } | Op::Append { text } | Op::Prepend { text } => Splice {
                start,
                len: 0,
                lines: text.clone(),
            },
            Op::DropLines { .. } => Splice {
                start,
                len: 1,
                lines: Vec::new(),
            },
        }
    }
}

/// Turn located splices into the op's result: one site per splice (an
/// insertion's `before` is empty, a deletion's `after` is empty) and the
/// rendered content.
fn finish(path: &str, segs: &Segments<'_>, splices: Vec<Splice>) -> (String, usize, Vec<Site>) {
    if splices.is_empty() {
        return (render(segs, &[]), 0, Vec::new());
    }
    let sites = splices
        .iter()
        .map(|sp| Site {
            path: path.to_string(),
            line: sp.start + 1,
            before: segs.segs[sp.start..sp.start + sp.len]
                .iter()
                .map(|(b, _)| *b)
                .collect::<Vec<_>>()
                .join("\n"),
            after: sp.lines.join("\n"),
        })
        .collect();
    (render(segs, &splices), splices.len(), sites)
}

/// Track the deepest-diverging nearest miss across candidate files.
fn track_miss(best: &mut Option<(String, NearestMiss)>, path: &str, content: &str, op: &Op) {
    let Some((find, squeeze)) = op.block_anchor() else {
        return;
    };
    let lines: Vec<&str> = content.lines().collect();
    if let Some(m) = block::nearest_miss_with(&lines, find, squeeze)
        && best
            .as_ref()
            .is_none_or(|(_, b)| m.first_diverging_line > b.first_diverging_line)
    {
        *best = Some((path.to_string(), m));
    }
}

fn outcome(
    spec: &EditSpec,
    total: usize,
    sites: Vec<Site>,
    miss: Option<(String, NearestMiss)>,
) -> EditOutcome {
    let verdict = spec.expect.eval(total as u64);
    EditOutcome {
        ordinal: spec.ordinal,
        kind: spec.kind,
        expect: spec.expect_label.clone(),
        mode: spec.mode_label.clone(),
        replacements: total,
        verdict,
        sites,
        miss: (verdict != Verdict::Success && total == 0)
            .then_some(miss)
            .flatten(),
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
            } else {
                track_miss(&mut miss, &f.path, &f.content, &spec.op);
            }
        }
        outcomes.push(outcome(spec, total, sites, miss));
    }
    Ok(outcomes)
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
    // (file index, ordinal, splice)
    let mut splices: Vec<(usize, usize, Splice)> = Vec::new();

    for spec in specs {
        let cand = candidates(spec, files)?;
        let mut total = 0usize;
        let mut sites: Vec<Site> = Vec::new();
        let mut miss: Option<(String, NearestMiss)> = None;
        for &i in &cand {
            let (_, hits, s) = spec.op.apply(&files[i].path, &pristine[i]);
            if hits == 0 {
                track_miss(&mut miss, &files[i].path, &pristine[i], &spec.op);
                continue;
            }
            total += hits;
            splices.extend(
                s.iter()
                    .map(|site| (i, spec.ordinal, spec.op.splice_for(site))),
            );
            sites.extend(s);
        }
        outcomes.push(outcome(spec, total, sites, miss));
    }

    // Overlap check: without cascade, two edits touching the same line are
    // ambiguous by construction. A pure insertion at a replaced span's first
    // line lands above it, and one at the line after lands below, so those are
    // not overlaps; an insertion strictly inside a replaced span is.
    splices.sort_by_key(|(file, ord, s)| (*file, s.start, usize::from(s.len > 0), *ord));
    for pair in splices.windows(2) {
        let (file_a, ord_a, a) = &pair[0];
        let (file_b, ord_b, b) = &pair[1];
        if file_a == file_b && ord_a != ord_b && b.start < a.start + a.len {
            return Err(format!(
                "edits {ord_a} and {ord_b} overlap at {}:{} (no-cascade requires disjoint edits)",
                files[*file_a].path,
                b.start + 1
            ));
        }
    }

    // Apply per file in one positional pass over pristine content.
    for (i, f) in files.iter_mut().enumerate() {
        let mine: Vec<Splice> = splices
            .iter()
            .filter(|(file, _, _)| *file == i)
            .map(|(_, _, s)| s.clone())
            .collect();
        if !mine.is_empty() {
            f.content = render(&Segments::split(&pristine[i]), &mine);
        }
    }
    Ok(outcomes)
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
        parse(doc, DEFAULT_FENCE, DIRECTIVES)
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, it)| compile_item(it, i + 1).unwrap())
            .collect()
    }

    fn compile_err(doc: &str) -> String {
        parse(doc, DEFAULT_FENCE, DIRECTIVES)
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, it)| compile_item(it, i + 1))
            .find_map(Result::err)
            .expect("a compile error")
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
    fn no_cascade_keeps_crlf_endings() {
        // Before the shared renderer, the no-cascade splicer wrote interior
        // replacement lines with a bare `\n` into a CRLF file.
        let doc = "#% edit\n#% find\na\n#% replace\nA1\nA2\n#% end\n";
        let s = specs(doc);
        let mut files = bufs(&[("f", "a\r\nb\r\n")]);
        run_no_cascade(&s, &mut files).unwrap();
        assert_eq!(files[0].content, "A1\r\nA2\r\nb\r\n");
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

    #[test]
    fn insert_names_the_anchor_once_and_leaves_it_in_place() {
        let doc = "\
#% insert-after
#% find
fn a() {
    one();
#% text
    two();
#% insert-before
#% find
}
#% text
    // end
#% end
";
        let s = specs(doc);
        let mut files = bufs(&[("f", "fn a() {\n    one();\n}\n")]);
        let out = run_cascade(&s, &mut files).unwrap();
        assert!(out.iter().all(|o| o.verdict == Verdict::Success), "{out:?}");
        assert_eq!(out[0].kind, "insert-after");
        assert_eq!(
            files[0].content,
            "fn a() {\n    one();\n    two();\n    // end\n}\n"
        );
        // An insertion site has no `before`; its line is where the text landed.
        assert_eq!(out[0].sites[0].line, 3);
        assert_eq!(out[0].sites[0].before, "");
        assert_eq!(out[0].sites[0].after, "    two();");
    }

    #[test]
    fn insert_with_a_single_line_anchor_matches_per_line_and_counts_each() {
        let doc = "#% insert-before expect=\"=2\" mode=regex\n#% find\n^    x\\(\n#% text\n    // call\n#% end\n";
        let s = specs(doc);
        let mut files = bufs(&[("f", "    x(1);\n    y();\n    x(2);\n")]);
        let out = run_cascade(&s, &mut files).unwrap();
        assert_eq!(out[0].verdict, Verdict::Success);
        assert_eq!(
            files[0].content,
            "    // call\n    x(1);\n    y();\n    // call\n    x(2);\n"
        );
    }

    #[test]
    fn insert_after_a_missed_block_anchor_reports_the_nearest_miss() {
        let doc = "#% insert-after\n#% find\nfn a() {\n    three();\n#% text\nx\n#% end\n";
        let s = specs(doc);
        let mut files = bufs(&[("f", "fn a() {\n    two();\n}\n")]);
        let out = run_cascade(&s, &mut files).unwrap();
        assert_eq!(out[0].verdict, Verdict::Error);
        assert_eq!(out[0].miss.as_ref().unwrap().1.first_diverging_line, 2);
        assert_eq!(files[0].content, "fn a() {\n    two();\n}\n");
    }

    #[test]
    fn drop_deletes_whole_matching_lines_or_a_block() {
        let doc = "\
#% drop expect=\"=2\"
#% find
trace!(
#% drop
#% find
    a();
    b();
#% end
";
        let s = specs(doc);
        let mut files = bufs(&[(
            "f",
            "    trace!(1);\n    a();\n    b();\n    c();\n    trace!(2);\n",
        )]);
        let out = run_cascade(&s, &mut files).unwrap();
        assert!(out.iter().all(|o| o.verdict == Verdict::Success), "{out:?}");
        assert_eq!(files[0].content, "    c();\n");
        assert_eq!(out[0].sites[0].before, "    trace!(1);");
        assert_eq!(out[0].sites[0].after, "");
    }

    #[test]
    fn append_and_prepend_touch_file_ends_and_count_files() {
        let doc = "\
#% append file=notes.md
#% text
- new
#% prepend file=notes.md
#% text
# Title

#% end
";
        let s = specs(doc);
        let mut files = bufs(&[("notes.md", "- old\n"), ("other.md", "x\n")]);
        let out = run_cascade(&s, &mut files).unwrap();
        assert!(out.iter().all(|o| o.verdict == Verdict::Success), "{out:?}");
        assert!(out[0].mode.is_none());
        assert_eq!(files[0].content, "# Title\n\n- old\n- new\n");
        assert_eq!(files[1].content, "x\n");

        // Without file=, an append counts every selected file, and the =1
        // default refuses to touch two of them.
        let s = specs("#% append\n#% text\nz\n#% end\n");
        let mut files = bufs(&[("a", "1\n"), ("b", "2\n")]);
        let out = run_cascade(&s, &mut files).unwrap();
        assert_eq!(out[0].replacements, 2);
        assert_eq!(out[0].verdict, Verdict::Error);
    }

    #[test]
    fn append_preserves_crlf_and_a_missing_final_newline() {
        let s = specs("#% append\n#% text\nz\n#% end\n");
        let mut files = bufs(&[("crlf", "a\r\nb\r\n"), ("bare", "a\nb")]);
        run_cascade(&s, &mut files).unwrap();
        assert_eq!(files[0].content, "a\r\nb\r\nz\r\n");
        assert_eq!(files[1].content, "a\nb\nz");
    }

    #[test]
    fn no_cascade_places_insertions_around_replacements_without_overlap() {
        let doc = "\
#% edit
#% find
b
#% replace
B
#% insert-before
#% find
b
#% text
x
#% insert-after
#% find
b
#% text
y
#% end
";
        let s = specs(doc);
        let mut files = bufs(&[("f", "a\nb\nc\n")]);
        let out = run_no_cascade(&s, &mut files).unwrap();
        assert!(out.iter().all(|o| o.verdict == Verdict::Success), "{out:?}");
        assert_eq!(files[0].content, "a\nx\nB\ny\nc\n");

        // An insertion strictly inside a replaced block is an overlap.
        let doc = "#% edit\n#% find\na\nb\nc\n#% replace\nZ\n#% insert-after\n#% find\na\n#% text\nx\n#% end\n";
        let s = specs(doc);
        let mut files = bufs(&[("f", "a\nb\nc\n")]);
        let err = run_no_cascade(&s, &mut files).unwrap_err();
        assert!(err.contains("overlap"), "{err}");
    }

    #[test]
    fn vocabulary_errors_name_the_directive_and_its_sections() {
        let e = compile_err("#% insert-after\n#% find\nx\n#% replace\ny\n#% end\n");
        assert!(e.contains("insert-after 1"), "{e}");
        assert!(e.contains("unknown section 'replace'"), "{e}");
        assert!(e.contains("find, text"), "{e}");
        let e = compile_err("#% append mode=regex\n#% text\ny\n#% end\n");
        assert!(e.contains("unknown attribute 'mode'"), "{e}");
        let e = compile_err("#% append\n#% text\n#% end\n");
        assert!(e.contains("empty 'text' section"), "{e}");
        let e = compile_err("#% drop\n#% end\n");
        assert!(e.contains("missing 'find' section"), "{e}");
    }
}
