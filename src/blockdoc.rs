// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The ct block document (`.ctb`) — the suite's one script format for batched
//! operations whose payloads are verbatim text.
//!
//! Delimited text, not JSON: payloads are code, and code must paste in with
//! zero escaping. A **fence line** starts with the fence string (`#%` by
//! default). A fence whose name the consuming tool declared as item-opening
//! (`edit` for `ct-edit`, `set`/`delete` for `ct-patch` batches) opens an
//! item and carries `key=value` attributes; other fence names open verbatim
//! **payload sections** of the current item; `end` closes the item. Outside
//! items, blank lines and `#`-comment lines are ignored.
//!
//! Inside a section every line is payload — including a blank line left
//! before the next fence to space the script out. A document can opt out of
//! that with the pragma `#% options trailing-blank=strip` (anywhere between
//! items, in force from there on; `keep` is the default and restores it):
//! blank lines at the end of a section closed by the next fence, or by the
//! end of the file, are then dropped as separators. A section closed by an
//! explicit `#% end` keeps them, so a payload that really ends in a blank
//! line is still expressible under `strip`.
//!
//! ```text
//! #% edit expect="=1" file=src/ast.rs
//! #% find
//!             Value::U64(v) => v.to_string(),
//! #% replace
//!             Value::U64(v) => v.to_string(),
//!             Value::I64(v) => v.to_string(),
//! #% end
//! ```

/// The default fence string opening every directive line.
pub const DEFAULT_FENCE: &str = "#%";

/// One parsed item: an opening directive, its attributes, and its payload
/// sections in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The item-opening directive name (`edit`, `set`, …).
    pub directive: String,
    /// `key=value` attributes from the opening fence line, in order.
    pub attrs: Vec<(String, String)>,
    /// Payload sections: (name, verbatim payload). Each payload line keeps
    /// its `\n`; an empty section is the empty string (zero lines).
    pub sections: Vec<(String, String)>,
    /// 1-based source line of the opening directive (for diagnostics).
    pub line: usize,
}

