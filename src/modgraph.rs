// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The intra-crate **module dependency graph**, built heuristically from `use`
//! statements — the lean, pure-Rust alternative to booting a semantic engine
//! (the same honesty class as [`crate::outline`]). It is the single-crate
//! analogue of [`crate::deps`]: it produces a [`deps::Graph`] whose nodes are
//! *modules* (crate-relative paths like `domain::entity`) and whose edges are
//! `use` dependencies, so the very same [`deps::forbid_path`] /
//! [`deps::cycles`] / [`deps::layer_violations`] assertions apply at module
//! granularity.
//!
//! ## What it sees, and what it does not
//!
//! Nodes come from the source files walked (one module per file; `lib.rs` /
//! `main.rs` / `mod.rs` map to their containing module). Edges come from
//! `use crate::…`, `use self::…`, and `use super::…` statements — brace groups
//! are expanded, `as` aliases stripped, globs and `self` segments folded to the
//! enclosing module, and each path resolved to the **longest known module
//! prefix**. External imports (`use std::…`, `use serde::…`) are ignored.
//!
//! Heuristic, not a parser: each `use` is read from the start of its line (how
//! rustfmt always writes them); a `use` reaching through a `pub use` re-export
//! resolves to the re-exporting module (not the origin); `use` inside macros or
//! `#[cfg(...)]`-disabled code, fully-qualified paths written inline without a
//! `use`, and the nesting of inline `mod foo { … }` blocks are not modelled —
//! the latter fold into their file's module. When in doubt the graph
//! under-reports rather than inventing edges.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use clap::{CommandFactory, Parser};
use regex::Regex;

use crate::deps::{flag_kinds, EdgeKind, Graph, Package};
use crate::pattern;
use crate::rules::ProbeOutcome;
use crate::walk::{self, EntryType};

/// The crate-relative module path for a source file, given its path **relative
/// to the crate source root**. The crate root file (`lib.rs` / `main.rs`) and
/// any `mod.rs` map to their containing module; every other file adds its stem.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use coding_tools::modgraph::module_name;
///
/// assert_eq!(module_name(Path::new("lib.rs")), "crate");
/// assert_eq!(module_name(Path::new("main.rs")), "crate");
/// assert_eq!(module_name(Path::new("domain.rs")), "domain");
/// assert_eq!(module_name(Path::new("domain/mod.rs")), "domain");
/// assert_eq!(module_name(Path::new("domain/entity.rs")), "domain::entity");
/// ```
pub fn module_name(rel: &Path) -> String {
    let mut segs: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if let Some(file) = segs.pop() {
        let stem = Path::new(&file)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(file);
        if stem != "mod" && stem != "lib" && stem != "main" {
            segs.push(stem);
        }
    }
    if segs.is_empty() {
        "crate".to_string()
    } else {
        segs.join("::")
    }
}

/// Build the module-use [`Graph`] from `(module_name, file_contents)` pairs.
/// Nodes are the module names; an edge `A -> B` means a file in module `A` has
/// a `use` resolving into module `B`. Self-edges are dropped.
pub fn build_graph(files: &[(String, String)]) -> Graph {
    let modules: HashSet<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    let mut packages = HashMap::new();
    let mut edges: HashMap<String, Vec<(String, Vec<EdgeKind>)>> = HashMap::new();
    let mut members: Vec<String> = Vec::new();
    for (name, content) in files {
        packages.insert(
            name.clone(),
            Package { name: name.clone(), version: String::new() },
        );
        members.push(name.clone());
        let current = name_segs(name);
        let mut targets: BTreeSet<String> = BTreeSet::new();
        for raw in use_targets(content) {
            if let Some(t) = resolve(&raw, &current, &modules)
                && t != *name
            {
                targets.insert(t);
            }
        }
        edges.insert(
            name.clone(),
            targets.into_iter().map(|t| (t, vec![EdgeKind::Normal])).collect(),
        );
    }
    members.sort();
    members.dedup();
    Graph { packages, edges, members }
}

/// The intra-crate `use` targets in a source file: each returned string is a
/// brace-expanded, alias-stripped path beginning with `crate`, `self`, or
/// `super` (external imports are dropped).
pub fn use_targets(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in use_statements(content) {
        for leaf in expand_braces(&strip_aliases(&stmt)) {
            let leaf = leaf.trim();
            let head = leaf.split("::").next().unwrap_or("");
            if matches!(head, "crate" | "self" | "super") {
                out.push(leaf.to_string());
            }
        }
    }
    out
}

