// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-json`'s query engine: a jq filter (the jaq dialect, via `jaq-core`)
//! compiled once and run over parsed JSON inputs, plus the bridge from
//! `ct-patch`'s node-path grammar to an equivalent filter, so a `--path` an
//! agent learned for `ct patch` reads the same node in `ct json`.
//!
//! Documents parse through the same lenient `jsonc-parser` tree `ct-patch`
//! edits (so JSON, JSONC and each JSONL record all read, comments and trailing
//! commas included) straight into jaq's [`Val`], **keeping key order** as jq
//! does. Results come back out as compact JSON text (jaq's own writer, so
//! numbers print as jq prints them) or as a [`serde_json::Value`] for the
//! suite's `--json` framing (whose writer sorts object keys).

use jaq_core::load::{Arena, File, Loader, lex, parse};
use jaq_core::{Ctx, Vars, data};
use jaq_json::Val;
use jsonc_parser::ast::{ObjectPropName, Value};
use jsonc_parser::{CollectOptions, ParseOptions, parse_to_ast};

use crate::patch::{Seg, normalize_value, parse_path};

/// A compiled jq filter.
pub struct Program {
    filter: jaq_core::Filter<data::JustLut<Val>>,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Program(..)")
    }
}

impl Program {
    /// Compile `code` with the jaq standard library loaded and the global
    /// variables `vars` (names without the `$`) declared, in the order their
    /// values will be supplied to [`Program::run`].
    ///
    /// # Examples
    ///
    /// ```
    /// use coding_tools::jsonq::{Program, from_json};
    ///
    /// let p = Program::compile(".[] | select(. > $min)", &["min".to_string()]).unwrap();
    /// let input = from_json(serde_json::json!([1, 5, 10])).unwrap();
    /// let out: Vec<String> = p
    ///     .run(input, vec![from_json(serde_json::json!(3)).unwrap()])
    ///     .map(|r| r.unwrap().to_string())
    ///     .collect();
    /// assert_eq!(out, ["5", "10"]);
    /// assert!(Program::compile(".[", &[]).is_err());
    /// assert!(Program::compile("nosuchfilter", &[]).unwrap_err().contains("undefined filter"));
    /// ```
    pub fn compile(code: &str, vars: &[String]) -> Result<Program, String> {
        let defs = jaq_core::defs()
            .chain(jaq_std::defs())
            .chain(jaq_json::defs());
        let funs = jaq_core::funs()
            .chain(jaq_std::funs())
            .chain(jaq_json::funs());
        let names: Vec<String> = vars.iter().map(|v| format!("${v}")).collect();
        let loader = Loader::new(defs);
        let arena = Arena::default();
        let modules = loader
            .load(&arena, File { code, path: () })
            .map_err(|errs| load_errors(code, errs))?;
        let filter = jaq_core::Compiler::default()
            .with_funs(funs)
            .with_global_vars(names.iter().map(String::as_str))
            .compile(modules)
            .map_err(|errs| compile_errors(code, errs))?;
        Ok(Program { filter })
    }

    /// Run the filter over one input, with the global variables' values in
    /// declaration order. Each item is one output value, or the filter's own
    /// error text (a `halt` is reported as an error too: the suite's exit
    /// codes are not the filter's to choose).
    pub fn run<'a>(
        &'a self,
        input: Val,
        vars: Vec<Val>,
    ) -> impl Iterator<Item = Result<Val, String>> + 'a {
        let ctx = Ctx::<data::JustLut<Val>>::new(&self.filter.lut, Vars::new(vars));
        self.filter.id.run((ctx, input)).map(|r| {
            r.map_err(|exn| match exn.get_err() {
                Ok(err) => err.to_string(),
                Err(exn) => match exn.get_halt() {
                    Ok(code) => format!("filter halted with code {code}"),
                    Err(_) => "filter broke out of an unknown label".to_string(),
                },
            })
        })
    }
}

/// Byte offset of `part` within `code`, when it really is a sub-slice.
fn offset_in(code: &str, part: &str) -> Option<usize> {
    let off = (part.as_ptr() as usize).checked_sub(code.as_ptr() as usize)?;
    (off + part.len() <= code.len()).then_some(off)
}

/// `expected X at offset N, found 'tok'` — one lex/parse complaint, located.
fn expected(code: &str, what: &str, found: &str) -> String {
    match offset_in(code, found) {
        Some(off) if found.is_empty() => {
            format!("expected {what} at offset {off} (end of filter)")
        }
        Some(off) => {
            let tok: String = found.chars().take(24).collect();
            format!("expected {what} at offset {off}, found '{tok}'")
        }
        None => format!("expected {what}"),
    }
}