impl Item {
    /// The value of attribute `key`, if present.
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The payload of section `name`, if present.
    pub fn section(&self, name: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Parse attributes from the remainder of a directive line. Each token splits
/// at the *first* `=` (so `expect==1` is key `expect`, value `=1`); a value
/// may be double-quoted to carry spaces or read unambiguously
/// (`expect="=1"`).
fn parse_attrs(rest: &str, line_no: usize) -> Result<Vec<(String, String)>, String> {
    let mut attrs = Vec::new();
    let mut chars = rest.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        // Key: up to the first '='.
        let mut eq = None;
        for (i, c) in rest[start..].char_indices() {
            if c == '=' {
                eq = Some(start + i);
                break;
            }
            if c.is_whitespace() {
                break;
            }
        }
        let Some(eq) = eq else {
            return Err(format!(
                "line {line_no}: attribute '{}' is not key=value",
                rest[start..].split_whitespace().next().unwrap_or("")
            ));
        };
        let key = rest[start..eq].to_string();
        if key.is_empty() {
            return Err(format!("line {line_no}: attribute with empty key"));
        }
        // Value: double-quoted (to the closing quote) or bare (to whitespace).
        let vstart = eq + 1;
        let (value, after) = if rest[vstart..].starts_with('"') {
            match rest[vstart + 1..].find('"') {
                Some(close) => (
                    rest[vstart + 1..vstart + 1 + close].to_string(),
                    vstart + close + 2,
                ),
                None => {
                    return Err(format!(
                        "line {line_no}: unterminated quoted value for '{key}'"
                    ));
                }
            }
        } else {
            let end = rest[vstart..]
                .find(char::is_whitespace)
                .map(|i| vstart + i)
                .unwrap_or(rest.len());
            (rest[vstart..end].to_string(), end)
        };
        attrs.push((key, value));
        while let Some(&(i, _)) = chars.peek() {
            if i < after {
                chars.next();
            } else {
                break;
            }
        }
    }
    Ok(attrs)
}

/// Parse a block document. `item_names` declares which directive names open
/// an item; every other fence name is a payload section. An item-opening
/// directive implicitly closes the previous item, and `end` closes one
/// explicitly — so attribute-only items (`#% delete path=…`) need no `end`.
///
/// # Examples
///
/// ```
/// use coding_tools::blockdoc::{parse, DEFAULT_FENCE};
///
/// let doc = "#% edit expect=\"=1\"\n#% find\nold()\n#% replace\nnew()\n#% end\n";
/// let items = parse(doc, DEFAULT_FENCE, &["edit"]).unwrap();
/// assert_eq!(items.len(), 1);
/// assert_eq!(items[0].attr("expect"), Some("=1"));
/// assert_eq!(items[0].section("find"), Some("old()\n"));
/// ```
pub fn parse(src: &str, fence: &str, item_names: &[&str]) -> Result<Vec<Item>, String> {
    if fence.is_empty() {
        return Err("fence string must not be empty".to_string());
    }
    let mut items: Vec<Item> = Vec::new();
    let mut open: Option<Item> = None;
    let mut section: Option<(String, String)> = None;
    // `#% options trailing-blank=strip`: blank lines ending a section that the
    // next fence (or EOF) closes are separators, not payload.
    let mut strip_trailing = false;

    // `strip` is `strip_trailing` for a section the next fence or EOF closes,
    // and false for an explicit `#% end`, which keeps the section as written.
    let close_section = |item: &mut Item, section: &mut Option<(String, String)>, strip: bool| {
        if let Some((name, mut payload)) = section.take() {
            if strip {
                strip_trailing_blank_lines(&mut payload);
            }
            item.sections.push((name, payload));
        }
    };

    for (idx, raw) in src.lines().enumerate() {
        let line_no = idx + 1;
        let fenced = raw
            .strip_prefix(fence)
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace));
        let Some(rest) = fenced else {
            // Not a fence line: verbatim payload inside a section, ignorable
            // (blank or comment) outside any item.
            match (&mut open, &mut section) {
                (_, Some((_, payload))) => {
                    payload.push_str(raw);
                    payload.push('\n');
                }
                (Some(item), None) => {
                    if !raw.trim().is_empty() {
                        return Err(format!(
                            "line {line_no}: stray content inside '{}' item (line {}); payload lines belong in a section",
                            item.directive, item.line
                        ));
                    }
                }
                (None, _) => {
                    if !raw.trim().is_empty() && !raw.trim_start().starts_with('#') {
                        return Err(format!(
                            "line {line_no}: content outside any item; expected a '{fence} <directive>' line"
                        ));
                    }
                }
            }
            continue;
        };

        let rest = rest.trim_start();
        let (name, attr_rest) = match rest.find(char::is_whitespace) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if name.is_empty() {
            return Err(format!("line {line_no}: fence line with no directive name"));
        }

