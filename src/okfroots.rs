// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! OKF **content roots**: which directories `ct-okf` treats as knowledge
//! bundles, how they are discovered, and how their concept files are fed to the
//! [`crate::okfindex`] search index.
//!
//! A directory is a content root if **any** of three signals holds, so a user
//! can adopt whichever is convenient and they interoperate:
//!
//! 1. a `.okf` **marker file** in the directory (our convention — it may be
//!    empty, or carry optional JSONC directives);
//! 2. a bundle-root `index.md` declaring `okf_version` — the only root signal
//!    the OKF standard itself defines;
//! 3. an entry in the project config `.ct/okf.jsonc` (the explicit list managed
//!    by `ct okf roots add/rm` and `ct okf roots scan --write`).
//!
//! All three converge on the same set; the config is the durable record. Paths
//! are anchored at the **project root** — the nearest ancestor holding `.ct`
//! (reusing [`crate::rules::discover_root`]) — and the index lives under
//! `.ct/okf/`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::okf;
use crate::okfindex::{DocSource, FileStat};

/// The project config file, under `.ct/`.
pub const CONFIG_FILE: &str = "okf.jsonc";
/// The per-directory root marker (our convention; OKF defines no marker).
pub const MARKER_FILE: &str = ".okf";
/// The index directory name, under `.ct/`.
pub const INDEX_DIR: &str = "okf";

/// The project root for `start`: the nearest ancestor containing `.ct`, else
/// `start` itself (so the tools still work in a directory without a `.ct`).
pub fn project_root(start: &Path) -> PathBuf {
    crate::rules::discover_root(start).unwrap_or_else(|| start.to_path_buf())
}

/// Path to `.ct/okf.jsonc` under `project`.
pub fn config_path(project: &Path) -> PathBuf {
    project.join(".ct").join(CONFIG_FILE)
}

/// Path to the index directory `.ct/okf/` under `project`.
pub fn index_dir(project: &Path) -> PathBuf {
    project.join(".ct").join(INDEX_DIR)
}

/// Normalize `dir` to a project-relative, `/`-separated key (the form stored in
/// config and used as a stable identity). An absolute or already-relative path
/// that is not under `project` is returned cleaned but unchanged in spirit.
pub fn rel_key(project: &Path, dir: &Path) -> String {
    let rel = dir.strip_prefix(project).unwrap_or(dir);
    let s = rel.to_string_lossy().replace('\\', "/");
    let s = s.trim_matches('/');
    if s.is_empty() {
        ".".to_string()
    } else {
        s.to_string()
    }
}

// ----- Config -------------------------------------------------------------------------

/// The `.ct/okf.jsonc` project config: the explicit list of content roots
/// (project-relative keys).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub roots: Vec<String>,
}

impl Config {
    /// Load the config, or an empty one when the file is absent. A malformed
    /// file is an error (so a typo is surfaced, not silently ignored).
    pub fn load(project: &Path) -> Result<Config, String> {
        let path = config_path(project);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return Ok(Config::default()),
        };
        let value =
            jsonc_parser::parse_to_serde_value(&text, &jsonc_parser::ParseOptions::default())
                .map_err(|e| format!("{}: {e}", path.display()))?
                .ok_or_else(|| format!("{}: empty config", path.display()))?;
        let obj = value
            .as_object()
            .ok_or_else(|| format!("{}: config root must be an object", path.display()))?;
        let mut roots = Vec::new();
        if let Some(arr) = obj.get("roots").and_then(|v| v.as_array()) {
            for r in arr {
                if let Some(s) = r.as_str() {
                    roots.push(s.trim_matches('/').to_string());
                }
            }
        }
        Ok(Config { roots })
    }

    /// Write the config (creating `.ct/` if needed), sorted and de-duplicated.
    pub fn save(&self, project: &Path) -> Result<(), String> {
        let mut roots: Vec<String> = self.roots.clone();
        roots.sort();
        roots.dedup();
        let path = config_path(project);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let value = serde_json::json!({ "roots": roots });
        let text = format!(
            "// OKF content roots for this project, managed by `ct okf roots`.\n{}\n",
            serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
        );
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Add `key` (a project-relative root). Returns whether it was newly added.
    pub fn add(&mut self, key: &str) -> bool {
        if self.roots.iter().any(|r| r == key) {
            false
        } else {
            self.roots.push(key.to_string());
            true
        }
    }

    /// Remove `key`. Returns whether it was present.
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.roots.len();
        self.roots.retain(|r| r != key);
        self.roots.len() != before
    }
}