/// Resolve one raw `use` path (e.g. `crate::a::b::Item`) against the set of
/// known module names, relative to the `current` module's segments. Returns the
/// longest known module prefix, or `None` for an unresolvable / external path.
fn resolve(raw: &str, current: &[String], modules: &HashSet<&str>) -> Option<String> {
    let parts: Vec<&str> = raw.split("::").map(str::trim).filter(|s| !s.is_empty()).collect();
    let mut abs: Vec<String> = Vec::new();
    let mut i = 0;
    match *parts.first()? {
        "crate" => i = 1,
        "self" => {
            abs = current.to_vec();
            i = 1;
        }
        "super" => {
            abs = current.to_vec();
            while parts.get(i) == Some(&"super") {
                abs.pop();
                i += 1;
            }
        }
        _ => return None, // an external crate
    }
    for p in &parts[i..] {
        if *p == "self" || *p == "*" {
            continue; // `a::{self}` / `a::*` both mean module `a`
        }
        abs.push((*p).to_string());
    }
    // Longest-prefix match: the trailing segments are usually item names.
    loop {
        let name = if abs.is_empty() { "crate".to_string() } else { abs.join("::") };
        if modules.contains(name.as_str()) {
            return Some(name);
        }
        abs.pop()?;
    }
}

/// The module-path segments of a module name (`crate` is the empty root).
fn name_segs(name: &str) -> Vec<String> {
    if name == "crate" {
        Vec::new()
    } else {
        name.split("::").map(String::from).collect()
    }
}

/// Gather `use` statements as path-tree strings (text between `use` and `;`),
/// joining continuation lines so multi-line brace groups arrive whole.
fn use_statements(content: &str) -> Vec<String> {
    let mut stmts = Vec::new();
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        let Some(rest) = strip_vis_use(line) else {
            continue;
        };
        let mut body = rest.to_string();
        loop {
            if let Some(idx) = body.find(';') {
                body.truncate(idx);
                stmts.push(body);
                break;
            }
            match lines.next() {
                Some(next) => {
                    body.push(' ');
                    body.push_str(next.trim());
                }
                None => {
                    stmts.push(body);
                    break;
                }
            }
        }
    }
    stmts
}

/// If `line` is a `use` item (optionally `pub` / `pub(...)`), return the text
/// after the `use` keyword; otherwise `None`.
fn strip_vis_use(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let after_vis = if let Some(r) = t.strip_prefix("pub") {
        let r = r.trim_start();
        let r = if r.starts_with('(') {
            r.find(')').map(|i| &r[i + 1..]).unwrap_or(r)
        } else {
            r
        };
        r.trim_start()
    } else {
        t
    };
    after_vis
        .strip_prefix("use")
        .filter(|r| r.starts_with(char::is_whitespace))
        .map(str::trim_start)
}

/// Remove ` as Alias` runs (the import-alias keyword) from a use-tree string.
fn strip_aliases(s: &str) -> std::borrow::Cow<'_, str> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\s+as\s+[A-Za-z_][A-Za-z0-9_]*").unwrap());
    re.replace_all(s, "")
}

/// Expand `{…}` groups in a use path into one leaf path per branch.
fn expand_braces(s: &str) -> Vec<String> {
    let s = s.trim();
    match s.find('{') {
        None => {
            if s.is_empty() {
                vec![]
            } else {
                vec![s.to_string()]
            }
        }
        Some(open) => {
            let Some(close) = matching_brace(s.as_bytes(), open) else {
                return vec![s.to_string()]; // unbalanced: leave verbatim
            };
            let prefix = &s[..open];
            let inner = &s[open + 1..close];
            let suffix = &s[close + 1..];
            let mut out = Vec::new();
            for part in split_top_commas(inner) {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                out.extend(expand_braces(&format!("{prefix}{part}{suffix}")));
            }
            out
        }
    }
}