fn load_errors(code: &str, errs: jaq_core::load::Errors<&str, ()>) -> String {
    let mut msgs = Vec::new();
    for (_, err) in errs {
        match err {
            jaq_core::load::Error::Io(es) => {
                msgs.extend(es.into_iter().map(|(p, e)| format!("cannot load {p}: {e}")));
            }
            jaq_core::load::Error::Lex(es) => msgs.extend(es.into_iter().map(|(exp, found)| {
                let what = match &exp {
                    lex::Expect::Delim(d) => format!("the closing delimiter for '{d}'"),
                    e => e.as_str().to_string(),
                };
                expected(code, &what, found)
            })),
            jaq_core::load::Error::Parse(es) => msgs.extend(es.into_iter().map(|(exp, found)| {
                let what = match &exp {
                    parse::Expect::Just(s) => format!("'{s}'"),
                    e => e.as_str().to_string(),
                };
                expected(code, &what, found)
            })),
        }
    }
    msgs.join("; ")
}

fn compile_errors(code: &str, errs: jaq_core::compile::Errors<&str, ()>) -> String {
    let mut msgs = Vec::new();
    for (_, es) in errs {
        for (name, undef) in es {
            let kind = undef.as_str();
            msgs.push(match offset_in(code, name) {
                Some(off) => format!("undefined {kind} '{name}' at offset {off}"),
                None => format!("undefined {kind} '{name}'"),
            });
        }
    }
    msgs.join("; ")
}

/// Parse one JSON-family document (comments and trailing commas tolerated,
/// as `ct-patch` reads them) into a jaq value, keeping object keys in
/// document order. `Ok(None)` is an empty document (only whitespace and
/// comments). A duplicated key keeps its last value, as jq does.
///
/// # Examples
///
/// ```
/// use coding_tools::jsonq::parse_document;
///
/// let v = parse_document("{ // note\n  \"z\": 1, \"a\": [true, null, 1.5e3,], }").unwrap().unwrap();
/// assert_eq!(v.to_string(), r#"{"z":1,"a":[true,null,1.5e3]}"#);
/// assert!(parse_document("  // nothing here\n").unwrap().is_none());
/// assert!(parse_document("{").is_err());
/// ```
pub fn parse_document(text: &str) -> Result<Option<Val>, String> {
    let parsed = parse_to_ast(
        text,
        &CollectOptions {
            comments: jsonc_parser::CommentCollectionStrategy::Off,
            tokens: false,
        },
        &ParseOptions::default(),
    )
    .map_err(|e| e.to_string())?;
    parsed.value.as_ref().map(ast_to_val).transpose()
}

fn ast_to_val(node: &Value<'_>) -> Result<Val, String> {
    Ok(match node {
        Value::NullKeyword(_) => Val::Null,
        Value::BooleanLit(b) => Val::Bool(b.value),
        Value::NumberLit(n) => parse_json(n.value)?,
        Value::StringLit(s) => Val::from(s.value.to_string()),
        Value::Array(a) => a
            .elements
            .iter()
            .map(ast_to_val)
            .collect::<Result<Vec<Val>, String>>()?
            .into_iter()
            .collect(),
        Value::Object(o) => {
            let mut map = jaq_json::Map::default();
            for p in &o.properties {
                let key = match &p.name {
                    ObjectPropName::String(s) => s.value.to_string(),
                    ObjectPropName::Word(w) => w.value.to_string(),
                };
                map.insert(Val::from(key), ast_to_val(&p.value)?);
            }
            Val::obj(map)
        }
    })
}

/// Parse exactly one strict JSON value (an `--argjson` value, a number
/// literal) with jaq's own reader, keeping key order.
///
/// # Examples
///
/// ```
/// use coding_tools::jsonq::parse_json;
///
/// assert_eq!(parse_json(r#"{"b": 1, "a": 2}"#).unwrap().to_string(), r#"{"b":1,"a":2}"#);
/// assert!(parse_json("{nope}").is_err());
/// assert!(parse_json("1 2").is_err());
/// ```
pub fn parse_json(text: &str) -> Result<Val, String> {
    jaq_json::read::parse_single(text.as_bytes()).map_err(|e| e.to_string())
}

