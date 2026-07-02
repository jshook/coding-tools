// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-survey`'s format-contextualized codebase survey.
//!
//! Where [`crate::tree`] reports file-generic line/word/character counts over any
//! tree, `ct-survey` reports them **bucketed by the units a build system defines**
//! — for Rust, the workspace → crate → module hierarchy. The honesty classes are
//! kept distinct and carried into the output so they are never silently conflated:
//!
//! * **authoritative** — crate identity, workspace membership, and cargo target
//!   kinds, read from `cargo metadata` (the same mechanism [`crate::deps`] uses);
//! * **exact** — file, line, word, and character counts;
//! * **heuristic** — the module bucketing (via [`crate::modgraph::module_name`])
//!   and the `#[test]` tally, which a scan approximates rather than proves.
//!
//! The pure pieces here (metadata parse, the test scan, the roll-up, rendering)
//! are doctested; `src/bin/ct-survey.rs` is the thin IO shell that walks the
//! filesystem and drives them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::modgraph::module_name;

/// Which contextual group type frames a survey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GroupKind {
    /// A cargo workspace: the elements are its member crates.
    CargoWorkspace,
    /// A single cargo crate: the element is that crate alone.
    CargoCrate,
}

impl GroupKind {
    /// The `--group` token / JSON label.
    pub fn label(self) -> &'static str {
        match self {
            GroupKind::CargoWorkspace => "cargo-workspace",
            GroupKind::CargoCrate => "cargo-crate",
        }
    }
}

/// How deep the survey graph descends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Depth {
    /// Stop at crates (no per-module breakdown).
    Crate,
    /// Descend into each crate's modules (the default).
    Module,
}

/// Sort key for crates and, within each crate, its modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SortKey {
    /// By name, ascending (the default).
    Name,
    /// By file count, largest first.
    Files,
    /// By line count, largest first.
    Lines,
    /// By heuristic test count, largest first.
    Tests,
}

/// Infer the contextual group type from a `Cargo.toml`'s text: a manifest that
/// declares a `[workspace]` table is a [`GroupKind::CargoWorkspace`], otherwise a
/// [`GroupKind::CargoCrate`]. This probes only the provided manifest — the
/// authoritative member and target data still comes from `cargo metadata`.
///
/// # Examples
///
/// ```
/// use coding_tools::survey::{infer_group, GroupKind};
///
/// assert_eq!(infer_group("[workspace]\nmembers = [\"a\"]\n"), GroupKind::CargoWorkspace);
/// assert_eq!(infer_group("[workspace.package]\nversion = \"1\"\n"), GroupKind::CargoWorkspace);
/// assert_eq!(infer_group("[package]\nname = \"x\"\n"), GroupKind::CargoCrate);
/// // A commented-out header does not count.
/// assert_eq!(infer_group("# [workspace]\n[package]\n"), GroupKind::CargoCrate);
/// ```
pub fn infer_group(manifest_text: &str) -> GroupKind {
    for line in manifest_text.lines() {
        let t = line.trim();
        if t.starts_with("[workspace]") || t.starts_with("[workspace.") {
            return GroupKind::CargoWorkspace;
        }
    }
    GroupKind::CargoCrate
}

/// One cargo target within a package.
#[derive(Debug, Clone)]
pub struct Target {
    /// Cargo target kinds, e.g. `["lib"]`, `["bin"]`, `["test"]`, `["bench"]`.
    pub kinds: Vec<String>,
    /// Absolute path to the target's entry source file.
    pub src_path: String,
}

/// One package as `cargo metadata` reports it (the subset a survey needs).
#[derive(Debug, Clone)]
pub struct PkgMeta {
    /// Opaque package id (the metadata graph key).
    pub id: String,
    /// Crate name.
    pub name: String,
    /// Resolved version.
    pub version: String,
    /// Absolute path to the package's `Cargo.toml`.
    pub manifest_path: String,
    /// The package's build targets.
    pub targets: Vec<Target>,
}

