// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-outline`'s heuristic declaration detection.
//!
//! A rule pack per language turns a file's text into ordered [`Entry`] rows —
//! kind, name, 1-based `start:end` span, and nesting depth. Detection is
//! line-pattern matching plus a block heuristic per language family: brace
//! matching for Rust (over comment/string-stripped text), indentation for
//! Python, heading levels for Markdown (fenced code blocks ignored). It is a
//! comprehension aid, not a parser: start lines are exact; an end the
//! heuristic cannot derive is `None` (rendered `start:?`), never a guess.

/// One detected declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The source language's own keyword for the declaration (`fn`, `class`,
    /// `h2`, …) — there is no cross-language kind abstraction.
    pub kind: String,
    /// The declared name (for `impl`, the trait-for-type text; for headings,
    /// the title).
    pub name: String,
    /// Exact 1-based line the declaration starts on.
    pub start: usize,
    /// Best-effort 1-based inclusive end line; `None` when the block
    /// heuristic could not derive one.
    pub end: Option<usize>,
    /// 1-based nesting depth (`1` = top level).
    pub depth: usize,
}

/// A supported language rule pack, keyed by file extension via
/// [`language_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// `.rs` — brace blocks over comment/string-stripped text.
    Rust,
    /// `.py` — indentation blocks.
    Python,
    /// `.md` — ATX headings as the outline; fenced code blocks ignored.
    Markdown,
}

impl Lang {
    /// The display name used in messages.
    pub fn label(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::Markdown => "markdown",
        }
    }
}

/// The rule pack for a file extension, or `None` when the language is not
/// (yet) supported.
///
/// # Examples
///
/// ```
/// use coding_tools::outline::{language_for, Lang};
///
/// assert_eq!(language_for("rs"), Some(Lang::Rust));
/// assert_eq!(language_for("py"), Some(Lang::Python));
/// assert_eq!(language_for("md"), Some(Lang::Markdown));
/// assert_eq!(language_for("zig"), None); // skipped in walks
/// ```
pub fn language_for(ext: &str) -> Option<Lang> {
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Some(Lang::Rust),
        "py" => Some(Lang::Python),
        "md" => Some(Lang::Markdown),
        _ => None,
    }
}

/// Outline `text` with the `lang` rule pack, in source order.
///
/// # Examples
///
/// ```
/// use coding_tools::outline::{outline, Lang};
///
/// let src = "pub struct Point { x: i32 }\n\nimpl Point {\n    fn norm(&self) -> i32 { 0 }\n}\n";
/// let entries = outline(Lang::Rust, src);
/// let rows: Vec<String> = entries
///     .iter()
///     .map(|e| format!("{}:{} {} {} d{}", e.start, e.end.map_or("?".into(), |n| n.to_string()), e.kind, e.name, e.depth))
///     .collect();
/// assert_eq!(rows, [
///     "1:1 struct Point d1",
///     "3:5 impl Point d1",
///     "4:4 fn norm d2",
/// ]);
/// ```
pub fn outline(lang: Lang, text: &str) -> Vec<Entry> {
    match lang {
        Lang::Rust => rust_outline(text),
        Lang::Python => python_outline(text),
        Lang::Markdown => markdown_outline(text),
    }
}

// ----- Rust ---------------------------------------------------------------------