/// A JSON value as a jaq value (object keys arrive in `serde_json`'s order).
///
/// # Examples
///
/// ```
/// use coding_tools::jsonq::from_json;
///
/// let v = from_json(serde_json::json!({"a": [1, "x", null]})).unwrap();
/// assert_eq!(v.to_string(), r#"{"a":[1,"x",null]}"#);
/// ```
pub fn from_json(v: serde_json::Value) -> Result<Val, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

/// A jaq value as a JSON value, for the suite's structured output. jaq can
/// yield values JSON cannot carry (`infinite`); those come back as their jq
/// text in a string rather than being silently rewritten.
///
/// # Examples
///
/// ```
/// use coding_tools::jsonq::{from_json, to_json};
///
/// let v = from_json(serde_json::json!([1, {"b": true}])).unwrap();
/// assert_eq!(to_json(&v), serde_json::json!([1, {"b": true}]));
/// ```
pub fn to_json(v: &Val) -> serde_json::Value {
    let text = v.to_string();
    serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
}

/// Render one output value as text: `raw` prints a string's own characters
/// (jq's `-r`); everything else is JSON, compact on one line or indented
/// (two spaces) when `pretty`.
///
/// # Examples
///
/// ```
/// use coding_tools::jsonq::{from_json, render};
///
/// let s = from_json(serde_json::json!("a\"b")).unwrap();
/// assert_eq!(render(&s, true, false), "a\"b");
/// assert_eq!(render(&s, false, false), r#""a\"b""#);
/// let o = from_json(serde_json::json!({"k": [1]})).unwrap();
/// assert_eq!(render(&o, false, true), "{\n  \"k\": [\n    1\n  ]\n}");
/// ```
pub fn render(v: &Val, raw: bool, pretty: bool) -> String {
    if raw && let Val::TStr(b) = v {
        return String::from_utf8_lossy(&b[..]).into_owned();
    }
    if !pretty {
        return v.to_string();
    }
    let pp = jaq_json::write::Pp {
        indent: Some("  ".to_string()),
        sep_space: true,
        ..Default::default()
    };
    let mut buf = Vec::new();
    match jaq_json::write::write(&mut buf, &pp, 0, v) {
        Ok(()) => String::from_utf8_lossy(&buf).into_owned(),
        Err(_) => v.to_string(),
    }
}

