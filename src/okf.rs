// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Open Knowledge Format (OKF) support shared across the suite.
//!
//! [OKF v0.1](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md)
//! is a minimal standard for knowledge corpora: a directory tree of Markdown
//! *concept* files, each carrying a leading YAML **frontmatter** block (one
//! required field, `type`; recommended `title`/`description`/`resource`/`tags`/
//! `timestamp`), plus reserved `index.md` (a directory listing) and `log.md` (a
//! change history). Cross-links are ordinary Markdown links, either
//! bundle-relative (`/tables/customers.md`) or document-relative.
//!
//! This module is the single home for the format: frontmatter detection and
//! field extraction ([`parse`]), reserved-file recognition ([`is_reserved`]),
//! cross-link extraction ([`links`]), bundle conformance ([`conformance`]) and
//! broken-link detection ([`broken_links`]). It is reused by `ct-okf`, by the
//! OKF-awareness added to `ct-search`/`ct-tree`/`ct-view`/`ct-outline`, and by
//! the `okf` built-in check ([`check`]).
//!
//! Conformance is deliberately permissive (per the spec): a non-reserved `.md`
//! conforms when it has parseable frontmatter carrying a non-empty `type`;
//! unknown keys, unknown types, and broken links are tolerated, never fatal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use clap::{CommandFactory, Parser};

use crate::rules::ProbeOutcome;
use crate::walk::{self, EntryType};

/// Reserved file names with defined structural roles; never concept documents.
pub const RESERVED: &[&str] = &["index.md", "log.md"];

/// Whether `file_name` is an OKF reserved file (`index.md` / `log.md`).
///
/// # Examples
///
/// ```
/// use coding_tools::okf::is_reserved;
///
/// assert!(is_reserved("index.md"));
/// assert!(is_reserved("log.md"));
/// assert!(!is_reserved("customers.md"));
/// ```
pub fn is_reserved(file_name: &str) -> bool {
    RESERVED.contains(&file_name)
}

/// The recognised frontmatter fields, extracted from a concept's YAML block.
/// Unknown keys are preserved in [`extra`](Frontmatter::extra), as the spec
/// requires consumers to tolerate and round-trip them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    /// The required `type` — the concept kind (e.g. `BigQuery Table`).
    pub type_: Option<String>,
    /// Human-readable name.
    pub title: Option<String>,
    /// One-sentence summary.
    pub description: Option<String>,
    /// URI of the underlying asset.
    pub resource: Option<String>,
    /// ISO 8601 last-modified datetime.
    pub timestamp: Option<String>,
    /// Cross-cutting tags.
    pub tags: Vec<String>,
    /// Any other scalar keys, preserved verbatim.
    pub extra: BTreeMap<String, String>,
}

/// A concept's parsed frontmatter and where it sits in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The extracted, recognised fields.
    pub fm: Frontmatter,
    /// The inner YAML text between the `---` fences (fences excluded).
    pub fm_block: String,
    /// 1-based inclusive line span of the whole block, fences included.
    pub fm_span: (usize, usize),
    /// 1-based line where the Markdown body begins (after the closing fence).
    pub body_start_line: usize,
    /// Whether the inner block parses as YAML (per `yaml-edit`).
    pub parseable: bool,
}

/// Parse a concept's leading frontmatter, or `None` when the text does not open
/// with a `---` fence (the only frontmatter form OKF defines).
///
/// Recognised scalar fields are typed; `tags` accepts both the flow form
/// (`tags: [a, b]`) and the block form (`tags:` then `- a`). [`parseable`] is
/// set by handing the inner block to `yaml-edit`, which is what conformance
/// checks for; field extraction itself is lenient.
///
/// [`parseable`]: Parsed::parseable
///
/// # Examples
///
/// ```
/// use coding_tools::okf::parse;
///
/// let doc = "---\ntype: Playbook\ntitle: Onboarding\ntags: [ops, hr]\n---\n# Steps\n";
/// let p = parse(doc).unwrap();
/// assert_eq!(p.fm.type_.as_deref(), Some("Playbook"));
/// assert_eq!(p.fm.tags, ["ops", "hr"]);
/// assert!(p.parseable);
/// assert_eq!(p.body_start_line, 6);
///
/// assert!(parse("# no frontmatter here\n").is_none());
/// ```
pub fn parse(text: &str) -> Option<Parsed> {
    // The file must open with a fence line that is exactly `---` (allowing a
    // trailing CR). A leading BOM or blank line means "no frontmatter".
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let is_fence = |l: &str| l.trim_end_matches(['\n', '\r']) == "---";
    if lines.is_empty() || !is_fence(lines[0]) {
        return None;
    }
    // Find the closing fence.
    let close = lines.iter().enumerate().skip(1).find(|(_, l)| is_fence(l));
    let (close_idx, _) = close?;
    let inner: String = lines[1..close_idx].concat();
    let parseable = yaml_edit::Document::from_str(&inner).is_ok();
    let fm = extract_fields(&inner);
    Some(Parsed {
        fm,
        fm_block: inner,
        fm_span: (1, close_idx + 1),
        body_start_line: close_idx + 2,
        parseable,
    })
}