/// The index of the `}` matching the `{` at `open`, by depth.
fn matching_brace(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split on commas that sit at brace depth zero.
fn split_top_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

// ----- The `mods` built-in check -------------------------------------------------

/// The targeting + assertion flags of a `mods` built-in check. No framing — the
/// rule layer owns the verdict.
#[derive(Parser, Debug)]
#[command(no_binary_name = true, disable_help_flag = true)]
struct ModsCheck {
    #[arg(long, default_value = "src")]
    base: String,
    #[arg(long)]
    name: Option<String>,
    #[arg(long, value_delimiter = ',')]
    ext: Vec<String>,
    #[arg(long)]
    hidden: bool,
    #[arg(long)]
    follow: bool,
    #[arg(long, value_name = "A=>B")]
    forbid: Vec<String>,
    #[arg(long)]
    acyclic: bool,
    #[arg(long, value_name = "L0,L1,...", value_delimiter = ',')]
    layers: Vec<String>,
    #[arg(long)]
    layers_closed: bool,
}

/// The `mods` check's flags as `(name, kind)` pairs, read straight from the clap
/// grammar (via [`crate::deps::flag_kinds`]). The single source of truth behind
/// the published `docs/explain/mods.json` schema (a test reconciles the two) and
/// the valid-flags hint on a bad argument.
pub fn check_flags() -> Vec<(String, &'static str)> {
    flag_kinds(ModsCheck::command())
}

/// Run a `mods` built-in check: walk the crate source under `root`/`--base`,
/// build the module-use graph, and assert against it. Returns the probe
/// outcome, a one-line reason, and the violation report. Spec / walk errors are
/// [`ProbeOutcome::Broken`].
pub fn check(args: &[String], root: &Path, _timeout: Option<Duration>) -> (ProbeOutcome, String, String) {
    let broken = |msg: String| (ProbeOutcome::Broken, msg, String::new());
    let cli = match ModsCheck::try_parse_from(args.iter().map(String::as_str)) {
        Ok(c) => c,
        Err(e) => {
            let valid = check_flags().iter().map(|(f, _)| format!("--{f}")).collect::<Vec<_>>().join(" ");
            return broken(format!(
                "mods: {} (valid flags: {valid})",
                e.to_string().lines().next().unwrap_or("bad arguments")
            ));
        }
    };
    if cli.forbid.is_empty() && !cli.acyclic && cli.layers.is_empty() {
        return broken("mods: nothing to assert (--forbid/--acyclic/--layers)".to_string());
    }
    if cli.layers_closed && cli.layers.is_empty() {
        return broken("mods: --layers-closed requires --layers".to_string());
    }
    let forbids: Vec<(String, String)> = match cli
        .forbid
        .iter()
        .map(|spec| {
            spec.split_once("=>")
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                .filter(|(a, b)| !a.is_empty() && !b.is_empty())
                .ok_or_else(|| format!("mods: --forbid needs 'A=>B', got '{spec}'"))
        })
        .collect()
    {
        Ok(f) => f,
        Err(e) => return broken(e),
    };

    let mut name_spec = cli.name.clone().unwrap_or_default();
    let exts: Vec<String> = if cli.ext.is_empty() { vec!["rs".to_string()] } else { cli.ext.clone() };
    for e in &exts {
        let e = e.trim().trim_start_matches('.');
        if e.is_empty() {
            continue;
        }
        if !name_spec.is_empty() {
            name_spec.push('|');
        }
        name_spec.push_str(&format!("*.{e}"));
    }
    let names = match pattern::compile_name_set(&name_spec) {
        Ok(n) => n,
        Err(e) => return broken(format!("mods: invalid --name/--ext: {e}")),
    };
    let base = root.join(&cli.base);
    let selector = walk::Selector {
        base: base.clone(),
        names: Some(names),
        types: vec![EntryType::F],
        size: None,
        hidden: cli.hidden,
        follow: cli.follow,
    };
    let mut files: Vec<(String, String)> = Vec::new();
    for entry in selector.walk() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => return broken(format!("mods: {e}")),
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(&base).unwrap_or(path);
        let Ok(text) = std::fs::read_to_string(path) else {
            continue; // unreadable / non-UTF-8 in a walk: skipped
        };
        files.push((module_name(rel), text));
    }
    if files.is_empty() {
        return broken(format!("mods: no source files under {}", base.display()));
    }
    let graph = build_graph(&files);

    let allowed: HashSet<EdgeKind> = [EdgeKind::Normal, EdgeKind::Build, EdgeKind::Dev].into_iter().collect();
    let mut violations: Vec<crate::deps::Violation> = Vec::new();
    for (from, to) in &forbids {
        match crate::deps::forbid_path(&graph, from, to, &allowed) {
            Ok(v) => violations.extend(v),
            Err(e) => return broken(format!("mods: {e}")),
        }
    }
    if cli.acyclic {
        violations.extend(crate::deps::cycles(&graph, &allowed, false));
    }
    if !cli.layers.is_empty() {
        let compiled = match cli.layers.iter().map(|p| pattern::compile_anchored(p)).collect::<Result<Vec<_>, _>>() {
            Ok(c) => c,
            Err(e) => return broken(format!("mods: --layers invalid pattern: {e}")),
        };
        let (layers, unassigned) =
            match crate::deps::assign_layers(&graph, &cli.layers, |i, n| compiled[i].is_match(n)) {
                Ok(r) => r,
                Err(e) => return broken(format!("mods: --layers: {e}")),
            };
        violations.extend(crate::deps::layer_violations(&graph, &layers, &allowed));
        if cli.layers_closed {
            violations.extend(unassigned.into_iter().map(|name| crate::deps::Violation {
                check: "layers-closed".to_string(),
                subject: name,
                evidence: "matches no layer".to_string(),
            }));
        }
    }

    crate::deps::report_outcome("mods", violations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::{self, EdgeKind};

    fn all_edges() -> HashSet<EdgeKind> {
        [EdgeKind::Normal, EdgeKind::Build, EdgeKind::Dev].into_iter().collect()
    }

    #[test]
    fn use_targets_handles_the_common_forms() {
        let src = r#"
            // a leading comment with the word use in it
            use std::collections::HashMap;          // external: dropped
            use crate::domain::Entity;
            pub use crate::infra::{Db, cache::Lru};  // re-export + nested brace
            use self::helpers::go as g;              // self + alias
            use super::sibling::Thing;
            use crate::a::{self, b};                 // self segment (folds in resolve)
            fn body() {
                use crate::late::Local;             // own-line local import: counts
            }
        "#;
        let mut t = use_targets(src);
        t.sort();
        // Raw, pre-resolution paths: brace-expanded, alias-stripped, intra-crate
        // only (`self`/`*` segments are folded later, by `resolve`).
        assert_eq!(
            t,
            vec![
                "crate::a::b",
                "crate::a::self",
                "crate::domain::Entity",
                "crate::infra::Db",
                "crate::infra::cache::Lru",
                "crate::late::Local",
                "self::helpers::go",
                "super::sibling::Thing",
            ]
        );
    }

    #[test]
    fn use_targets_joins_multiline_groups() {
        let src = "use crate::a::{\n    b,\n    c::d,\n};\n";
        let mut t = use_targets(src);
        t.sort();
        assert_eq!(t, vec!["crate::a::b", "crate::a::c::d"]);
    }

    #[test]
    fn resolve_picks_the_longest_known_module() {
        let modules: HashSet<&str> = ["crate", "a", "a::b", "domain"].into_iter().collect();
        // `a::b::Item` -> module `a::b`; `a::Item` -> module `a`.
        assert_eq!(resolve("crate::a::b::Item", &[], &modules).as_deref(), Some("a::b"));
        assert_eq!(resolve("crate::a::Item", &[], &modules).as_deref(), Some("a"));
        // self/super resolve relative to the current module.
        let cur = name_segs("a::b");
        assert_eq!(resolve("super::Item", &cur, &modules).as_deref(), Some("a"));
        assert_eq!(resolve("self::Item", &cur, &modules).as_deref(), Some("a::b"));
        // An item in the crate root folds to `crate`; externals are None.
        assert_eq!(resolve("crate::TopItem", &[], &modules).as_deref(), Some("crate"));
        assert_eq!(resolve("serde::Deserialize", &[], &modules), None);
    }

    /// crate -> domain -> infra (a clean three-module layering).
    fn sample_crate() -> Vec<(String, String)> {
        vec![
            ("crate".into(), "mod domain;\nmod infra;\nuse crate::domain::Entity;\n".into()),
            ("domain".into(), "use crate::infra::Db;\npub struct Entity;\n".into()),
            ("infra".into(), "pub struct Db;\n".into()),
        ]
    }

    #[test]
    fn build_graph_edges_and_forbid() {
        let g = build_graph(&sample_crate());
        // domain reaches infra; the reverse does not hold.
        let v = deps::forbid_path(&g, "domain", "infra", &all_edges()).unwrap().unwrap();
        assert_eq!(v.subject, "domain=>infra");
        assert_eq!(v.evidence, "domain -> infra");
        assert!(deps::forbid_path(&g, "infra", "domain", &all_edges()).unwrap().is_none());
    }

    #[test]
    fn build_graph_layers_flag_an_upward_module_edge() {
        let g = build_graph(&sample_crate());
        // infra is the lowest layer (highest first): domain must not reach infra.
        let labels = vec!["infra".to_string(), "domain".to_string()];
        let (layers, _) = deps::assign_layers(&g, &labels, |i, name| labels[i] == name).unwrap();
        let viol = deps::layer_violations(&g, &layers, &all_edges());
        assert_eq!(viol.len(), 1);
        assert_eq!(viol[0].subject, "domain => infra");
        assert_eq!(viol[0].evidence, "domain -> infra");
    }

    #[test]
    fn build_graph_detects_a_module_cycle() {
        let files = vec![
            ("crate".into(), "mod a;\nmod b;\n".to_string()),
            ("a".into(), "use crate::b::Thing;\n".to_string()),
            ("b".into(), "use crate::a::Other;\n".to_string()),
        ];
        let g = build_graph(&files);
        let cycles = deps::cycles(&g, &all_edges(), false);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].subject, "a, b");
        assert_eq!(cycles[0].evidence, "a -> b -> a");
    }
}