        if name == "end" {
            let Some(mut item) = open.take() else {
                return Err(format!("line {line_no}: 'end' with no open item"));
            };
            close_section(&mut item, &mut section, false);
            items.push(item);
        } else if name == OPTIONS_DIRECTIVE {
            if let Some(item) = &open {
                return Err(format!(
                    "line {line_no}: '{OPTIONS_DIRECTIVE}' must appear between items, not inside '{}' (line {})",
                    item.directive, item.line
                ));
            }
            for (key, value) in parse_attrs(attr_rest, line_no)? {
                match (key.as_str(), value.as_str()) {
                    ("trailing-blank", "strip") => strip_trailing = true,
                    ("trailing-blank", "keep") => strip_trailing = false,
                    ("trailing-blank", other) => {
                        return Err(format!(
                            "line {line_no}: trailing-blank={other} is not keep|strip"
                        ));
                    }
                    (other, _) => {
                        return Err(format!(
                            "line {line_no}: unknown option '{other}' (known: trailing-blank=keep|strip)"
                        ));
                    }
                }
            }
        } else if item_names.contains(&name) {
            if let Some(mut item) = open.take() {
                close_section(&mut item, &mut section, strip_trailing);
                items.push(item);
            }
            open = Some(Item {
                directive: name.to_string(),
                attrs: parse_attrs(attr_rest, line_no)?,
                sections: Vec::new(),
                line: line_no,
            });
        } else {
            let Some(item) = open.as_mut() else {
                return Err(format!(
                    "line {line_no}: unknown directive '{name}' (expected one of: {})",
                    item_names.join(", ")
                ));
            };
            if !attr_rest.trim().is_empty() {
                return Err(format!(
                    "line {line_no}: section '{name}' takes no attributes"
                ));
            }
            close_section(item, &mut section, strip_trailing);
            if item.section(name).is_some() {
                return Err(format!(
                    "line {line_no}: duplicate section '{name}' in '{}' item (line {})",
                    item.directive, item.line
                ));
            }
            section = Some((name.to_string(), String::new()));
        }
    }

    if let Some(mut item) = open.take() {
        close_section(&mut item, &mut section, strip_trailing);
        items.push(item);
    }
    Ok(items)
}

/// The reserved document-level directive carrying parser options.
pub const OPTIONS_DIRECTIVE: &str = "options";

