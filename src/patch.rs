// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-patch`'s structured-edit engine for JSON / JSONC / JSONL / YAML.
//!
//! It parses a dotted/bracketed node path (keys, `[N]` indices, `[key=value]`
//! object predicates), and applies [`Op`]erations preserving everything outside
//! the changed node. For the JSON family, edits are **byte-range splices** against
//! the `jsonc-parser` syntax tree (comments, indentation, key order, trailing
//! commas all preserved); [`apply_doc`] runs a sequence over one document and
//! [`apply_jsonl`] runs them over each line. For YAML, [`apply_yaml`] uses the
//! pure-Rust, comment-preserving `yaml-edit` backend (currently `--set`-replace
//! and `--delete`; `--add`/`--move-*` error, as yaml-edit 0.2 mis-indents inserts).

use jsonc_parser::ast::{Array, Object, ObjectPropName, Value};
use jsonc_parser::common::Ranged;
use jsonc_parser::{CollectOptions, ParseOptions, parse_to_ast};
use serde_json::json;
use yaml_edit::path::YamlPath;

/// A path segment: an object key, an array index, or a predicate selecting the
/// array element whose `key` equals `value` (`[key=value]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Index(usize),
    Select { key: String, value: String },
}

/// Parse a dotted/bracketed path; a leading `.` is optional. Keys are
/// dot-separated; `[N]` selects an array index and `[key=value]` selects the
/// object in an array whose `key` equals `value`.
///
/// # Examples
///
/// ```
/// use coding_tools::patch::{parse_path, Seg};
///
/// assert_eq!(
///     parse_path("a[2].c").unwrap(),
///     vec![Seg::Key("a".into()), Seg::Index(2), Seg::Key("c".into())]
/// );
/// assert_eq!(
///     parse_path(".servers[name=web].port").unwrap(),
///     vec![
///         Seg::Key("servers".into()),
///         Seg::Select { key: "name".into(), value: "web".into() },
///         Seg::Key("port".into()),
///     ]
/// );
/// assert!(parse_path("").is_err());
/// assert!(parse_path("a[x]").is_err()); // not an index and not key=value
/// ```
pub fn parse_path(spec: &str) -> Result<Vec<Seg>, String> {
    let body = spec.strip_prefix('.').unwrap_or(spec);
    if body.is_empty() {
        return Err(format!("empty path '{spec}'"));
    }
    let mut segs = Vec::new();
    for part in body.split('.') {
        let mut rest = part;
        match rest.find('[') {
            Some(br) => {
                let key = &rest[..br];
                if !key.is_empty() {
                    segs.push(Seg::Key(key.to_string()));
                }
                rest = &rest[br..];
                while let Some(stripped) = rest.strip_prefix('[') {
                    let close = stripped
                        .find(']')
                        .ok_or_else(|| format!("unclosed '[' in path '{spec}'"))?;
                    let inner = &stripped[..close];
                    if let Some((k, v)) = inner.split_once('=') {
                        segs.push(Seg::Select {
                            key: k.to_string(),
                            value: v.to_string(),
                        });
                    } else {
                        let idx: usize = inner
                            .parse()
                            .map_err(|_| format!("invalid index '[{inner}]' in path '{spec}'"))?;
                        segs.push(Seg::Index(idx));
                    }
                    rest = &stripped[close + 1..];
                }
                if !rest.is_empty() {
                    return Err(format!("trailing characters '{rest}' in path '{spec}'"));
                }
            }
            None => {
                if rest.is_empty() {
                    return Err(format!("empty segment in path '{spec}'"));
                }
                segs.push(Seg::Key(rest.to_string()));
            }
        }
    }
    Ok(segs)
}