/// Strip one layer of matching single/double quotes from a YAML scalar.
fn unquote(v: &str) -> String {
    let v = v.trim();
    let bytes = v.as_bytes();
    if v.len() >= 2
        && ((bytes[0] == b'"' && bytes[v.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[v.len() - 1] == b'\''))
    {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

/// Parse a flow sequence body (`a, b, "c"`) — the inside of `[...]`.
fn flow_items(body: &str) -> Vec<String> {
    body.split(',')
        .map(|s| unquote(s.trim()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// The lenient field reader behind [`parse`]: top-level `key: value` scalars and
/// a `tags` sequence (flow or block). Indented continuation lines that are not
/// block sequence items are ignored, so nested mappings never corrupt the
/// recognised fields.
fn extract_fields(inner: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let raw: Vec<&str> = inner.lines().collect();
    let mut i = 0;
    while i < raw.len() {
        let line = raw[i];
        i += 1;
        // Only top-level keys (no leading whitespace) define fields.
        if line.is_empty() || line.starts_with([' ', '\t']) || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim();
        if key == "tags" {
            if val.is_empty() {
                // Block form: collect following `- item` lines.
                while i < raw.len() {
                    let item = raw[i];
                    let t = item.trim_start();
                    if item.starts_with([' ', '\t']) && t.starts_with('-') {
                        let v = unquote(t[1..].trim());
                        if !v.is_empty() {
                            fm.tags.push(v);
                        }
                        i += 1;
                    } else if t.is_empty() {
                        i += 1;
                    } else {
                        break;
                    }
                }
            } else if let Some(body) = val.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                fm.tags = flow_items(body);
            } else {
                // A single bare value.
                fm.tags = flow_items(val);
            }
            continue;
        }
        let value = unquote(val);
        match key {
            "type" => fm.type_ = Some(value),
            "title" => fm.title = Some(value),
            "description" => fm.description = Some(value),
            "resource" => fm.resource = Some(value),
            "timestamp" => fm.timestamp = Some(value),
            _ if !value.is_empty() => {
                fm.extra.insert(key.to_string(), value);
            }
            _ => {}
        }
    }
    fm
}

/// Render a [`Frontmatter`] as a JSON object — only fields that are present
/// appear; unknown keys from [`extra`](Frontmatter::extra) are included
/// verbatim. Shared so `ct-okf` and the OKF-aware tools emit metadata the same
/// way.
pub fn fm_to_json(fm: &Frontmatter) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    if let Some(t) = &fm.type_ {
        m.insert("type".into(), serde_json::Value::String(t.clone()));
    }
    if let Some(t) = &fm.title {
        m.insert("title".into(), serde_json::Value::String(t.clone()));
    }
    if let Some(d) = &fm.description {
        m.insert("description".into(), serde_json::Value::String(d.clone()));
    }
    if let Some(r) = &fm.resource {
        m.insert("resource".into(), serde_json::Value::String(r.clone()));
    }
    if let Some(t) = &fm.timestamp {
        m.insert("timestamp".into(), serde_json::Value::String(t.clone()));
    }
    if !fm.tags.is_empty() {
        m.insert(
            "tags".into(),
            serde_json::Value::Array(
                fm.tags
                    .iter()
                    .map(|t| serde_json::Value::String(t.clone()))
                    .collect(),
            ),
        );
    }
    for (k, v) in &fm.extra {
        m.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    serde_json::Value::Object(m)
}

/// A Markdown cross-link found in a concept body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The raw link target (the `(...)` of `[text](target)`).
    pub target: String,
    /// Whether the target is bundle-relative (begins with `/`).
    pub absolute: bool,
    /// 1-based line the link occurs on.
    pub line: usize,
}

/// Extract Markdown `[text](target)` cross-links from `body`, skipping external
/// URLs (`http(s)://`, `mailto:`) and bare anchors (`#…`). The kind of
/// relationship is conveyed by prose, not syntax, so links are untyped edges.
///
/// # Examples
///
/// ```
/// use coding_tools::okf::links;
///
/// let body = "see [customers](/tables/customers.md) and [home](https://x.test)\n";
/// let ls = links(body);
/// assert_eq!(ls.len(), 1);
/// assert_eq!(ls[0].target, "/tables/customers.md");
/// assert!(ls[0].absolute);
/// ```
pub fn links(body: &str) -> Vec<Link> {
    // [text](target) — target is the run up to the first ')' or whitespace.
    let re = regex::Regex::new(r"\[[^\]]*\]\(([^)\s]+)\)").expect("static regex compiles");
    let mut out = Vec::new();
    for (n, line) in body.lines().enumerate() {
        for cap in re.captures_iter(line) {
            let target = cap[1].to_string();
            let lower = target.to_ascii_lowercase();
            if lower.starts_with("http://")
                || lower.starts_with("https://")
                || lower.starts_with("mailto:")
                || target.starts_with('#')
            {
                continue;
            }
            out.push(Link {
                absolute: target.starts_with('/'),
                target,
                line: n + 1,
            });
        }
    }
    out
}

/// A per-file conformance finding for a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Path relative to the bundle base (or absolute if it could not be made so).
    pub path: PathBuf,
    /// Whether this is a reserved file (`index.md`/`log.md`).
    pub reserved: bool,
    /// Whether the file opens with a frontmatter fence.
    pub has_frontmatter: bool,
    /// Whether the frontmatter block parses as YAML.
    pub parseable: bool,
    /// Whether a non-empty `type` is present (concepts only).
    pub has_type: bool,
    /// Whether this file satisfies OKF v0.1 conformance for its role.
    pub conformant: bool,
    /// Human-readable reasons it fails (empty when conformant).
    pub issues: Vec<String>,
    /// The parsed frontmatter, when present (for downstream listing).
    pub fm: Option<Frontmatter>,
}

/// Walk a bundle and judge each `.md` file's conformance.
///
/// Concepts (non-reserved `.md`) must have parseable frontmatter with a
/// non-empty `type`. Reserved files (`index.md`/`log.md`) need no `type` (and the
/// bundle-root `index.md` may carry `okf_version` frontmatter); their only rule
/// here is that any frontmatter present is parseable. The walk honors the
/// [`walk::Selector`] it is handed, so callers control root, filter, and flags.
pub fn conformance(selector: &walk::Selector) -> Result<Vec<Finding>, String> {
    let base = &selector.base;
    let mut findings = Vec::new();
    for entry in selector.walk() {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let rel = path.strip_prefix(base).unwrap_or(path).to_path_buf();
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", rel.display()))?;
        let reserved = is_reserved(&name);
        let parsed = parse(&text);
        let mut issues = Vec::new();
        let (has_frontmatter, parseable, has_type, fm) = match &parsed {
            Some(p) => (
                true,
                p.parseable,
                p.fm.type_.as_deref().is_some_and(|t| !t.trim().is_empty()),
                Some(p.fm.clone()),
            ),
            None => (false, false, false, None),
        };
        if reserved {
            // Reserved files (index.md/log.md) need no `type`; the bundle-root
            // index.md may even carry `okf_version` frontmatter. Their only
            // requirement here is that any frontmatter present is parseable.
            if has_frontmatter && !parseable {
                issues.push("frontmatter is not parseable YAML".to_string());
            }
        } else if !has_frontmatter {
            issues.push("missing frontmatter (no leading --- fence)".to_string());
        } else if !parseable {
            issues.push("frontmatter is not parseable YAML".to_string());
        } else if !has_type {
            issues.push("frontmatter missing a non-empty `type`".to_string());
        }
        findings.push(Finding {
            path: rel,
            reserved,
            has_frontmatter,
            parseable,
            has_type,
            conformant: issues.is_empty(),
            issues,
            fm,
        });
    }
    Ok(findings)
}

/// Find bundle cross-links whose target file is missing. Bundle-relative
/// (`/…`) targets resolve against `base`; document-relative targets resolve
/// against the linking file's directory. Any fragment (`#…`) is dropped before
/// resolution. External URLs are ignored by [`links`] and never appear here.
pub fn broken_links(selector: &walk::Selector) -> Result<Vec<(PathBuf, Link)>, String> {
    let base = &selector.base;
    let mut broken = Vec::new();
    for entry in selector.walk() {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path.strip_prefix(base).unwrap_or(path).to_path_buf();
        let dir = path.parent().unwrap_or(base);
        // Only the body's links matter; frontmatter has no Markdown links.
        let body = match parse(&text) {
            Some(p) => {
                let start = p.body_start_line.saturating_sub(1);
                text.lines().skip(start).collect::<Vec<_>>().join("\n")
            }
            None => text.clone(),
        };
        for link in links(&body) {
            let target = link.target.split('#').next().unwrap_or("");
            if target.is_empty() {
                continue;
            }
            let resolved = if link.absolute {
                base.join(target.trim_start_matches('/'))
            } else {
                dir.join(target)
            };
            if !resolved.exists() {
                broken.push((rel.clone(), link));
            }
        }
    }
    Ok(broken)
}

/// Today's date as `YYYY-MM-DD` (UTC), via Howard Hinnant's civil-from-days.
/// Shared so `log.md` entries and timestamps are stamped consistently.
pub fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Quote a frontmatter scalar value if it would not be a safe bare YAML scalar.
/// Shared by the authoring verbs and the `--script` engine so a value written by
/// either path round-trips identically.
///
/// # Examples
///
/// ```
/// use coding_tools::okf::yaml_scalar;
///
/// assert_eq!(yaml_scalar("Customers"), "Customers");
/// assert_eq!(yaml_scalar("a: b"), "\"a: b\"");
/// ```
pub fn yaml_scalar(v: &str) -> String {
    let needs_quote = v.is_empty()
        || v != v.trim()
        || v.starts_with(['[', '{', '#', '*', '&', '!', '|', '>', '\'', '"', '@', '`'])
        || v.contains(": ")
        || v.ends_with(':');
    if needs_quote {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        v.to_string()
    }
}

/// Build a new concept's file text: a frontmatter block (a non-empty `type` is
/// required by OKF) followed by the body, defaulting to a single `# title`
/// heading when no body is given.
pub fn build_concept(
    type_: &str,
    title: &str,
    description: Option<&str>,
    tags: &[String],
    timestamp: &str,
    body: Option<&str>,
) -> String {
    let mut s = format!("---\ntype: {}\n", yaml_scalar(type_));
    s.push_str(&format!("title: {}\n", yaml_scalar(title)));
    if let Some(d) = description {
        s.push_str(&format!("description: {}\n", yaml_scalar(d)));
    }
    if !tags.is_empty() {
        let items: Vec<String> = tags.iter().map(|t| yaml_scalar(t)).collect();
        s.push_str(&format!("tags: [{}]\n", items.join(", ")));
    }
    s.push_str(&format!("timestamp: {timestamp}\n---\n\n"));
    match body {
        Some(b) if !b.trim().is_empty() => {
            s.push_str(b);
            if !b.ends_with('\n') {
                s.push('\n');
            }
        }
        _ => s.push_str(&format!("# {title}\n")),
    }
    s
}

/// Set or update a top-level scalar frontmatter `field` on a concept's `text`,
/// preserving every other byte. Returns the new text and whether an existing key
/// was replaced (`false` means a new key was appended before the closing fence).
/// Errors when the text has no frontmatter to edit.
pub fn set_field(text: &str, field: &str, value: &str) -> Result<(String, bool), String> {
    let parsed = parse(text).ok_or("no frontmatter to edit")?;
    let (start, end) = parsed.fm_span; // 1-based, fences included
    let all: Vec<&str> = text.split_inclusive('\n').collect();
    let inner = &all[start..end - 1];
    let new_line = format!("{field}: {}\n", yaml_scalar(value));
    let mut replaced = false;
    let mut new_inner: Vec<String> = Vec::with_capacity(inner.len() + 1);
    for line in inner {
        let is_target = line
            .split_once(':')
            .is_some_and(|(k, _)| k.trim() == field && !line.starts_with([' ', '\t']));
        if is_target && !replaced {
            new_inner.push(new_line.clone());
            replaced = true;
        } else {
            new_inner.push((*line).to_string());
        }
    }
    if !replaced {
        new_inner.push(new_line);
    }
    let mut out = String::with_capacity(text.len() + field.len() + value.len() + 4);
    out.push_str(&all[..start].concat());
    out.push_str(&new_inner.concat());
    out.push_str(&all[end - 1..].concat());
    Ok((out, replaced))
}

/// Prepend a dated, labelled entry to a `log.md`'s existing text, merging into
/// the same-day section when it is already on top (newest first).
pub fn log_entry(existing: &str, today: &str, kind: &str, message: &str) -> String {
    let bullet = format!("* **{kind}**: {message}\n");
    let heading = format!("## {today}\n");
    if let Some(rest) = existing.strip_prefix(&heading) {
        format!("{heading}{bullet}{rest}")
    } else if existing.trim().is_empty() {
        format!("{heading}{bullet}")
    } else {
        format!("{heading}{bullet}\n{existing}")
    }
}

/// Render an `index.md` body from `(file, title, description)` entries.
pub fn render_index(entries: &[(String, String, String)]) -> String {
    let mut out = String::from("# Index\n\n");
    for (file, title, desc) in entries {
        if desc.is_empty() {
            out.push_str(&format!("* [{title}]({file})\n"));
        } else {
            out.push_str(&format!("* [{title}]({file}) - {desc}\n"));
        }
    }
    out
}

/// Build a `.md`-restricted [`walk::Selector`] under `base`, optionally narrowed
/// by a `name` pattern set. Shared by the tool and the built-in check so they
/// select concepts identically.
pub fn md_selector(
    base: PathBuf,
    names: Option<Vec<regex::Regex>>,
    hidden: bool,
    follow: bool,
) -> walk::Selector {
    let names = names.or_else(|| crate::pattern::compile_name_set("*.md").ok());
    walk::Selector {
        base,
        names,
        types: vec![EntryType::F],
        size: None,
        hidden,
        follow,
        no_ignore: false,
    }
}

// ----- The `okf` built-in check -------------------------------------------------------

/// The `okf` built-in check's argument grammar (mirrors `deps`/`mods`): assert a
/// bundle's OKF conformance, optionally also rejecting broken cross-links.
#[derive(Parser, Debug)]
#[command(
    name = "okf",
    about = "Assert that a directory is a conformant OKF bundle."
)]
struct OkfCheck {
    /// Bundle root to check, relative to the project root.
    #[arg(long, default_value = ".")]
    base: PathBuf,
    /// Limit to files whose name matches; '|'-separated alternatives.
    #[arg(long)]
    name: Option<String>,
    /// Include dot-entries (names starting with '.').
    #[arg(long)]
    hidden: bool,
    /// Follow symlinks while traversing.
    #[arg(long)]
    follow: bool,
    /// Also fail when a bundle-relative cross-link points at a missing file.
    #[arg(long)]
    strict: bool,
}

/// The `okf` check's introspected grammar (see [`crate::deps::grammar`]).
pub fn check_grammar() -> crate::deps::Grammar {
    crate::deps::grammar(OkfCheck::command())
}

/// Run an `okf` built-in check: walk the bundle under `root`/`--base` and assert
/// every non-reserved `.md` conforms (parseable frontmatter with a non-empty
/// `type`); with `--strict`, also assert no broken bundle cross-links. Returns
/// the probe outcome, a one-line reason, and a violation report. Argument and
/// walk errors are [`ProbeOutcome::Broken`].
pub fn check(
    args: &[String],
    root: &Path,
    timeout: Option<Duration>,
) -> (ProbeOutcome, String, String) {
    let started = Instant::now();
    let broken = |msg: String| (ProbeOutcome::Broken, msg, String::new());
    let cli = match OkfCheck::try_parse_from(
        std::iter::once("okf").chain(args.iter().map(String::as_str)),
    ) {
        Ok(c) => c,
        Err(e) => {
            let valid = check_grammar()
                .flags
                .iter()
                .map(|s| format!("--{}", s.name))
                .collect::<Vec<_>>()
                .join(" ");
            return broken(format!(
                "okf: {} (valid flags: {valid})",
                e.to_string().lines().next().unwrap_or("bad arguments")
            ));
        }
    };

    let names = match &cli.name {
        Some(spec) => match crate::pattern::compile_name_set(spec) {
            Ok(n) => Some(n),
            Err(e) => return broken(format!("okf: invalid --name: {e}")),
        },
        None => None,
    };
    let base = root.join(&cli.base);
    if !base.exists() {
        return broken(format!(
            "okf: bundle base does not exist: {}",
            base.display()
        ));
    }
    let selector = md_selector(base.clone(), names, cli.hidden, cli.follow);

    let findings = match conformance(&selector) {
        Ok(f) => f,
        Err(e) => return broken(format!("okf: {e}")),
    };
    if let Some(limit) = timeout
        && started.elapsed() >= limit
    {
        return broken(format!("okf: timed out after {:.1}s", limit.as_secs_f64()));
    }

    let mut report = String::new();
    let mut violations = 0usize;
    for f in &findings {
        if !f.conformant {
            violations += 1;
            report.push_str(&format!("{}: {}\n", f.path.display(), f.issues.join("; ")));
        }
    }
    let concepts = findings.iter().filter(|f| !f.reserved).count();

    if cli.strict {
        match broken_links(&selector) {
            Ok(bl) => {
                for (path, link) in &bl {
                    violations += 1;
                    report.push_str(&format!(
                        "{}:{}: broken link {}\n",
                        path.display(),
                        link.line,
                        link.target
                    ));
                }
            }
            Err(e) => return broken(format!("okf: {e}")),
        }
    }

    if violations == 0 {
        (
            ProbeOutcome::Holds,
            format!("{concepts} concept(s) conform"),
            report,
        )
    } else {
        (
            ProbeOutcome::Violated,
            format!("{violations} OKF violation(s)"),
            report.trim_end().to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_detects_and_extracts_frontmatter() {
        let doc = "---\ntype: Playbook\ntitle: Onboarding\ndescription: How to onboard\nresource: bq://x\ntimestamp: 2026-01-02\ntags: [ops, hr]\nowner: jane\n---\n# Steps\nbody\n";
        let p = parse(doc).unwrap();
        assert_eq!(p.fm.type_.as_deref(), Some("Playbook"));
        assert_eq!(p.fm.title.as_deref(), Some("Onboarding"));
        assert_eq!(p.fm.description.as_deref(), Some("How to onboard"));
        assert_eq!(p.fm.resource.as_deref(), Some("bq://x"));
        assert_eq!(p.fm.timestamp.as_deref(), Some("2026-01-02"));
        assert_eq!(p.fm.tags, ["ops", "hr"]);
        assert_eq!(p.fm.extra.get("owner").map(String::as_str), Some("jane"));
        assert!(p.parseable);
        assert_eq!(p.fm_span, (1, 9));
        assert_eq!(p.body_start_line, 10);
    }

    #[test]
    fn parse_handles_block_tags_and_quotes() {
        let doc = "---\ntype: \"BigQuery Table\"\ntags:\n  - core\n  - 'pii'\n---\nbody\n";
        let p = parse(doc).unwrap();
        assert_eq!(p.fm.type_.as_deref(), Some("BigQuery Table"));
        assert_eq!(p.fm.tags, ["core", "pii"]);
    }

    #[test]
    fn parse_returns_none_without_a_fence() {
        assert!(parse("# title\nno frontmatter\n").is_none());
        assert!(parse("").is_none());
        // A blank first line is not a fence.
        assert!(parse("\n---\ntype: x\n---\n").is_none());
    }

    #[test]
    fn unclosed_fence_is_not_frontmatter() {
        assert!(parse("---\ntype: x\nno closing fence\n").is_none());
    }

    #[test]
    fn reserved_files_recognised() {
        assert!(is_reserved("index.md"));
        assert!(is_reserved("log.md"));
        assert!(!is_reserved("concept.md"));
    }

    #[test]
    fn links_classifies_and_filters() {
        let body = "[a](/tables/x.md) [b](../sibling.md) [c](https://e.test) [d](#frag) [e](mailto:x@y.z)\n";
        let ls = links(body);
        assert_eq!(ls.len(), 2);
        assert_eq!(ls[0].target, "/tables/x.md");
        assert!(ls[0].absolute);
        assert_eq!(ls[1].target, "../sibling.md");
        assert!(!ls[1].absolute);
    }
}