/// Drop whole blank (empty or whitespace-only) lines from the end of a payload
/// whose lines each end in `\n`.
fn strip_trailing_blank_lines(payload: &mut String) {
    while let Some(body) = payload.strip_suffix('\n') {
        let start = body.rfind('\n').map_or(0, |i| i + 1);
        if body[start..].trim().is_empty() {
            payload.truncate(start);
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_blank_lines_are_payload_unless_options_say_strip() {
        // Default: the blank line before the next fence is part of `replace`.
        let doc = "#% edit\n#% find\na\n#% replace\nb\n\n#% edit\n#% find\nc\n\n  \n";
        let items = parse(doc, "#%", &["edit"]).unwrap();
        assert_eq!(items[0].section("replace"), Some("b\n\n"));
        assert_eq!(items[1].section("find"), Some("c\n\n  \n"));

        // strip: separators before a fence or EOF go; `#% end` keeps them.
        let doc = "#% options trailing-blank=strip\n\
                   #% edit\n#% find\na\n\n#% replace\nb\n\n\
                   #% edit\n#% find\nc\n\n#% end\n\
                   #% edit\n#% find\n\n#% replace\nd\n\n  \n";
        let items = parse(doc, "#%", &["edit"]).unwrap();
        assert_eq!(items[0].section("find"), Some("a\n"));
        assert_eq!(items[0].section("replace"), Some("b\n"));
        assert_eq!(
            items[1].section("find"),
            Some("c\n\n"),
            "explicit end is verbatim"
        );
        assert_eq!(
            items[2].section("find"),
            Some(""),
            "a blank-only section empties"
        );
        assert_eq!(items[2].section("replace"), Some("d\n"));

        // The pragma can be switched back, and is checked.
        let doc = "#% options trailing-blank=strip\n#% options trailing-blank=keep\n\
                   #% edit\n#% find\na\n\n";
        assert_eq!(
            parse(doc, "#%", &["edit"]).unwrap()[0].section("find"),
            Some("a\n\n")
        );
        let e = parse("#% options trailing-blank=maybe\n", "#%", &["edit"]).unwrap_err();
        assert!(e.contains("keep|strip"), "{e}");
        let e = parse("#% options eol=lf\n", "#%", &["edit"]).unwrap_err();
        assert!(e.contains("unknown option 'eol'"), "{e}");
        let e = parse(
            "#% edit\n#% options trailing-blank=strip\n",
            "#%",
            &["edit"],
        )
        .unwrap_err();
        assert!(e.contains("between items"), "{e}");
    }

    #[test]
    fn parses_items_attrs_and_verbatim_sections() {
        let doc = "\
# a comment outside items

#% edit expect==1 mode=literal file=src/a.rs
#% find
    old(\"$x\");
#% replace
    new(\"$x\");
    extra();
#% end
";
        let items = parse(doc, DEFAULT_FENCE, &["edit"]).unwrap();
        assert_eq!(items.len(), 1);
        let it = &items[0];
        assert_eq!(it.directive, "edit");
        // First-'=' split: expect==1 is key 'expect', value '=1'.
        assert_eq!(it.attr("expect"), Some("=1"));
        assert_eq!(it.attr("mode"), Some("literal"));
        assert_eq!(it.attr("file"), Some("src/a.rs"));
        assert_eq!(it.section("find"), Some("    old(\"$x\");\n"));
        assert_eq!(
            it.section("replace"),
            Some("    new(\"$x\");\n    extra();\n")
        );
        assert_eq!(it.line, 3);
    }

    #[test]
    fn quoted_values_carry_spaces_and_read_unambiguously() {
        let items = parse(
            "#% edit expect=\"=1\" note=\"two words\"\n#% find\nx\n#% end\n",
            DEFAULT_FENCE,
            &["edit"],
        )
        .unwrap();
        assert_eq!(items[0].attr("expect"), Some("=1"));
        assert_eq!(items[0].attr("note"), Some("two words"));
    }

    #[test]
    fn empty_section_is_zero_lines_and_end_is_implicit_between_items() {
        let doc = "#% edit\n#% find\nx\n#% replace\n#% edit\n#% find\ny\n#% replace\nz\n#% end\n";
        let items = parse(doc, DEFAULT_FENCE, &["edit"]).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].section("replace"), Some(""));
        assert_eq!(items[1].section("replace"), Some("z\n"));
    }

    #[test]
    fn custom_fence_lets_payloads_contain_the_default() {
        let doc = "::: edit\n::: find\n#% not a fence here\n::: replace\nok\n::: end\n";
        let items = parse(doc, ":::", &["edit"]).unwrap();
        assert_eq!(items[0].section("find"), Some("#% not a fence here\n"));
    }

    #[test]
    fn payload_lines_resembling_the_fence_prefix_are_fences() {
        // '#%x' is NOT a fence (no separator), so it stays payload.
        let doc = "#% edit\n#% find\n#%x payload\n#% end\n";
        let items = parse(doc, DEFAULT_FENCE, &["edit"]).unwrap();
        assert_eq!(items[0].section("find"), Some("#%x payload\n"));
    }

    #[test]
    fn errors_are_specific() {
        let unknown = parse("#% nonsense\n", DEFAULT_FENCE, &["edit"]).unwrap_err();
        assert!(unknown.contains("unknown directive"), "{unknown}");
        let stray = parse("stray\n", DEFAULT_FENCE, &["edit"]).unwrap_err();
        assert!(stray.contains("outside any item"), "{stray}");
        let dup = parse(
            "#% edit\n#% find\nx\n#% find\ny\n#% end\n",
            DEFAULT_FENCE,
            &["edit"],
        )
        .unwrap_err();
        assert!(dup.contains("duplicate section"), "{dup}");
        let unq = parse("#% edit expect=\"=1\n", DEFAULT_FENCE, &["edit"]).unwrap_err();
        assert!(unq.contains("unterminated"), "{unq}");
        let end = parse("#% end\n", DEFAULT_FENCE, &["edit"]).unwrap_err();
        assert!(end.contains("no open item"), "{end}");
    }

    #[test]
    fn attribute_only_items_close_implicitly() {
        let doc = "#% del path=a.b\n#% del path=c.d\n";
        let items = parse(doc, DEFAULT_FENCE, &["del"]).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].attr("path"), Some("c.d"));
    }
}