/// Split a `PATH=VALUE` spec at the first `=` that is *outside* any `[...]`, so a
/// predicate like `.a[name=x].b=1` splits into (`.a[name=x].b`, `1`).
///
/// # Examples
///
/// ```
/// use coding_tools::patch::split_assign;
///
/// assert_eq!(
///     split_assign(".servers[name=web].port=8443"),
///     Some((".servers[name=web].port", "8443"))
/// );
/// assert_eq!(split_assign(".x=hi"), Some((".x", "hi")));
/// assert_eq!(split_assign(".x"), None);
/// ```
pub fn split_assign(spec: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    for (i, c) in spec.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = (depth - 1).max(0),
            '=' if depth == 0 => return Some((&spec[..i], &spec[i + 1..])),
            _ => {}
        }
    }
    None
}

/// Normalise a `--set`/`--add` value: valid JSON is kept (compact); anything else
/// is taken as a JSON string.
///
/// # Examples
///
/// ```
/// use coding_tools::patch::normalize_value;
///
/// assert_eq!(normalize_value("8080"), "8080");           // a JSON number
/// assert_eq!(normalize_value("true"), "true");           // a JSON bool
/// assert_eq!(normalize_value("[1,2]"), "[1,2]");         // a JSON array
/// assert_eq!(normalize_value("hello"), "\"hello\"");     // not JSON -> a string
/// ```
pub fn normalize_value(v: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(v) {
        Ok(parsed) => parsed.to_string(),
        Err(_) => serde_json::Value::String(v.to_string()).to_string(),
    }
}

/// Where a `--move-*` relocates an array element within its list.
#[derive(Debug, Clone, Copy)]
pub enum MoveTo {
    First,
    Last,
    Up,
    Down,
}

/// A single patch operation, with the raw path text kept for messages.
pub enum Op {
    Set {
        path: Vec<Seg>,
        raw: String,
        value: String,
    },
    Add {
        path: Vec<Seg>,
        raw: String,
        value: String,
    },
    Delete {
        path: Vec<Seg>,
        raw: String,
    },
    Move {
        path: Vec<Seg>,
        raw: String,
        to: MoveTo,
    },
}

/// The key string of an object property.
fn prop_key(name: &ObjectPropName) -> String {
    match name {
        ObjectPropName::String(s) => s.value.to_string(),
        ObjectPropName::Word(w) => w.value.to_string(),
    }
}

/// Whether a scalar value node equals the (unquoted) predicate `value` text.
fn scalar_eq(node: &Value, value: &str) -> bool {
    match node {
        Value::StringLit(s) => s.value.as_ref() == value,
        Value::NumberLit(n) => n.value == value,
        Value::BooleanLit(b) => (if b.value { "true" } else { "false" }) == value,
        Value::NullKeyword(_) => value == "null",
        _ => false,
    }
}

/// The index of the first array element that is an object with `key` == `value`.
fn select_index(arr: &Array, key: &str, value: &str) -> Option<usize> {
    arr.elements.iter().position(|e| match e {
        Value::Object(o) => o
            .properties
            .iter()
            .any(|p| prop_key(&p.name) == key && scalar_eq(&p.value, value)),
        _ => false,
    })
}

/// Resolve the final segment of a path to an array index within `parent`
/// (for `Index` and `Select`; `Key` is not an array position).
fn final_array_index(parent: &Value, last: &Seg, raw: &str) -> Result<usize, String> {
    let arr = as_array(parent, raw)?;
    match last {
        Seg::Index(i) => Ok(*i),
        Seg::Select { key, value } => select_index(arr, key, value)
            .ok_or_else(|| format!("path '{raw}': no array element where {key}={value}")),
        Seg::Key(_) => Err(format!(
            "path '{raw}': expected an array element, got an object key"
        )),
    }
}