// ----- Detection ----------------------------------------------------------------------

/// How a root was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Detection {
    /// Listed in `.ct/okf.jsonc`.
    Config,
    /// Has a `.okf` marker file.
    Marker,
    /// Has a bundle-root `index.md` declaring `okf_version`.
    OkfVersion,
}

impl Detection {
    pub fn label(self) -> &'static str {
        match self {
            Detection::Config => "config",
            Detection::Marker => "marker",
            Detection::OkfVersion => "okf_version",
        }
    }
}

/// A detected content root: its absolute directory, project-relative key, and
/// the signals that flagged it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub dir: PathBuf,
    pub key: String,
    pub via: Vec<Detection>,
}

/// Whether `dir` has a bundle-root `index.md` carrying `okf_version` frontmatter.
fn has_okf_version_index(dir: &Path) -> bool {
    let p = dir.join("index.md");
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| okf::parse(&t))
        .is_some_and(|parsed| parsed.fm.extra.contains_key("okf_version"))
}

/// Whether `dir` directly contains at least one OKF **concept** — a non-reserved
/// `.md` whose frontmatter carries a non-empty `type`.
fn has_concept(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if okf::is_reserved(name) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path)
            && okf::parse(&text)
                .is_some_and(|p| p.fm.type_.as_deref().is_some_and(|t| !t.trim().is_empty()))
        {
            return true;
        }
    }
    false
}

/// Build a `.md`-restricted directory walker for `dir`, including dot-entries so
/// `.okf` markers are visible, and honouring ignore files (the index should not
/// cover what the VCS ignores).
fn walk(dir: &Path) -> impl Iterator<Item = PathBuf> {
    ignore::WalkBuilder::new(dir)
        .hidden(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
}

/// Detect every content root in `project`: the union of config entries, `.okf`
/// markers, and `okf_version` index files. Sorted by key; each carries the
/// signals that flagged it.
pub fn detect(project: &Path) -> Result<Vec<Root>, String> {
    use std::collections::BTreeMap;
    let mut found: BTreeMap<String, (PathBuf, BTreeSet<Detection>)> = BTreeMap::new();
    let note = |dir: PathBuf,
                via: Detection,
                found: &mut BTreeMap<String, (PathBuf, BTreeSet<Detection>)>| {
        let key = rel_key(project, &dir);
        found
            .entry(key)
            .or_insert_with(|| (dir, BTreeSet::new()))
            .1
            .insert(via);
    };

    // 1) Config entries.
    for key in Config::load(project)?.roots {
        let dir = project.join(&key);
        note(dir, Detection::Config, &mut found);
    }
    // 2) & 3) Markers and okf_version index files, found by one walk.
    for path in walk(project) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name == MARKER_FILE
            && let Some(parent) = path.parent()
        {
            note(parent.to_path_buf(), Detection::Marker, &mut found);
        } else if name == "index.md"
            && let Some(parent) = path.parent()
            && has_okf_version_index(parent)
        {
            note(parent.to_path_buf(), Detection::OkfVersion, &mut found);
        }
    }

    Ok(found
        .into_iter()
        .map(|(key, (dir, via))| Root {
            dir,
            key,
            via: via.into_iter().collect(),
        })
        .collect())
}

/// Heuristically derive candidate roots in `project` for onboarding: the
/// top-most directories that either declare `okf_version` or directly contain an
/// OKF concept. A directory is dropped when an ancestor already qualifies, so
/// nested concept folders collapse into their bundle root.
pub fn scan_candidates(project: &Path) -> Vec<PathBuf> {
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for path in walk(project) {
        let Some(parent) = path.parent() else {
            continue;
        };
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let qualifies = (name == "index.md" && has_okf_version_index(parent))
            || (path.extension().and_then(|e| e.to_str()) == Some("md")
                && !okf::is_reserved(name)
                && has_concept(parent));
        if qualifies {
            dirs.insert(parent.to_path_buf());
        }
    }
    // Collapse to top-most: drop any dir that has an ancestor in the set.
    let all: Vec<PathBuf> = dirs.iter().cloned().collect();
    all.iter()
        .filter(|d| !all.iter().any(|a| *d != a && d.starts_with(a)))
        .cloned()
        .collect()
}