/// Replace comments and string/char-literal contents with spaces, preserving
/// line structure, so brace counting and keyword matching see only code.
/// Heuristic: raw strings, nested block comments, and lifetimes are handled;
/// pathological token sequences may still leak.
fn strip_rust(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let n = chars.len();

    // Emit ch verbatim for newline (keep line structure), else a space.
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };

    while i < n {
        let c = chars[i];
        match c {
            '/' if i + 1 < n && chars[i + 1] == '/' => {
                while i < n && chars[i] != '\n' {
                    out.push(' ');
                    i += 1;
                }
            }
            '/' if i + 1 < n && chars[i + 1] == '*' => {
                let mut depth = 1;
                out.push(' ');
                out.push(' ');
                i += 2;
                while i < n && depth > 0 {
                    if chars[i] == '/' && i + 1 < n && chars[i + 1] == '*' {
                        depth += 1;
                        out.push(blank(chars[i]));
                        out.push(blank(chars[i + 1]));
                        i += 2;
                    } else if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                        depth -= 1;
                        out.push(blank(chars[i]));
                        out.push(blank(chars[i + 1]));
                        i += 2;
                    } else {
                        out.push(blank(chars[i]));
                        i += 1;
                    }
                }
            }
            'r' | 'b'
                if {
                    // Raw / byte-string starts: r", r#", br", b".
                    let mut j = i;
                    if chars[j] == 'b' && j + 1 < n && chars[j + 1] == 'r' {
                        j += 1;
                    }
                    if chars[j] == 'r' || chars[j] == 'b' {
                        let mut k = j + 1;
                        if chars[j] == 'r' {
                            while k < n && chars[k] == '#' {
                                k += 1;
                            }
                        }
                        k < n && chars[k] == '"'
                    } else {
                        false
                    }
                } =>
            {
                // Copy the prefix, then blank the quoted body.
                let mut hashes = 0;
                out.push(chars[i]);
                i += 1;
                if i < n && chars[i] == 'r' {
                    out.push('r');
                    i += 1;
                }
                while i < n && chars[i] == '#' {
                    hashes += 1;
                    out.push('#');
                    i += 1;
                }
                out.push('"');
                i += 1; // opening quote
                'raw: while i < n {
                    if chars[i] == '"' {
                        let mut k = i + 1;
                        let mut seen = 0;
                        while k < n && chars[k] == '#' && seen < hashes {
                            k += 1;
                            seen += 1;
                        }
                        if seen == hashes {
                            out.push('"');
                            for _ in 0..hashes {
                                out.push('#');
                            }
                            i = k;
                            break 'raw;
                        }
                    }
                    out.push(blank(chars[i]));
                    i += 1;
                }
            }
            '"' => {
                out.push('"');
                i += 1;
                while i < n {
                    if chars[i] == '\\' && i + 1 < n {
                        out.push(' ');
                        out.push(' ');
                        i += 2;
                        continue;
                    }
                    if chars[i] == '"' {
                        out.push('"');
                        i += 1;
                        break;
                    }
                    out.push(blank(chars[i]));
                    i += 1;
                }
            }
            '\'' => {
                // Char literal vs lifetime: 'x' / '\n' are literals; 'a in a
                // generic position is a lifetime and passes through.
                if i + 2 < n && chars[i + 1] == '\\' {
                    // Escaped char literal: blank the body. (Column alignment
                    // is irrelevant; only line structure matters.)
                    out.push('\'');
                    i += 2;
                    while i < n && chars[i] != '\'' {
                        out.push(' ');
                        i += 1;
                    }
                    if i < n {
                        out.push('\'');
                        i += 1;
                    }
                } else if i + 2 < n && chars[i + 2] == '\'' {
                    out.push('\'');
                    out.push(' ');
                    out.push('\'');
                    i += 3;
                } else {
                    out.push('\'');
                    i += 1;
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The leading identifier of `s` (`[A-Za-z0-9_]+`), if any.
fn ident(s: &str) -> Option<&str> {
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    if end == 0 { None } else { Some(&s[..end]) }
}

/// Strip one `pub` / `pub(...)` visibility prefix.
fn strip_vis(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("pub") else {
        return s;
    };
    let rest = rest.trim_start();
    if let Some(after) = rest.strip_prefix('(') {
        if let Some(close) = after.find(')') {
            return after[close + 1..].trim_start();
        }
        return s;
    }
    if rest.len() < s.len() { rest } else { s }
}

/// Detect a Rust declaration on a (comment/string-stripped, trimmed) line.
/// `parent_kind` is the enclosing entry's kind, used to suppress items that
/// are local detail rather than structure (e.g. `const` inside a `fn`).
fn rust_decl(trimmed: &str, parent_kind: Option<&str>) -> Option<(String, String)> {
    let s = strip_vis(trimmed);
    let inside_fn = parent_kind == Some("fn");

    // fn, with any qualifier prefix.
    {
        let mut t = s;
        loop {
            let next = ["default ", "const ", "async ", "unsafe "]
                .iter()
                .find_map(|q| t.strip_prefix(q));
            if let Some(rest) = next {
                t = rest.trim_start();
                continue;
            }
            if let Some(rest) = t.strip_prefix("extern") {
                let rest = rest.trim_start();
                if let Some(r) = rest.strip_prefix('"')
                    && let Some(close) = r.find('"')
                {
                    t = r[close + 1..].trim_start();
                    continue;
                }
            }
            break;
        }
        if let Some(rest) = t.strip_prefix("fn ")
            && let Some(name) = ident(rest.trim_start())
        {
            return Some(("fn".into(), name.into()));
        }
    }

    for (kw, kind) in [
        ("mod ", "mod"),
        ("struct ", "struct"),
        ("enum ", "enum"),
        ("trait ", "trait"),
        ("macro_rules! ", "macro"),
    ] {
        if let Some(rest) = s.strip_prefix(kw)
            && let Some(name) = ident(rest.trim_start())
        {
            return Some((kind.into(), name.into()));
        }
    }
    if let Some(rest) = s.strip_prefix("unsafe ")
        && let Some(rest) = rest.trim_start().strip_prefix("trait ")
        && let Some(name) = ident(rest.trim_start())
    {
        return Some(("trait".into(), name.into()));
    }

    if s == "impl" || s.starts_with("impl ") || s.starts_with("impl<") {
        let mut rest = &s[4..];
        if let Some(r) = rest.strip_prefix('<') {
            // Skip the generic parameter list to the matching '>'.
            let mut depth = 1usize;
            let mut idx = None;
            for (i, c) in r.char_indices() {
                match c {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            idx = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            rest = idx.map(|i| &r[i + 1..]).unwrap_or("");
        }
        let mut name = rest.trim();
        if let Some(cut) = name.find('{') {
            name = name[..cut].trim();
        }
        if let Some(cut) = name.find(" where") {
            name = name[..cut].trim();
        }
        if !name.is_empty() {
            return Some(("impl".into(), name.into()));
        }
    }

    if !inside_fn {
        if let Some(rest) = s.strip_prefix("type ")
            && let Some(name) = ident(rest.trim_start())
        {
            return Some(("type".into(), name.into()));
        }
        if let Some(rest) = s.strip_prefix("const ")
            && let Some(name) = ident(rest.trim_start())
        {
            return Some(("const".into(), name.into()));
        }
        if let Some(rest) = s.strip_prefix("static ") {
            let rest = rest.trim_start();
            let rest = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
            if let Some(name) = ident(rest) {
                return Some(("static".into(), name.into()));
            }
        }
    }
    None
}

/// From the declaration's line, derive the block end: the line where the
/// first `{` after the declaration is balanced, or `start` itself when a `;`
/// terminates first. `None` when neither resolves within sight (20 lines of
/// lookahead for the opener) or the block never closes.
fn brace_block_end(lines: &[&str], start_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut opened = false;
    for (j, line) in lines.iter().enumerate().skip(start_idx) {
        for c in line.chars() {
            match c {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => {
                    depth = depth.saturating_sub(1);
                    if opened && depth == 0 {
                        return Some(j + 1);
                    }
                }
                ';' if !opened => return Some(start_idx + 1),
                _ => {}
            }
        }
        if !opened && j >= start_idx + 20 {
            return None;
        }
    }
    None
}

fn rust_outline(text: &str) -> Vec<Entry> {
    let stripped = strip_rust(text);
    let lines: Vec<&str> = stripped.lines().collect();
    let mut entries: Vec<Entry> = Vec::new();
    // Stack of (kind, end_line) for open enclosing entries.
    let mut stack: Vec<(String, Option<usize>)> = Vec::new();

    for (i, raw) in lines.iter().enumerate() {
        let line_no = i + 1;
        while let Some((_, Some(end))) = stack.last() {
            if *end < line_no {
                stack.pop();
            } else {
                break;
            }
        }
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue; // attributes / blanks
        }
        let parent_kind = stack.last().map(|(k, _)| k.as_str());
        if let Some((kind, name)) = rust_decl(trimmed, parent_kind) {
            let end = brace_block_end(&lines, i);
            entries.push(Entry {
                kind: kind.clone(),
                name,
                start: line_no,
                end,
                depth: stack.len() + 1,
            });
            // Only block-owners enclose later lines.
            if end.is_some_and(|e| e > line_no) {
                stack.push((kind, end));
            }
        }
    }
    entries
}

// ----- Python -------------------------------------------------------------------

/// Leading indentation in columns (tab = 4).
fn indent_cols(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

fn python_outline(text: &str) -> Vec<Entry> {
    let lines: Vec<&str> = text.lines().collect();
    let mut entries: Vec<Entry> = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (indent, entry index)
    let mut in_triple: Option<&str> = None;

    for (i, line) in lines.iter().enumerate() {
        let content = line.trim();
        // Rough triple-quoted string tracking so docstring text cannot
        // masquerade as declarations.
        if let Some(q) = in_triple {
            if content.contains(q) {
                in_triple = None;
            }
            continue;
        }
        for q in ["\"\"\"", "'''"] {
            if content.starts_with(q) && !content[q.len()..].contains(q) {
                in_triple = Some(q);
            }
        }
        if in_triple.is_some() || content.is_empty() || content.starts_with('#') {
            continue;
        }

        let decl = content
            .strip_prefix("async def ")
            .or_else(|| content.strip_prefix("def "))
            .map(|rest| ("def", rest))
            .or_else(|| content.strip_prefix("class ").map(|rest| ("class", rest)));
        let Some((kind, rest)) = decl else { continue };
        let Some(name) = ident(rest.trim_start()) else {
            continue;
        };

        let indent = indent_cols(line);
        while let Some((top_indent, _)) = stack.last() {
            if *top_indent >= indent {
                stack.pop();
            } else {
                break;
            }
        }

        // End: the last content line indented deeper than the declaration.
        let mut end = i + 1;
        for (j, later) in lines.iter().enumerate().skip(i + 1) {
            let t = later.trim();
            if t.is_empty() {
                continue;
            }
            if indent_cols(later) <= indent {
                break;
            }
            end = j + 1;
        }

        entries.push(Entry {
            kind: kind.into(),
            name: name.into(),
            start: i + 1,
            end: Some(end),
            depth: stack.len() + 1,
        });
        stack.push((indent, entries.len() - 1));
    }
    entries
}

// ----- Markdown -----------------------------------------------------------------

fn markdown_outline(text: &str) -> Vec<Entry> {
    let lines: Vec<&str> = text.lines().collect();
    let mut headings: Vec<(usize, usize, String)> = Vec::new(); // (level, line, title)
    let mut in_fence = false;

    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let level = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&level)
            && let Some(rest) = t.get(level..)
            && rest.starts_with(' ')
        {
            let title = rest.trim().trim_end_matches('#').trim().to_string();
            headings.push((level, i + 1, title));
        }
    }

    let total = lines.len();
    headings
        .iter()
        .enumerate()
        .map(|(idx, (level, start, title))| {
            // The section runs to the line before the next heading of the
            // same or a shallower level.
            let end = headings[idx + 1..]
                .iter()
                .find(|(l, ..)| l <= level)
                .map(|(_, s, _)| s - 1)
                .unwrap_or(total);
            Entry {
                kind: format!("h{level}"),
                name: title.clone(),
                start: *start,
                end: Some(end.max(*start)),
                depth: *level,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(e: &Entry) -> String {
        format!(
            "{}:{} {} {} d{}",
            e.start,
            e.end.map_or("?".to_string(), |n| n.to_string()),
            e.kind,
            e.name,
            e.depth
        )
    }

    #[test]
    fn rust_detects_kinds_spans_and_nesting() {
        let src = r#"
pub mod inner;

/// Doc with a brace { that must not count.
pub struct Point {
    x: i32,
}

impl Display for Point {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let brace = "{"; // string brace must not count
        write!(f, "{}", self.x)
    }
}

pub(crate) async fn fetch() {}

macro_rules! declare {
    () => {};
}
"#;
        let rows: Vec<String> = rust_outline(src).iter().map(row).collect();
        assert_eq!(
            rows,
            [
                "2:2 mod inner d1",
                "5:7 struct Point d1",
                "9:14 impl Display for Point d1",
                "10:13 fn fmt d2",
                "16:16 fn fetch d1",
                "18:20 macro declare d1",
            ]
        );
    }

    #[test]
    fn rust_suppresses_locals_inside_fn_but_keeps_assoc_items() {
        let src = "impl Foo {\n    const MAX: u8 = 1;\n    fn go() {\n        const LOCAL: u8 = 2;\n    }\n}\n";
        let rows: Vec<String> = rust_outline(src).iter().map(row).collect();
        assert_eq!(
            rows,
            ["1:6 impl Foo d1", "2:2 const MAX d2", "3:5 fn go d2"]
        );
    }

    #[test]
    fn rust_unclosed_block_yields_unknown_end() {
        let src = "fn broken() {\n    let x = 1;\n";
        let e = &rust_outline(src)[0];
        assert_eq!((e.start, e.end), (1, None));
    }

    #[test]
    fn python_indentation_nesting_and_spans() {
        let src = "class A:\n    def m(self):\n        pass\n\n    async def n(self):\n        pass\n\ndef top():\n    pass\n";
        let rows: Vec<String> = python_outline(src).iter().map(row).collect();
        assert_eq!(
            rows,
            [
                "1:6 class A d1",
                "2:3 def m d2",
                "5:6 def n d2",
                "8:9 def top d1",
            ]
        );
    }

    #[test]
    fn python_docstring_text_is_not_a_declaration() {
        let src = "def real():\n    \"\"\"\n    def fake():\n    \"\"\"\n    pass\n";
        let rows: Vec<String> = python_outline(src).iter().map(row).collect();
        assert_eq!(rows, ["1:5 def real d1"]);
    }

    #[test]
    fn markdown_headings_with_fenced_code_ignored() {
        let src = "# Title\n\nintro\n\n```sh\n# not a heading\n```\n\n## Section A\n\nbody\n\n## Section B\n";
        let rows: Vec<String> = markdown_outline(src).iter().map(row).collect();
        assert_eq!(
            rows,
            [
                "1:13 h1 Title d1",
                "9:12 h2 Section A d2",
                "13:13 h2 Section B d2"
            ]
        );
    }

    #[test]
    fn language_keying_is_extension_based() {
        assert_eq!(language_for("RS"), Some(Lang::Rust));
        assert_eq!(language_for("txt"), None);
    }
}