/// The leading whitespace of the line containing byte offset `pos`.
fn line_indent(text: &str, pos: usize) -> String {
    let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    text[line_start..pos]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Replace `text[start..end]` with `repl`; report whether the text changed.
fn splice(text: &str, start: usize, end: usize, repl: &str) -> (String, bool) {
    let out = format!("{}{}{}", &text[..start], repl, &text[end..]);
    let changed = out != text;
    (out, changed)
}

/// Walk to the node at `segs` (all of them); `None` if any segment is absent.
fn navigate<'a>(root: &'a Value<'a>, segs: &[Seg]) -> Option<&'a Value<'a>> {
    let mut cur = root;
    for seg in segs {
        cur = match (seg, cur) {
            (Seg::Key(k), Value::Object(o)) => {
                &o.properties.iter().find(|p| prop_key(&p.name) == *k)?.value
            }
            (Seg::Index(i), Value::Array(a)) => a.elements.get(*i)?,
            (Seg::Select { key, value }, Value::Array(a)) => {
                a.elements.get(select_index(a, key, value)?)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

/// Append `"key": value` (or just `value` for arrays) into a container that
/// already has elements, matching its inline/multiline style and indentation.
fn append_into(
    text: &str,
    open: usize,
    first_child: usize,
    last_end: usize,
    entry: &str,
) -> (String, bool) {
    let multiline = text[open + 1..first_child].contains('\n');
    let insert = if multiline {
        format!(",\n{}{entry}", line_indent(text, first_child))
    } else {
        format!(", {entry}")
    };
    splice(text, last_end, last_end, &insert)
}

/// Apply `--set` at `path`.
fn set_in(
    text: &str,
    root: &Value,
    path: &[Seg],
    value: &str,
    raw: &str,
) -> Result<(String, bool), String> {
    let (last, parents) = path.split_last().expect("path is non-empty");
    let parent = navigate(root, parents)
        .ok_or_else(|| format!("path '{raw}' not found (a parent segment is missing)"))?;
    match last {
        Seg::Key(k) => {
            let obj = as_object(parent, raw)?;
            if let Some(p) = obj.properties.iter().find(|p| prop_key(&p.name) == *k) {
                let r = p.value.range();
                Ok(splice(text, r.start, r.end, value))
            } else {
                Ok(add_key(text, obj, k, value))
            }
        }
        Seg::Index(i) => {
            let arr = as_array(parent, raw)?;
            let len = arr.elements.len();
            if *i < len {
                let r = arr.elements[*i].range();
                Ok(splice(text, r.start, r.end, value))
            } else if *i == len {
                Ok(append_elem(text, arr, value))
            } else {
                Err(format!(
                    "path '{raw}': index {i} is out of range (length {len})"
                ))
            }
        }
        Seg::Select { key, value: want } => {
            let arr = as_array(parent, raw)?;
            let i = select_index(arr, key, want)
                .ok_or_else(|| format!("path '{raw}': no array element where {key}={want}"))?;
            let r = arr.elements[i].range();
            Ok(splice(text, r.start, r.end, value))
        }
    }
}

/// Add a new `"key": value` property to an object.
fn add_key(text: &str, obj: &Object, key: &str, value: &str) -> (String, bool) {
    let entry = format!("{}: {value}", json!(key));
    if obj.properties.is_empty() {
        let pos = obj.range().start + 1;
        return splice(text, pos, pos, &entry);
    }
    let first = obj.properties[0].range().start;
    let last_end = obj.properties.last().unwrap().range().end;
    append_into(text, obj.range().start, first, last_end, &entry)
}

/// Append a new element to an array.
fn append_elem(text: &str, arr: &Array, value: &str) -> (String, bool) {
    if arr.elements.is_empty() {
        let pos = arr.range().start + 1;
        return splice(text, pos, pos, value);
    }
    let first = arr.elements[0].range().start;
    let last_end = arr.elements.last().unwrap().range().end;
    append_into(text, arr.range().start, first, last_end, value)
}

/// Apply `--delete` at `path`. A path that does not resolve is a no-op.
fn delete_in(text: &str, root: &Value, path: &[Seg], raw: &str) -> Result<(String, bool), String> {
    let (last, parents) = path.split_last().expect("path is non-empty");
    let parent = match navigate(root, parents) {
        Some(p) => p,
        None => return Ok((text.to_string(), false)),
    };
    match last {
        Seg::Key(k) => {
            let obj = as_object(parent, raw)?;
            match obj.properties.iter().position(|p| prop_key(&p.name) == *k) {
                Some(idx) => Ok(delete_member(
                    text,
                    obj.range().start,
                    obj.range().end,
                    &obj.properties
                        .iter()
                        .map(|p| (p.range().start, p.range().end))
                        .collect::<Vec<_>>(),
                    idx,
                    "{}",
                )),
                None => Ok((text.to_string(), false)),
            }
        }
        Seg::Index(i) => {
            let arr = as_array(parent, raw)?;
            if *i < arr.elements.len() {
                Ok(delete_member(
                    text,
                    arr.range().start,
                    arr.range().end,
                    &arr.elements
                        .iter()
                        .map(|e| (e.range().start, e.range().end))
                        .collect::<Vec<_>>(),
                    *i,
                    "[]",
                ))
            } else {
                Ok((text.to_string(), false))
            }
        }
        Seg::Select { key, value: want } => {
            let arr = as_array(parent, raw)?;
            match select_index(arr, key, want) {
                Some(i) => Ok(delete_member(
                    text,
                    arr.range().start,
                    arr.range().end,
                    &arr.elements
                        .iter()
                        .map(|e| (e.range().start, e.range().end))
                        .collect::<Vec<_>>(),
                    i,
                    "[]",
                )),
                None => Ok((text.to_string(), false)),
            }
        }
    }
}

/// Remove member `idx` from a container, taking the adjacent comma with it so the
/// surrounding members stay well-formed. `empty` is the literal for "now empty".
fn delete_member(
    text: &str,
    open: usize,
    close: usize,
    members: &[(usize, usize)],
    idx: usize,
    empty: &str,
) -> (String, bool) {
    if members.len() == 1 {
        return splice(text, open, close, empty);
    }
    let (start, end) = if idx + 1 < members.len() {
        // Not last: take from this member's start up to the next member's start
        // (sweeps the separating comma and whitespace).
        (members[idx].0, members[idx + 1].0)
    } else {
        // Last: take from the previous member's end (sweeps the comma before it).
        (members[idx - 1].1, members[idx].1)
    };
    splice(text, start, end, "")
}

fn as_object<'a>(node: &'a Value<'a>, raw: &str) -> Result<&'a Object<'a>, String> {
    match node {
        Value::Object(o) => Ok(o),
        _ => Err(format!("path '{raw}': expected an object")),
    }
}

fn as_array<'a>(node: &'a Value<'a>, raw: &str) -> Result<&'a Array<'a>, String> {
    match node {
        Value::Array(a) => Ok(a),
        _ => Err(format!("path '{raw}': expected an array")),
    }
}

/// Apply `--add` at `path`: append `value` to the array there.
fn add_in(
    text: &str,
    root: &Value,
    path: &[Seg],
    value: &str,
    raw: &str,
) -> Result<(String, bool), String> {
    let node = navigate(root, path).ok_or_else(|| format!("path '{raw}' not found"))?;
    let arr = as_array(node, raw)?;
    Ok(append_elem(text, arr, value))
}

/// Apply `--move-*` at `path` (which selects an array element): relocate it
/// within its list, reordering element texts and keeping the separator style.
fn move_in(
    text: &str,
    root: &Value,
    path: &[Seg],
    to: MoveTo,
    raw: &str,
) -> Result<(String, bool), String> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| format!("empty path '{raw}'"))?;
    let parent = navigate(root, parents).ok_or_else(|| format!("path '{raw}' not found"))?;
    let arr = as_array(parent, raw)?;
    let i = final_array_index(parent, last, raw)?;
    let len = arr.elements.len();
    if i >= len {
        return Err(format!(
            "path '{raw}': index {i} is out of range (length {len})"
        ));
    }
    if len < 2 {
        return Ok((text.to_string(), false));
    }
    let j = match to {
        MoveTo::First => 0,
        MoveTo::Last => len - 1,
        MoveTo::Up => i.saturating_sub(1),
        MoveTo::Down => (i + 1).min(len - 1),
    };
    if i == j {
        return Ok((text.to_string(), false));
    }
    let spans: Vec<(usize, usize)> = arr
        .elements
        .iter()
        .map(|e| {
            let r = e.range();
            (r.start, r.end)
        })
        .collect();
    let items: Vec<&str> = spans.iter().map(|&(s, e)| &text[s..e]).collect();
    let mut order: Vec<usize> = (0..len).collect();
    let moved = order.remove(i);
    order.insert(j, moved);
    // The glue between the first two elements is the separator style to reuse.
    let sep = text[spans[0].1..spans[1].0].to_string();
    let reordered: Vec<&str> = order.iter().map(|&k| items[k]).collect();
    Ok(splice(
        text,
        spans[0].0,
        spans[len - 1].1,
        &reordered.join(&sep),
    ))
}