/// Translate a `ct-patch` node path into the jq filter that reads the same
/// node: keys and `[N]` index, `[key=value]` picks the **first** array element
/// whose `key` equals `value` (parsed as JSON when it is one, else a string —
/// the same promotion `ct-patch` applies to a `--set` value), and `[*]`, which
/// `ct-patch` does not take, iterates every element. A leading `.` is optional.
///
/// # Examples
///
/// ```
/// use coding_tools::jsonq::path_filter;
///
/// assert_eq!(path_filter("wires[0].to_node").unwrap(), r#".["wires"] | .[0] | .["to_node"]"#);
/// assert_eq!(
///     path_filter(".servers[name=web].port").unwrap(),
///     r#".["servers"] | first(.[] | select(.["name"] == "web")) | .["port"]"#
/// );
/// assert_eq!(path_filter("nodes[*].id").unwrap(), r#".["nodes"] | .[] | .["id"]"#);
/// assert_eq!(path_filter("[*]").unwrap(), ".[]");
/// assert!(path_filter("").is_err());
/// assert!(path_filter("a[x]").is_err());
/// ```
pub fn path_filter(spec: &str) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    for (i, piece) in spec.split("[*]").enumerate() {
        if i > 0 {
            parts.push(".[]".to_string());
        }
        let piece = piece.strip_prefix('.').unwrap_or(piece);
        if piece.is_empty() {
            continue;
        }
        for seg in parse_path(piece).map_err(|e| e.replace(piece, spec))? {
            parts.push(match seg {
                Seg::Key(k) => format!(".[{}]", serde_json::Value::String(k)),
                Seg::Index(n) => format!(".[{n}]"),
                Seg::Select { key, value } => format!(
                    "first(.[] | select(.[{}] == {}))",
                    serde_json::Value::String(key),
                    normalize_value(&value)
                ),
            });
        }
    }
    if parts.is_empty() {
        return Err(format!("empty path '{spec}'"));
    }
    Ok(parts.join(" | "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(code: &str, input: serde_json::Value) -> Vec<String> {
        let p = Program::compile(code, &[]).unwrap();
        p.run(from_json(input).unwrap(), vec![])
            .map(|r| r.unwrap().to_string())
            .collect()
    }

    #[test]
    fn standard_library_and_json_defs_are_loaded() {
        assert_eq!(
            run(
                r#"[.[] | .a] | map(. * 2) | add"#,
                serde_json::json!([{"a": 1}, {"a": 2}])
            ),
            ["6"]
        );
        assert_eq!(
            run(
                r#"to_entries | map("\(.key)=\(.value)") | join(",")"#,
                serde_json::json!({"x": 1, "y": "z"})
            ),
            [r#""x=1,y=z""#]
        );
        assert_eq!(
            run("tojson | fromjson | length", serde_json::json!([1, 2, 3])),
            ["3"]
        );
    }

    #[test]
    fn filter_errors_are_text_not_panics() {
        let p = Program::compile(".a", &[]).unwrap();
        let errs: Vec<_> = p
            .run(from_json(serde_json::json!(5)).unwrap(), vec![])
            .collect();
        assert_eq!(errs.len(), 1);
        let e = errs[0].as_ref().unwrap_err();
        assert!(e.contains("cannot index"), "{e}");
    }

    #[test]
    fn halt_is_reported_not_exited() {
        let p = Program::compile("halt", &[]).unwrap();
        let out: Vec<_> = p.run(Val::Null, vec![]).collect();
        assert_eq!(out.len(), 1);
        assert!(out[0].as_ref().unwrap_err().contains("halted"));
    }

    #[test]
    fn syntax_errors_name_the_offset() {
        let e = Program::compile(".a | ", &[]).unwrap_err();
        assert!(e.contains("expected"), "{e}");
        let e = Program::compile(".a | foo(1)", &[]).unwrap_err();
        assert!(e.contains("undefined filter 'foo'"), "{e}");
        assert!(e.contains("offset 5"), "{e}");
        let e = Program::compile("$nope", &[]).unwrap_err();
        assert!(e.contains("undefined variable"), "{e}");
    }

    #[test]
    fn variables_bind_in_declaration_order() {
        let p = Program::compile("[$a, $b]", &["a".into(), "b".into()]).unwrap();
        let out: Vec<String> = p
            .run(
                Val::Null,
                vec![Val::from("x".to_string()), Val::from(2isize)],
            )
            .map(|r| r.unwrap().to_string())
            .collect();
        assert_eq!(out, [r#"["x",2]"#]);
    }

    #[test]
    fn path_filter_reads_what_ct_patch_would_set() {
        let doc = serde_json::json!({
            "servers": [{"name": "web", "port": 80}, {"name": "db", "port": 5432}],
            "tags": ["a", "b"]
        });
        for (path, want) in [
            ("servers[name=db].port", vec!["5432"]),
            ("servers[port=80].name", vec![r#""web""#]),
            ("servers[1]", vec![r#"{"name":"db","port":5432}"#]),
            ("tags[*]", vec![r#""a""#, r#""b""#]),
            ("servers[*].name", vec![r#""web""#, r#""db""#]),
            ("servers[name=nope].port", vec![]),
        ] {
            assert_eq!(
                run(&path_filter(path).unwrap(), doc.clone()),
                want,
                "{path}"
            );
        }
    }

    #[test]
    fn documents_keep_key_order_and_tolerate_jsonc() {
        let v = parse_document("{\"z\": {\"y\": 1, \"x\": 2}, /* c */ \"a\": [3,],}")
            .unwrap()
            .unwrap();
        assert_eq!(v.to_string(), r#"{"z":{"y":1,"x":2},"a":[3]}"#);
        let p = Program::compile("keys_unsorted", &[]).unwrap();
        let out: Vec<String> = p.run(v, vec![]).map(|r| r.unwrap().to_string()).collect();
        assert_eq!(out, [r#"["z","a"]"#]);
        // A duplicated key keeps its last value, as jq does.
        let d = parse_document(r#"{"k": 1, "k": 2}"#).unwrap().unwrap();
        assert_eq!(d.to_string(), r#"{"k":2}"#);
    }

    #[test]
    fn render_modes() {
        let v = parse_json(r#"{"s": "x", "n": 1.5}"#).unwrap();
        assert_eq!(render(&v, true, false), r#"{"s":"x","n":1.5}"#);
        assert_eq!(
            render(&v, false, true),
            "{\n  \"s\": \"x\",\n  \"n\": 1.5\n}"
        );
        let s = parse_json(r#""two\nlines""#).unwrap();
        assert_eq!(render(&s, true, true), "two\nlines");
    }
}