impl PkgMeta {
    /// The package directory (its `Cargo.toml`'s parent).
    pub fn dir(&self) -> PathBuf {
        Path::new(&self.manifest_path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// The primary source root for module bucketing: the directory of the `lib`
    /// target's entry file, else the first `bin`, else the first target. `None`
    /// when no target carries a source path.
    pub fn src_root(&self) -> Option<PathBuf> {
        let pick = self
            .targets
            .iter()
            .find(|t| t.kinds.iter().any(|k| k == "lib"))
            .or_else(|| {
                self.targets
                    .iter()
                    .find(|t| t.kinds.iter().any(|k| k == "bin"))
            })
            .or_else(|| self.targets.first())?;
        Path::new(&pick.src_path).parent().map(Path::to_path_buf)
    }

    /// Authoritative count of cargo test targets (a `kind` of `test`).
    pub fn test_targets(&self) -> u64 {
        self.targets
            .iter()
            .filter(|t| t.kinds.iter().any(|k| k == "test"))
            .count() as u64
    }

    /// Authoritative count of cargo bench targets (a `kind` of `bench`).
    pub fn bench_targets(&self) -> u64 {
        self.targets
            .iter()
            .filter(|t| t.kinds.iter().any(|k| k == "bench"))
            .count() as u64
    }
}

/// The parsed subset of `cargo metadata`: packages by id, workspace member ids,
/// and the workspace root directory.
#[derive(Debug, Clone)]
pub struct Metadata {
    /// Package id → its metadata.
    pub packages: BTreeMap<String, PkgMeta>,
    /// Workspace member package ids.
    pub members: Vec<String>,
    /// The workspace root directory.
    pub workspace_root: String,
}

/// Parse `cargo metadata --format-version 1` JSON into the survey [`Metadata`].
/// Errors on malformed JSON or a missing `packages`/`workspace_members` array —
/// a defective read, never a silent empty survey.
pub fn parse_metadata(text: &str) -> Result<Metadata, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("cargo metadata JSON: {e}"))?;
    let mut packages = BTreeMap::new();
    for p in v["packages"]
        .as_array()
        .ok_or("metadata missing packages")?
    {
        let id = p["id"].as_str().ok_or("package missing id")?.to_string();
        let targets = p["targets"]
            .as_array()
            .map(|ts| {
                ts.iter()
                    .map(|t| Target {
                        kinds: t["kind"]
                            .as_array()
                            .map(|ks| {
                                ks.iter()
                                    .filter_map(|k| k.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        src_path: t["src_path"].as_str().unwrap_or("").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        packages.insert(
            id.clone(),
            PkgMeta {
                id,
                name: p["name"].as_str().unwrap_or("").to_string(),
                version: p["version"].as_str().unwrap_or("").to_string(),
                manifest_path: p["manifest_path"].as_str().unwrap_or("").to_string(),
                targets,
            },
        );
    }
    let members = v["workspace_members"]
        .as_array()
        .ok_or("metadata missing workspace_members")?
        .iter()
        .filter_map(|m| m.as_str().map(String::from))
        .collect();
    let workspace_root = v["workspace_root"].as_str().unwrap_or("").to_string();
    Ok(Metadata {
        packages,
        members,
        workspace_root,
    })
}

/// Heuristic count of test functions in a Rust source: attributes whose final
/// path segment is `test` — `#[test]`, `#[tokio::test]`, `#[test_case::test]`,
/// and the like. A comprehension aid, not a parser: it does not discount
/// attributes inside strings or comments, and `#[cfg(test)]` (a module gate, not
/// a test) is deliberately excluded. Always reported as a heuristic value.
///
/// # Examples
///
/// ```
/// use coding_tools::survey::count_tests;
///
/// assert_eq!(count_tests("#[test]\nfn a() {}\n#[tokio::test]\nasync fn b() {}"), 2);
/// // `#[cfg(test)]` gates a module; it is not a test.
/// assert_eq!(count_tests("#[cfg(test)]\nmod tests { fn helper() {} }"), 0);
/// assert_eq!(count_tests("fn not_a_test() {}"), 0);
/// ```
pub fn count_tests(src: &str) -> u64 {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"#\[\s*(?:[A-Za-z_]\w*\s*::\s*)*test\s*[\](]").expect("a valid regex")
    });
    re.find_iter(src).count() as u64
}

/// One walked source file's contribution: its path relative to the crate's
/// source root (`None` when it lies outside that root, e.g. an integration test
/// under `tests/`), its exact counts, and its heuristic test tally.
#[derive(Debug, Clone)]
pub struct FileStat {
    /// Path relative to the crate source root, `/`-separated; `None` if outside.
    pub rel_to_src: Option<String>,
    /// Exact line count.
    pub lines: u64,
    /// Exact word count.
    pub words: u64,
    /// Exact character count.
    pub chars: u64,
    /// Heuristic `#[test]` count.
    pub tests: u64,
}

/// A rolled-up count block (a crate's or a module's).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    /// Number of source files.
    pub files: u64,
    /// Total lines.
    pub lines: u64,
    /// Total words.
    pub words: u64,
    /// Total characters.
    pub chars: u64,
    /// Total heuristic test count.
    pub tests: u64,
}

/// One module node in the survey graph.
#[derive(Debug, Clone)]
pub struct ModuleNode {
    /// Crate-relative module path (e.g. `domain::entity`).
    pub name: String,
    /// The module's counts.
    pub counts: Counts,
}

/// Roll a crate's [`FileStat`]s into whole-crate [`Counts`] (every file) plus a
/// per-module breakdown (only files under the source root, bucketed by
/// [`module_name`]), the modules sorted by name. The whole-crate total can
/// exceed the module sum: files outside the source root (integration tests,
/// benches) count toward the crate but belong to no module.
///
/// # Examples
///
/// ```
/// use coding_tools::survey::{roll_up, FileStat};
///
/// let files = vec![
///     FileStat { rel_to_src: Some("lib.rs".into()), lines: 10, words: 20, chars: 100, tests: 1 },
///     FileStat { rel_to_src: Some("a/mod.rs".into()), lines: 5, words: 8, chars: 40, tests: 0 },
///     FileStat { rel_to_src: None, lines: 3, words: 4, chars: 20, tests: 2 }, // a tests/ file
/// ];
/// let (crate_counts, modules) = roll_up(&files);
/// assert_eq!(crate_counts.files, 3);
/// assert_eq!(crate_counts.lines, 18);
/// assert_eq!(crate_counts.tests, 3);
/// // Two modules: `a` and `crate` (lib.rs); the tests/ file is in neither.
/// assert_eq!(modules.len(), 2);
/// assert_eq!(modules[0].name, "a");
/// assert_eq!(modules[1].name, "crate");
/// ```
pub fn roll_up(files: &[FileStat]) -> (Counts, Vec<ModuleNode>) {
    let mut crate_counts = Counts::default();
    let mut by_mod: BTreeMap<String, Counts> = BTreeMap::new();
    for f in files {
        crate_counts.files += 1;
        crate_counts.lines += f.lines;
        crate_counts.words += f.words;
        crate_counts.chars += f.chars;
        crate_counts.tests += f.tests;
        if let Some(rel) = &f.rel_to_src {
            let m = by_mod.entry(module_name(Path::new(rel))).or_default();
            m.files += 1;
            m.lines += f.lines;
            m.words += f.words;
            m.chars += f.chars;
            m.tests += f.tests;
        }
    }
    let modules = by_mod
        .into_iter()
        .map(|(name, counts)| ModuleNode { name, counts })
        .collect();
    (crate_counts, modules)
}

/// One crate node in the survey graph.
#[derive(Debug, Clone)]
pub struct CrateNode {
    /// Crate name.
    pub name: String,
    /// Resolved version.
    pub version: String,
    /// The crate's rolled-up counts (every source file).
    pub counts: Counts,
    /// Authoritative cargo test-target count.
    pub test_targets: u64,
    /// Authoritative cargo bench-target count.
    pub bench_targets: u64,
    /// The crate's modules (empty at `--depth crate`).
    pub modules: Vec<ModuleNode>,
}

/// A complete survey graph.
#[derive(Debug, Clone)]
pub struct Survey {
    /// The contextual group type this survey was built under.
    pub group: GroupKind,
    /// Workspace (or lone crate) display name.
    pub name: String,
    /// Workspace root (or lone crate) directory.
    pub root: String,
    /// The surveyed crates.
    pub crates: Vec<CrateNode>,
}

fn order(a_name: &str, b_name: &str, a: u64, b: u64, key: SortKey) -> std::cmp::Ordering {
    match key {
        SortKey::Name => a_name.cmp(b_name),
        // Count keys descend (largest first); ties break by name.
        _ => b.cmp(&a).then_with(|| a_name.cmp(b_name)),
    }
}

fn count_for(c: &Counts, key: SortKey) -> u64 {
    match key {
        SortKey::Name | SortKey::Files => c.files,
        SortKey::Lines => c.lines,
        SortKey::Tests => c.tests,
    }
}

impl Survey {
    /// Sort crates, and each crate's modules, by `key` in place.
    pub fn sort(&mut self, key: SortKey) {
        self.crates.sort_by(|a, b| {
            order(
                &a.name,
                &b.name,
                count_for(&a.counts, key),
                count_for(&b.counts, key),
                key,
            )
        });
        for c in &mut self.crates {
            c.modules.sort_by(|a, b| {
                order(
                    &a.name,
                    &b.name,
                    count_for(&a.counts, key),
                    count_for(&b.counts, key),
                    key,
                )
            });
        }
    }
}

/// The whole-survey totals: rolled-up [`Counts`] plus authoritative test- and
/// bench-target counts across every crate.
pub fn totals(survey: &Survey) -> (Counts, u64, u64) {
    let mut c = Counts::default();
    let mut test_targets = 0;
    let mut bench_targets = 0;
    for cr in &survey.crates {
        c.files += cr.counts.files;
        c.lines += cr.counts.lines;
        c.words += cr.counts.words;
        c.chars += cr.counts.chars;
        c.tests += cr.counts.tests;
        test_targets += cr.test_targets;
        bench_targets += cr.bench_targets;
    }
    (c, test_targets, bench_targets)
}

/// Render the survey as indented text. Heuristic values (test counts) wear a
/// trailing `~`; a closing legend explains the marks.
///
/// # Examples
///
/// ```
/// use coding_tools::survey::{render_text, CrateNode, Counts, Depth, GroupKind, Survey};
///
/// let survey = Survey {
///     group: GroupKind::CargoCrate,
///     name: "demo".into(),
///     root: "/demo".into(),
///     crates: vec![CrateNode {
///         name: "demo".into(),
///         version: "0.1.0".into(),
///         counts: Counts { files: 2, lines: 30, words: 40, chars: 300, tests: 3 },
///         test_targets: 1,
///         bench_targets: 0,
///         modules: vec![],
///     }],
/// };
/// let text = render_text(&survey, Depth::Crate);
/// assert!(text.starts_with("crate demo"));
/// assert!(text.contains("tests 3~"));
/// assert!(text.contains("test-targets 1"));
/// ```
pub fn render_text(survey: &Survey, depth: Depth) -> String {
    let mut out = String::new();
    match survey.group {
        GroupKind::CargoWorkspace => out.push_str(&format!(
            "workspace {} — {} crate(s)   [grouping: authoritative via cargo metadata]\n",
            survey.name,
            survey.crates.len()
        )),
        GroupKind::CargoCrate => out.push_str(&format!(
            "crate {}   [grouping: authoritative via cargo metadata]\n",
            survey.name
        )),
    }
    for c in &survey.crates {
        out.push_str(&format!(
            "  {} v{}  files {}  lines {}  tests {}~  test-targets {}  benches {}\n",
            c.name,
            c.version,
            c.counts.files,
            c.counts.lines,
            c.counts.tests,
            c.test_targets,
            c.bench_targets
        ));
        if depth == Depth::Module {
            for m in &c.modules {
                out.push_str(&format!(
                    "    {}  files {}  lines {}  tests {}~\n",
                    m.name, m.counts.files, m.counts.lines, m.counts.tests
                ));
            }
        }
    }
    let (tot, test_targets, bench_targets) = totals(survey);
    out.push_str(&format!(
        "totals  files {}  lines {}  tests {}~  test-targets {}  benches {}\n",
        tot.files, tot.lines, tot.tests, test_targets, bench_targets
    ));
    out.push_str(
        "(~ = heuristic; file/line counts exact; grouping and target counts authoritative)\n",
    );
    out
}

/// The survey as a structured JSON value, each metric block tagged with the
/// honesty class it belongs to (so an exact line count is never read as a
/// heuristic test count).
pub fn to_json(survey: &Survey) -> Value {
    let (tot, test_targets, bench_targets) = totals(survey);
    let crates: Vec<Value> = survey
        .crates
        .iter()
        .map(|c| {
            let modules: Vec<Value> = c
                .modules
                .iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "files": m.counts.files,
                        "lines": m.counts.lines,
                        "words": m.counts.words,
                        "chars": m.counts.chars,
                        "tests": m.counts.tests,
                    })
                })
                .collect();
            json!({
                "name": c.name,
                "version": c.version,
                "files": c.counts.files,
                "lines": c.counts.lines,
                "words": c.counts.words,
                "chars": c.counts.chars,
                "tests": c.counts.tests,
                "test_targets": c.test_targets,
                "bench_targets": c.bench_targets,
                "modules": modules,
            })
        })
        .collect();
    json!({
        "tool": "ct-survey",
        "group": survey.group.label(),
        "name": survey.name,
        "root": survey.root,
        "honesty": {
            "grouping": "authoritative",
            "counts": "exact",
            "tests": "heuristic",
            "test_targets": "authoritative",
            "modules": "heuristic",
        },
        "crates": crates,
        "totals": {
            "crates": survey.crates.len(),
            "files": tot.files,
            "lines": tot.lines,
            "words": tot.words,
            "chars": tot.chars,
            "tests": tot.tests,
            "test_targets": test_targets,
            "bench_targets": bench_targets,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-package metadata document with lib/bin/test/bench targets.
    fn sample() -> &'static str {
        r#"{
          "packages": [
            {"id": "app 0.1.0 (path+file:///w/app)", "name": "app", "version": "0.1.0",
             "manifest_path": "/w/app/Cargo.toml",
             "targets": [
               {"kind": ["lib"], "src_path": "/w/app/src/lib.rs"},
               {"kind": ["bin"], "src_path": "/w/app/src/bin/tool.rs"},
               {"kind": ["test"], "src_path": "/w/app/tests/it.rs"},
               {"kind": ["bench"], "src_path": "/w/app/benches/b.rs"}
             ]}
          ],
          "workspace_members": ["app 0.1.0 (path+file:///w/app)"],
          "workspace_root": "/w"
        }"#
    }

    #[test]
    fn parses_packages_members_and_targets() {
        let m = parse_metadata(sample()).unwrap();
        assert_eq!(m.members.len(), 1);
        assert_eq!(m.workspace_root, "/w");
        let p = m.packages.values().next().unwrap();
        assert_eq!(p.name, "app");
        assert_eq!(p.version, "0.1.0");
        assert_eq!(p.test_targets(), 1);
        assert_eq!(p.bench_targets(), 1);
        assert_eq!(p.dir(), Path::new("/w/app"));
        // The lib target wins the source root, not the bin.
        assert_eq!(p.src_root().unwrap(), Path::new("/w/app/src"));
    }

    #[test]
    fn malformed_or_incomplete_metadata_errors() {
        assert!(parse_metadata("{ not json").is_err());
        assert!(parse_metadata("{}").is_err());
    }

    #[test]
    fn test_scan_counts_attributes_not_cfg_gates() {
        let src =
            "#[cfg(test)]\nmod t {\n  #[test]\n  fn a() {}\n  #[tokio::test]\n  async fn b() {}\n}";
        assert_eq!(count_tests(src), 2);
    }

    #[test]
    fn sort_orders_crates_and_breaks_ties_by_name() {
        let mk = |name: &str, files: u64| CrateNode {
            name: name.into(),
            version: "0".into(),
            counts: Counts {
                files,
                ..Counts::default()
            },
            test_targets: 0,
            bench_targets: 0,
            modules: vec![],
        };
        let mut s = Survey {
            group: GroupKind::CargoWorkspace,
            name: "w".into(),
            root: "/w".into(),
            crates: vec![mk("b", 1), mk("a", 3), mk("c", 3)],
        };
        s.sort(SortKey::Files);
        // Descending by files; a and c tie at 3, name breaks the tie.
        let order: Vec<&str> = s.crates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(order, ["a", "c", "b"]);
        s.sort(SortKey::Name);
        let order: Vec<&str> = s.crates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(order, ["a", "b", "c"]);
    }
}