/// Apply one [`Op`] to a single JSON(C) document, returning the new text and
/// whether it changed.
pub fn apply_op(text: &str, op: &Op) -> Result<(String, bool), String> {
    let parsed = parse_to_ast(text, &CollectOptions::default(), &ParseOptions::default())
        .map_err(|e| format!("parse error: {e}"))?;
    let root = parsed.value.as_ref().ok_or("document is empty")?;
    match op {
        Op::Set { path, value, raw } => set_in(text, root, path, value, raw),
        Op::Add { path, value, raw } => add_in(text, root, path, value, raw),
        Op::Delete { path, raw } => delete_in(text, root, path, raw),
        Op::Move { path, to, raw } => move_in(text, root, path, *to, raw),
    }
}

/// Apply every op to a whole-document text (JSON/JSONC), in order. Returns the
/// new text and the number of ops that changed it.
///
/// Edits are byte-range splices, so untouched bytes — including comments — are
/// preserved exactly.
///
/// # Examples
///
/// ```
/// use coding_tools::patch::{apply_doc, parse_path, normalize_value, Op};
///
/// let set = Op::Set {
///     path: parse_path(".a").unwrap(),
///     raw: ".a".into(),
///     value: normalize_value("42"),
/// };
/// let (out, changes) =
///     apply_doc("{\n  \"a\": 1, // keep me\n  \"b\": 2\n}\n", &[set]).unwrap();
/// assert_eq!(changes, 1);
/// // Only the value changed; the comment and layout are preserved.
/// assert_eq!(out, "{\n  \"a\": 42, // keep me\n  \"b\": 2\n}\n");
/// ```
pub fn apply_doc(text: &str, ops: &[Op]) -> Result<(String, usize), String> {
    let mut cur = text.to_string();
    let mut changes = 0usize;
    for op in ops {
        let (next, changed) = apply_op(&cur, op)?;
        if changed {
            changes += 1;
        }
        cur = next;
    }
    Ok((cur, changes))
}