/// Create an empty `.okf` marker in `dir` (no-op if one already exists).
pub fn write_marker(dir: &Path) -> Result<(), String> {
    let path = dir.join(MARKER_FILE);
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&path, "// OKF content root marker.\n")
        .map_err(|e| format!("{}: {e}", path.display()))
}

// ----- Feeding the index --------------------------------------------------------------

/// Nanoseconds since the Unix epoch for a file's mtime (0 if unavailable).
fn mtime_ns(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// The live concept files across `roots`: every non-reserved `.md`, as a
/// [`FileStat`] keyed by project-relative path. De-duplicated when roots
/// overlap. The result drives [`crate::okfindex::Index::update`].
pub fn concept_files(project: &Path, roots: &[PathBuf]) -> Vec<FileStat> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for root in roots {
        for path in walk(root) {
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if okf::is_reserved(name) {
                continue;
            }
            let key = rel_key(project, &path);
            if !seen.insert(key.clone()) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            out.push(FileStat {
                key,
                path,
                mtime_ns: mtime_ns(&meta),
                size: meta.len(),
            });
        }
    }
    out
}

/// Read one concept file into a [`DocSource`] for indexing: title/type/tags from
/// frontmatter (title falling back to the file stem), and a searchable text of
/// the description, resource, and body.
pub fn load_doc(path: &Path) -> Result<DocSource, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let parsed = okf::parse(&text);
    let (fm, body) = match &parsed {
        Some(p) => {
            let start = p.body_start_line.saturating_sub(1);
            let body = text.lines().skip(start).collect::<Vec<_>>().join("\n");
            (p.fm.clone(), body)
        }
        None => (okf::Frontmatter::default(), text.clone()),
    };
    let mut searchable = String::new();
    for part in [
        fm.description.as_deref(),
        fm.resource.as_deref(),
        Some(body.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        searchable.push_str(part);
        searchable.push(' ');
    }
    Ok(DocSource {
        title: fm.title.unwrap_or(stem),
        type_: fm.type_.unwrap_or_default(),
        tags: fm.tags,
        text: searchable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TAG: AtomicU32 = AtomicU32::new(0);

    fn scratch() -> PathBuf {
        let n = TAG.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ct-okfroots-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".ct")).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn concept(type_: &str, title: &str) -> String {
        format!(
            "---\ntype: {type_}\ntitle: {title}\ndescription: about {title}\n---\n# {title}\nbody text\n"
        )
    }

    #[test]
    fn rel_key_normalizes_under_project() {
        let project = Path::new("/proj");
        assert_eq!(rel_key(project, Path::new("/proj/docs/kb")), "docs/kb");
        assert_eq!(rel_key(project, Path::new("/proj")), ".");
    }

    #[test]
    fn detects_roots_via_marker_okf_version_and_config() {
        let p = scratch();
        // A marker root.
        write(&p.join("kb1/a.md"), &concept("Note", "Alpha"));
        write(&p.join("kb1/.okf"), "");
        // An okf_version index root.
        write(
            &p.join("kb2/index.md"),
            "---\nokf_version: \"0.1\"\n---\n# Index\n",
        );
        write(&p.join("kb2/b.md"), &concept("Note", "Beta"));
        // A config-only root.
        write(&p.join("kb3/c.md"), &concept("Note", "Gamma"));
        Config {
            roots: vec!["kb3".to_string()],
        }
        .save(&p)
        .unwrap();

        let roots = detect(&p).unwrap();
        let keys: Vec<&str> = roots.iter().map(|r| r.key.as_str()).collect();
        assert!(keys.contains(&"kb1"), "{keys:?}");
        assert!(keys.contains(&"kb2"), "{keys:?}");
        assert!(keys.contains(&"kb3"), "{keys:?}");
        let kb1 = roots.iter().find(|r| r.key == "kb1").unwrap();
        assert!(kb1.via.contains(&Detection::Marker));
        let kb2 = roots.iter().find(|r| r.key == "kb2").unwrap();
        assert!(kb2.via.contains(&Detection::OkfVersion));
        let kb3 = roots.iter().find(|r| r.key == "kb3").unwrap();
        assert!(kb3.via.contains(&Detection::Config));
    }

    #[test]
    fn scan_collapses_nested_concept_dirs_to_topmost() {
        let p = scratch();
        write(
            &p.join("kb/index.md"),
            "---\nokf_version: \"0.1\"\n---\n# Index\n",
        );
        write(&p.join("kb/a.md"), &concept("Note", "A"));
        write(&p.join("kb/sub/b.md"), &concept("Note", "B"));
        let cands = scan_candidates(&p);
        // Only the top-most "kb" qualifies; "kb/sub" collapses into it.
        assert_eq!(cands.len(), 1, "{cands:?}");
        assert!(cands[0].ends_with("kb"));
    }

    #[test]
    fn config_roundtrips_and_dedups() {
        let p = scratch();
        let mut cfg = Config::default();
        assert!(cfg.add("docs/kb"));
        assert!(!cfg.add("docs/kb")); // already present
        assert!(cfg.add("notes"));
        cfg.save(&p).unwrap();
        let loaded = Config::load(&p).unwrap();
        assert_eq!(
            loaded.roots,
            vec!["docs/kb".to_string(), "notes".to_string()]
        );
        let mut loaded = loaded;
        assert!(loaded.remove("notes"));
        assert!(!loaded.remove("notes"));
    }

    #[test]
    fn concept_files_lists_md_excluding_reserved() {
        let p = scratch();
        write(&p.join("kb/a.md"), &concept("Note", "A"));
        write(&p.join("kb/b.md"), &concept("Note", "B"));
        write(
            &p.join("kb/index.md"),
            "---\nokf_version: \"0.1\"\n---\n# Index\n",
        );
        write(&p.join("kb/log.md"), "# Log\n");
        let files = concept_files(&p, &[p.join("kb")]);
        let mut keys: Vec<&str> = files.iter().map(|f| f.key.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["kb/a.md", "kb/b.md"]); // index.md/log.md excluded
    }

    #[test]
    fn load_doc_extracts_frontmatter_and_body() {
        let p = scratch();
        let path = p.join("kb/customers.md");
        write(
            &path,
            "---\ntype: BigQuery Table\ntitle: Customers\ndescription: the customer dimension\ntags: [core, pii]\n---\n# Customers\nrow-per-customer.\n",
        );
        let doc = load_doc(&path).unwrap();
        assert_eq!(doc.title, "Customers");
        assert_eq!(doc.type_, "BigQuery Table");
        assert_eq!(doc.tags, vec!["core".to_string(), "pii".to_string()]);
        assert!(doc.text.contains("customer dimension"));
        assert!(doc.text.contains("row-per-customer"));
    }

    #[test]
    fn project_root_walks_up_to_ct() {
        let p = scratch(); // holds .ct
        let deep = p.join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(project_root(&deep), p);

        // With no `.ct` above it, discovery falls back to the start directory.
        let n = TAG.fetch_add(1, Ordering::Relaxed);
        let lone =
            std::env::temp_dir().join(format!("ct-okfroots-lone-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lone);
        std::fs::create_dir_all(&lone).unwrap();
        assert_eq!(project_root(&lone), lone);
    }

    #[test]
    fn write_marker_is_idempotent_and_detected() {
        let p = scratch();
        let kb = p.join("kb");
        std::fs::create_dir_all(&kb).unwrap();
        write_marker(&kb).unwrap();
        assert!(kb.join(MARKER_FILE).is_file());
        write_marker(&kb).unwrap(); // second call is a no-op, not an error
        let roots = detect(&p).unwrap();
        assert!(
            roots
                .iter()
                .any(|r| r.key == "kb" && r.via.contains(&Detection::Marker)),
            "{roots:?}"
        );
    }

    #[test]
    fn concept_files_respects_ignore_files() {
        let p = scratch();
        write(&p.join("kb/a.md"), &concept("Note", "A"));
        write(&p.join("kb/skip/b.md"), &concept("Note", "B"));
        // A `.ignore` file (honored by the walker without requiring git) hides skip/.
        write(&p.join("kb/.ignore"), "skip/\n");
        let files = concept_files(&p, &[p.join("kb")]);
        let keys: Vec<&str> = files.iter().map(|f| f.key.as_str()).collect();
        assert!(keys.contains(&"kb/a.md"), "{keys:?}");
        assert!(
            !keys.iter().any(|k| k.contains("skip/b.md")),
            "ignored file indexed: {keys:?}"
        );
    }
}