/// Apply every op to each non-blank line of a JSONL document.
pub fn apply_jsonl(text: &str, ops: &[Op]) -> Result<(String, usize), String> {
    let mut out = String::with_capacity(text.len());
    let mut changes = 0usize;
    for segment in text.split_inclusive('\n') {
        let (body, nl) = match segment.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (segment, ""),
        };
        if body.trim().is_empty() {
            out.push_str(segment);
            continue;
        }
        let (patched, n) = apply_doc(body, ops)?;
        changes += n;
        out.push_str(&patched);
        out.push_str(nl);
    }
    Ok((out, changes))
}

/// A path as plain mapping keys; errors if any segment is an array index or
/// predicate (the YAML backend addresses mapping key paths in this version).
fn key_path(path: &[Seg], raw: &str) -> Result<Vec<String>, String> {
    path.iter()
        .map(|s| match s {
            Seg::Key(k) => Ok(k.clone()),
            Seg::Index(_) => Err(format!(
                "array-index paths are not yet supported for YAML ('{raw}')"
            )),
            Seg::Select { .. } => Err(format!(
                "predicate paths are not yet supported for YAML ('{raw}')"
            )),
        })
        .collect()
}

/// The leading run of blank/`#`-comment lines. `yaml-edit` 0.2 drops the
/// document-leading comment block on round-trip, so we capture and re-attach it.
fn leading_trivia(text: &str) -> String {
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            end += line.len();
        } else {
            break;
        }
    }
    text[..end].to_string()
}

/// Set a value at a dotted key path with its correct YAML type (so `8080` is a
/// number, `true` a bool, `"x"` a string), via `yaml-edit`'s typed `AsYaml`.
fn yaml_set(doc: &yaml_edit::Document, dotted: &str, value: &str, raw: &str) -> Result<(), String> {
    // `value` is already normalized JSON, so it always parses.
    let parsed: serde_json::Value = serde_json::from_str(value)
        .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
    match parsed {
        serde_json::Value::Bool(b) => doc.set_path(dotted, b),
        serde_json::Value::String(s) => doc.set_path(dotted, s.as_str()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                doc.set_path(dotted, i);
            } else if let Some(u) = n.as_u64() {
                doc.set_path(dotted, u);
            } else {
                doc.set_path(dotted, n.as_f64().unwrap());
            }
        }
        serde_json::Value::Null => {
            return Err(format!(
                "path '{raw}': null values are not yet supported for YAML"
            ));
        }
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            return Err(format!(
                "path '{raw}': array/object values are not yet supported for YAML"
            ));
        }
    }
    Ok(())
}

/// Apply every op to a YAML document via the pure-Rust, comment-preserving
/// `yaml-edit` backend. Returns the new text and the number of ops that changed
/// it. Supports `--set` (replace an existing key) and `--delete`; `--add` and
/// `--move-*` (and array-index/predicate paths) error rather than risk malformed
/// output.
///
/// # Examples
///
/// ```
/// use coding_tools::patch::{apply_yaml, parse_path, normalize_value, Op};
///
/// let set = Op::Set {
///     path: parse_path(".server.port").unwrap(),
///     raw: ".server.port".into(),
///     value: normalize_value("9090"),
/// };
/// let yaml = "# cfg\nserver:\n  host: localhost   # inline\n  port: 8080\n";
/// let (out, changes) = apply_yaml(yaml, &[set]).unwrap();
/// assert_eq!(changes, 1);
/// assert!(out.contains("# cfg"));       // leading comment preserved
/// assert!(out.contains("port: 9090"));  // a number, not a quoted string
/// assert!(out.contains("# inline"));    // inline comment preserved
/// ```
pub fn apply_yaml(text: &str, ops: &[Op]) -> Result<(String, usize), String> {
    use std::str::FromStr;
    let doc = yaml_edit::Document::from_str(text).map_err(|e| format!("yaml parse error: {e}"))?;
    let mut changes = 0usize;
    for op in ops {
        let before = doc.to_string();
        match op {
            Op::Set { path, value, raw } => {
                let keys = key_path(path, raw)?;
                let dotted = keys.join(".");
                // Replace-only for now: yaml-edit's auto-create mis-indents new
                // keys, so creating keys is reserved for the insert verbs.
                if doc.get_path(&dotted).is_none() {
                    return Err(format!(
                        "path '{raw}': key does not exist (adding new YAML keys is handled by the insert verbs)"
                    ));
                }
                yaml_set(&doc, &dotted, value, raw)?;
            }
            Op::Delete { path, raw } => {
                let keys = key_path(path, raw)?;
                let (last, parents) = keys
                    .split_last()
                    .ok_or_else(|| format!("empty path '{raw}'"))?;
                let mut map = doc
                    .as_mapping()
                    .ok_or_else(|| format!("path '{raw}': document root is not a mapping"))?;
                for k in parents {
                    map = map
                        .get_mapping(k)
                        .ok_or_else(|| format!("path '{raw}': '{k}' is not a mapping"))?;
                }
                map.remove(last);
            }
            // yaml-edit 0.2 mis-indents structural inserts (producing invalid
            // YAML), so --add and --move-* are JSON-family only for now.
            Op::Add { raw, .. } => {
                return Err(format!("--add is not yet supported for YAML ('{raw}')"));
            }
            Op::Move { raw, .. } => {
                return Err(format!("--move-* is not yet supported for YAML ('{raw}')"));
            }
        }
        if doc.to_string() != before {
            changes += 1;
        }
    }
    // Re-attach the document-leading comment block if yaml-edit dropped it.
    let leading = leading_trivia(text);
    let mut out = doc.to_string();
    if !leading.is_empty() && !out.starts_with(&leading) {
        out = format!("{leading}{out}");
    }
    Ok((out, changes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(text: &str, path: &str, value: &str) -> (String, bool) {
        apply_op(
            text,
            &Op::Set {
                path: parse_path(path).unwrap(),
                raw: path.to_string(),
                value: normalize_value(value),
            },
        )
        .unwrap()
    }

    fn delete(text: &str, path: &str) -> (String, bool) {
        apply_op(
            text,
            &Op::Delete {
                path: parse_path(path).unwrap(),
                raw: path.to_string(),
            },
        )
        .unwrap()
    }

    fn add(text: &str, path: &str, value: &str) -> (String, bool) {
        apply_op(
            text,
            &Op::Add {
                path: parse_path(path).unwrap(),
                raw: path.to_string(),
                value: normalize_value(value),
            },
        )
        .unwrap()
    }

    fn move_el(text: &str, path: &str, to: MoveTo) -> (String, bool) {
        apply_op(
            text,
            &Op::Move {
                path: parse_path(path).unwrap(),
                raw: path.to_string(),
                to,
            },
        )
        .unwrap()
    }

    #[test]
    fn path_parsing() {
        assert_eq!(
            parse_path(".a.b").unwrap(),
            vec![Seg::Key("a".into()), Seg::Key("b".into())]
        );
        assert_eq!(parse_path("[0]").unwrap(), vec![Seg::Index(0)]);
        assert_eq!(
            parse_path("a[name=web].port").unwrap(),
            vec![
                Seg::Key("a".into()),
                Seg::Select {
                    key: "name".into(),
                    value: "web".into()
                },
                Seg::Key("port".into()),
            ]
        );
        assert!(parse_path("").is_err());
        assert!(parse_path("a[x]").is_err());
    }

    #[test]
    fn assign_splits_outside_brackets() {
        assert_eq!(split_assign(".a[n=b].c=1"), Some((".a[n=b].c", "1")));
        assert_eq!(split_assign(".x=hi"), Some((".x", "hi")));
        assert_eq!(split_assign(".x"), None);
    }

    #[test]
    fn value_normalisation() {
        assert_eq!(normalize_value("42"), "42");
        assert_eq!(normalize_value("name"), "\"name\"");
    }

    #[test]
    fn set_replaces_value_preserving_comments_and_layout() {
        let t = "{\n  \"a\": 1, // keep me\n  \"b\": 2\n}\n";
        let (out, changed) = set(t, ".a", "42");
        assert!(changed);
        assert_eq!(out, "{\n  \"a\": 42, // keep me\n  \"b\": 2\n}\n");
    }

    #[test]
    fn set_adds_missing_key_multiline_with_indent() {
        let (out, _) = set("{\n  \"a\": 1\n}\n", ".b", "true");
        assert_eq!(out, "{\n  \"a\": 1,\n  \"b\": true\n}\n");
    }

    #[test]
    fn set_string_fallback_quotes_value() {
        let (out, _) = set("{\"a\":1}", ".a", "hello");
        assert_eq!(out, "{\"a\":\"hello\"}");
    }

    #[test]
    fn nested_set_and_array_index() {
        let t = "{\n  \"x\": { \"y\": [10, 20, 30] }\n}\n";
        let (out, changed) = set(t, ".x.y[1]", "99");
        assert!(changed);
        assert_eq!(out, "{\n  \"x\": { \"y\": [10, 99, 30] }\n}\n");
    }

    #[test]
    fn append_array_element_via_index() {
        let (out, _) = set("[1, 2]", "[2]", "3"); // index == len appends
        assert_eq!(out, "[1, 2, 3]");
    }

    #[test]
    fn predicate_selects_object_in_array() {
        let t = "{ \"xs\": [ {\"n\":\"a\",\"v\":1}, {\"n\":\"b\",\"v\":2} ] }";
        let (out, changed) = set(t, ".xs[n=b].v", "9");
        assert!(changed);
        assert_eq!(
            out,
            "{ \"xs\": [ {\"n\":\"a\",\"v\":1}, {\"n\":\"b\",\"v\":9} ] }"
        );
        let (del, _) = delete(t, ".xs[n=a]");
        assert_eq!(del, "{ \"xs\": [ {\"n\":\"b\",\"v\":2} ] }");
    }

    #[test]
    fn delete_takes_its_comma() {
        assert_eq!(
            delete("{\"a\":1,\"b\":2,\"c\":3}", ".b").0,
            "{\"a\":1,\"c\":3}"
        );
        assert_eq!(delete("{\"a\":1,\"b\":2}", ".b").0, "{\"a\":1}");
        assert_eq!(delete("{ \"a\": 1 }", ".a").0, "{}");
        assert!(!delete("{\"a\":1}", ".z").1); // missing key is a no-op
    }

    #[test]
    fn add_appends_without_index() {
        let (out, changed) = add("{\"xs\": [1, 2]}", ".xs", "3");
        assert!(changed);
        assert_eq!(out, "{\"xs\": [1, 2, 3]}");
    }

    #[test]
    fn move_reorders_preserving_separator() {
        assert_eq!(
            move_el("{\"xs\": [1, 2, 3]}", ".xs[0]", MoveTo::Last).0,
            "{\"xs\": [2, 3, 1]}"
        );
        assert_eq!(
            move_el("{\"xs\": [1, 2, 3]}", ".xs[2]", MoveTo::Up).0,
            "{\"xs\": [1, 3, 2]}"
        );
        assert!(!move_el("{\"xs\": [1]}", ".xs[0]", MoveTo::Last).1); // single-element no-op
    }

    fn yaml_op_set(path: &str, value: &str) -> Op {
        Op::Set {
            path: parse_path(path).unwrap(),
            raw: path.to_string(),
            value: normalize_value(value),
        }
    }

    #[test]
    fn yaml_set_replace_and_delete_preserve_comments() {
        let yaml = "# top\nserver:\n  host: localhost   # inline\n  port: 8080\n  debug: true\n";
        let del = Op::Delete {
            path: parse_path(".server.debug").unwrap(),
            raw: ".server.debug".to_string(),
        };
        let (out, changes) = apply_yaml(yaml, &[yaml_op_set(".server.port", "9090"), del]).unwrap();
        assert_eq!(changes, 2);
        assert!(out.contains("# top"), "leading comment kept: {out:?}");
        assert!(out.contains("port: 9090"), "number not quoted: {out:?}");
        assert!(out.contains("# inline"), "inline comment kept: {out:?}");
        assert!(!out.contains("debug:"), "debug deleted: {out:?}");
    }

    #[test]
    fn yaml_add_and_predicate_paths_error() {
        let add = Op::Add {
            path: parse_path(".server.tags").unwrap(),
            raw: ".server.tags".to_string(),
            value: normalize_value("x"),
        };
        let e = apply_yaml("server:\n  tags:\n    - a\n", &[add]).unwrap_err();
        assert!(e.contains("not yet supported for YAML"), "{e}");

        let pred = apply_yaml("xs: []\n", &[yaml_op_set(".xs[n=a].v", "1")]).unwrap_err();
        assert!(
            pred.contains("predicate paths are not yet supported"),
            "{pred}"
        );
    }
}
