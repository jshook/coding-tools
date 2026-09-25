// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `ct-okf --script` engine: a batch of OKF mutations applied under the
//! prepare/confirm/write standard.
//!
//! A `.ctb` block document (parsed by [`crate::blockdoc`]) lists `new`/`adopt`/
//! `set`/`log`/`index`/`init` items. [`simulate`] runs the whole batch in memory over
//! a [`Vfs`] overlay — in script order, under cascade, so a later `index` sees a
//! concept an earlier `new` created and a later `set` edits an earlier one — and
//! returns the complete set of pending writes. The caller writes them only when
//! every op succeeded; any failing op aborts the batch with nothing written.
//!
//! The overlay reads through a [`Disk`] for files it has not (yet) written, so
//! the engine is pure with respect to a supplied disk and can be unit-tested
//! against an in-memory one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::blockdoc::Item;
use crate::okf;

/// The directives that open an item in an okf script.
pub const ITEM_NAMES: &[&str] = &["new", "adopt", "set", "log", "index", "init"];

/// The read surface the [`Vfs`] overlays writes on top of.
pub trait Disk {
    /// The file's text, or `None` if it does not exist / is unreadable.
    fn read(&self, path: &Path) -> Option<String>;
    /// Whether a file exists at `path`.
    fn exists(&self, path: &Path) -> bool;
    /// The `.md` files directly inside `dir` (non-recursive).
    fn list_md(&self, dir: &Path) -> Vec<PathBuf>;
    /// The subdirectories directly inside `dir`, dot-directories excluded.
    fn list_dirs(&self, dir: &Path) -> Vec<PathBuf>;
}

/// A real filesystem [`Disk`].
pub struct FsDisk;

impl Disk for FsDisk {
    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn list_md(&self, dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("md") {
                    out.push(p);
                }
            }
        }
        out
    }
    fn list_dirs(&self, dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                let dot = entry.file_name().to_string_lossy().starts_with('.');
                if p.is_dir() && !dot {
                    out.push(p);
                }
            }
        }
        out
    }
}

/// A copy-on-write view of a [`Disk`]: reads fall through to disk, writes are
/// held in an overlay until the batch is confirmed.
struct Vfs<'a> {
    disk: &'a dyn Disk,
    overlay: BTreeMap<PathBuf, String>,
}

impl<'a> Vfs<'a> {
    fn new(disk: &'a dyn Disk) -> Self {
        Vfs {
            disk,
            overlay: BTreeMap::new(),
        }
    }
    fn read(&self, path: &Path) -> Option<String> {
        self.overlay
            .get(path)
            .cloned()
            .or_else(|| self.disk.read(path))
    }
    fn exists(&self, path: &Path) -> bool {
        self.overlay.contains_key(path) || self.disk.exists(path)
    }
    fn write(&mut self, path: PathBuf, content: String) {
        self.overlay.insert(path, content);
    }
    /// `.md` files directly in `dir`, merging disk entries with overlay writes.
    fn list_md(&self, dir: &Path) -> Vec<PathBuf> {
        let mut set: BTreeSet<PathBuf> = self.disk.list_md(dir).into_iter().collect();
        for key in self.overlay.keys() {
            if key.parent() == Some(dir) && key.extension().and_then(|x| x.to_str()) == Some("md") {
                set.insert(key.clone());
            }
        }
        set.into_iter().collect()
    }
    /// Subdirectories directly in `dir`, including those that exist only
    /// because the overlay holds a file beneath them.
    fn list_dirs(&self, dir: &Path) -> Vec<PathBuf> {
        let mut set: BTreeSet<PathBuf> = self.disk.list_dirs(dir).into_iter().collect();
        for key in self.overlay.keys() {
            let Ok(rest) = key.strip_prefix(dir) else {
                continue;
            };
            let mut comps = rest.components();
            if let (Some(first), Some(_)) = (comps.next(), comps.next()) {
                set.insert(dir.join(first));
            }
        }
        set.into_iter().collect()
    }
}

/// One compiled okf mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OkfOp {
    New {
        file: String,
        type_: String,
        title: Option<String>,
        description: Option<String>,
        tags: Vec<String>,
        body: Option<String>,
    },
    Adopt {
        file: String,
        type_: String,
    },
    Set {
        file: String,
        field: String,
        value: String,
    },
    Log {
        base: Option<String>,
        kind: String,
        message: String,
    },
    Index {
        base: Option<String>,
        recursive: bool,
    },
    Init {
        base: Option<String>,
    },
}

/// One script op with its source position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpSpec {
    pub ordinal: usize,
    pub line: usize,
    pub op: OkfOp,
}

/// One simulated write or no-op, for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub ordinal: usize,
    pub verb: String,
    /// Path relative to the engine base (for stable, portable reporting).
    pub path: String,
    /// `create` / `update` / `add` / `present`, etc.
    pub effect: String,
}

/// The fully-simulated batch: per-op actions and the pending writes (each path
/// relative to the engine base, paired with its final content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<Action>,
    pub writes: Vec<(PathBuf, String)>,
}

/// Validate an item's attributes/sections against the allowed vocabulary.
fn check_vocab(
    item: &Item,
    ordinal: usize,
    attrs: &[&str],
    sections: &[&str],
) -> Result<(), String> {
    let at = |msg: String| format!("op {ordinal} (script line {}): {msg}", item.line);
    for (k, _) in &item.attrs {
        if !attrs.contains(&k.as_str()) {
            return Err(at(format!(
                "unknown attribute '{k}' for '{}' (allowed: {})",
                item.directive,
                attrs.join(", ")
            )));
        }
    }
    for (k, _) in &item.sections {
        if !sections.contains(&k.as_str()) {
            let allowed = if sections.is_empty() {
                "none".to_string()
            } else {
                sections.join(", ")
            };
            return Err(at(format!(
                "unknown section '{k}' for '{}' (allowed: {allowed})",
                item.directive
            )));
        }
    }
    Ok(())
}

/// Compile parsed [`Item`]s into [`OpSpec`]s, validating each.
pub fn compile(items: &[Item]) -> Result<Vec<OpSpec>, String> {
    let mut specs = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let ordinal = i + 1;
        let at = |msg: String| format!("op {ordinal} (script line {}): {msg}", item.line);
        let req_attr = |key: &str| {
            item.attr(key)
                .map(str::to_string)
                .ok_or_else(|| at(format!("missing required '{key}='")))
        };
        let op = match item.directive.as_str() {
            "new" => {
                check_vocab(
                    item,
                    ordinal,
                    &["file", "type", "title"],
                    &["description", "tags", "body"],
                )?;
                let tags = item
                    .section("tags")
                    .map(|s| {
                        s.lines()
                            .map(str::trim)
                            .filter(|l| !l.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                OkfOp::New {
                    file: req_attr("file")?,
                    type_: req_attr("type")?,
                    title: item.attr("title").map(str::to_string),
                    description: item
                        .section("description")
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                    tags,
                    body: item.section("body").map(str::to_string),
                }
            }
            "adopt" => {
                check_vocab(item, ordinal, &["file", "type"], &[])?;
                OkfOp::Adopt {
                    file: req_attr("file")?,
                    type_: req_attr("type")?,
                }
            }
            "set" => {
                check_vocab(item, ordinal, &["file", "field", "value"], &[])?;
                OkfOp::Set {
                    file: req_attr("file")?,
                    field: req_attr("field")?,
                    value: req_attr("value")?,
                }
            }
            "log" => {
                check_vocab(item, ordinal, &["base", "kind"], &["message"])?;
                let message = item
                    .section("message")
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| at("missing 'message' section".to_string()))?;
                OkfOp::Log {
                    base: item.attr("base").map(str::to_string),
                    kind: item.attr("kind").unwrap_or("Update").to_string(),
                    message,
                }
            }
            "index" => {
                check_vocab(item, ordinal, &["base", "recursive"], &[])?;
                let recursive = match item.attr("recursive") {
                    None | Some("false") => false,
                    Some("true") => true,
                    Some(other) => {
                        return Err(at(format!(
                            "recursive= must be true or false, got '{other}'"
                        )));
                    }
                };
                OkfOp::Index {
                    base: item.attr("base").map(str::to_string),
                    recursive,
                }
            }
            "init" => {
                check_vocab(item, ordinal, &["base"], &[])?;
                OkfOp::Init {
                    base: item.attr("base").map(str::to_string),
                }
            }
            other => return Err(at(format!("unknown directive '{other}'"))),
        };
        specs.push(OpSpec {
            ordinal,
            line: item.line,
            op,
        });
    }
    Ok(specs)
}

/// Render a path relative to `base` for reporting (falls back to the full path).
fn rel(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Every directory beneath `dir`, deepest first (post-order), `dir` excluded.
fn descendants(vfs: &Vfs, dir: &Path, out: &mut Vec<PathBuf>) {
    for sub in vfs.list_dirs(dir) {
        descendants(vfs, &sub, out);
        out.push(sub);
    }
}

/// The `index.md` entries for `dir` and how many are concepts: its concepts,
/// then each immediate subdirectory holding an `index.md`, linked as
/// `sub/index.md`. A concept is titled by [`okf::display_title`]; a
/// subdirectory by its index's `title`, else its index's heading unless that
/// is the generated default `Index`, else the directory name.
fn index_entries(vfs: &Vfs, dir: &Path) -> (Vec<(String, String, String)>, usize) {
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for p in vfs.list_md(dir) {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if okf::is_reserved(&name) {
            continue;
        }
        let text = vfs.read(&p).unwrap_or_default();
        let title = okf::display_title(&text, name.trim_end_matches(".md"));
        let desc = okf::parse(&text)
            .and_then(|x| x.fm.description)
            .unwrap_or_default();
        entries.push((name, title, desc));
    }
    let concepts = entries.len();
    for sub in vfs.list_dirs(dir) {
        let Some(text) = vfs.read(&sub.join("index.md")) else {
            continue;
        };
        let dname = sub
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let fm = okf::parse(&text).map(|p| p.fm).unwrap_or_default();
        let title = fm
            .title
            .filter(|t| !t.trim().is_empty())
            .or_else(|| okf::first_heading(&text).filter(|h| h != "Index"))
            .unwrap_or_else(|| dname.clone());
        entries.push((
            format!("{dname}/index.md"),
            title,
            fm.description.unwrap_or_default(),
        ));
    }
    (entries, concepts)
}

/// Regenerate `dir`'s `index.md` into the overlay, keeping the existing
/// index's frontmatter and heading (see [`okf::render_index`]).
fn write_index(vfs: &mut Vfs, base: &Path, dir: &Path, ordinal: usize, actions: &mut Vec<Action>) {
    let (entries, concepts) = index_entries(vfs, dir);
    let target = dir.join("index.md");
    let existing = vfs.read(&target);
    let mut effect = format!("{concepts} concept(s)");
    if entries.len() > concepts {
        effect.push_str(&format!(", {} section(s)", entries.len() - concepts));
    }
    actions.push(Action {
        ordinal,
        verb: "index".into(),
        path: rel(base, &target),
        effect,
    });
    vfs.write(target, okf::render_index(existing.as_deref(), &entries));
}

/// Simulate the whole batch over `disk`, rooted at `base`, stamping `today` into
/// new concepts and log entries. Returns the plan (actions + pending writes), or
/// the first op's error — in which case the caller writes nothing.
pub fn simulate(
    base: &Path,
    specs: &[OpSpec],
    disk: &dyn Disk,
    today: &str,
) -> Result<Plan, String> {
    let mut vfs = Vfs::new(disk);
    let mut actions = Vec::with_capacity(specs.len());
    for spec in specs {
        let at = |msg: String| format!("op {} (script line {}): {msg}", spec.ordinal, spec.line);
        match &spec.op {
            OkfOp::New {
                file,
                type_,
                title,
                description,
                tags,
                body,
            } => {
                let target = base.join(file);
                if vfs.exists(&target) {
                    return Err(at(format!("{file} already exists; refusing to overwrite")));
                }
                let title = title.clone().unwrap_or_else(|| {
                    target
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
                let content = okf::build_concept(
                    type_,
                    &title,
                    description.as_deref(),
                    tags,
                    today,
                    body.as_deref(),
                );
                actions.push(Action {
                    ordinal: spec.ordinal,
                    verb: "new".into(),
                    path: rel(base, &target),
                    effect: "create".into(),
                });
                vfs.write(target, content);
            }
            OkfOp::Adopt { file, type_ } => {
                let target = base.join(file);
                let name = target
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if okf::is_reserved(name) {
                    return Err(at(format!("{file} is a reserved file, not a concept")));
                }
                let text = vfs
                    .read(&target)
                    .ok_or_else(|| at(format!("no such file: {file}")))?;
                let stem = target
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let (new_text, title) = okf::adopt(&text, type_, &stem, today)
                    .map_err(|e| at(format!("{file}: {e}")))?;
                actions.push(Action {
                    ordinal: spec.ordinal,
                    verb: "adopt".into(),
                    path: rel(base, &target),
                    effect: format!("title {title:?}"),
                });
                vfs.write(target, new_text);
            }
            OkfOp::Set { file, field, value } => {
                let target = base.join(file);
                let text = vfs
                    .read(&target)
                    .ok_or_else(|| at(format!("no such concept: {file}")))?;
                let (new_text, replaced) =
                    okf::set_field(&text, field, value).map_err(|e| at(format!("{file}: {e}")))?;
                actions.push(Action {
                    ordinal: spec.ordinal,
                    verb: "set".into(),
                    path: rel(base, &target),
                    effect: if replaced { "update" } else { "add" }.into(),
                });
                vfs.write(target, new_text);
            }
            OkfOp::Index {
                base: sub,
                recursive,
            } => {
                let dir = sub
                    .as_ref()
                    .map(|s| base.join(s))
                    .unwrap_or(base.to_path_buf());
                // Children before parents, so a parent lists the index a
                // child gained earlier in this same op.
                let mut dirs = Vec::new();
                if *recursive {
                    descendants(&vfs, &dir, &mut dirs);
                }
                for d in dirs {
                    if !index_entries(&vfs, &d).0.is_empty() {
                        write_index(&mut vfs, base, &d, spec.ordinal, &mut actions);
                    }
                }
                write_index(&mut vfs, base, &dir, spec.ordinal, &mut actions);
            }
            OkfOp::Log {
                base: sub,
                kind,
                message,
            } => {
                let dir = sub
                    .as_ref()
                    .map(|s| base.join(s))
                    .unwrap_or(base.to_path_buf());
                let target = dir.join("log.md");
                let existing = vfs.read(&target).unwrap_or_default();
                let updated = okf::log_entry(&existing, today, kind, message);
                actions.push(Action {
                    ordinal: spec.ordinal,
                    verb: "log".into(),
                    path: rel(base, &target),
                    effect: kind.clone(),
                });
                vfs.write(target, updated);
            }
            OkfOp::Init { base: sub } => {
                let dir = sub
                    .as_ref()
                    .map(|s| base.join(s))
                    .unwrap_or(base.to_path_buf());
                let target = dir.join("index.md");
                if vfs.exists(&target) {
                    actions.push(Action {
                        ordinal: spec.ordinal,
                        verb: "init".into(),
                        path: rel(base, &target),
                        effect: "present".into(),
                    });
                } else {
                    actions.push(Action {
                        ordinal: spec.ordinal,
                        verb: "init".into(),
                        path: rel(base, &target),
                        effect: "create".into(),
                    });
                    vfs.write(target, "---\nokf_version: \"0.1\"\n---\n\n# Index\n".into());
                }
            }
        }
    }
    let writes = vfs.overlay.into_iter().collect();
    Ok(Plan { actions, writes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockdoc::{DEFAULT_FENCE, parse};
    use std::cell::RefCell;

    /// An in-memory disk for deterministic, filesystem-free tests.
    #[derive(Default)]
    struct MemDisk {
        files: RefCell<BTreeMap<PathBuf, String>>,
    }
    impl MemDisk {
        fn with(files: &[(&str, &str)]) -> Self {
            let m = MemDisk::default();
            for (p, c) in files {
                m.files.borrow_mut().insert(PathBuf::from(p), c.to_string());
            }
            m
        }
    }
    impl Disk for MemDisk {
        fn read(&self, path: &Path) -> Option<String> {
            self.files.borrow().get(path).cloned()
        }
        fn exists(&self, path: &Path) -> bool {
            self.files.borrow().contains_key(path)
        }
        fn list_md(&self, dir: &Path) -> Vec<PathBuf> {
            self.files
                .borrow()
                .keys()
                .filter(|p| {
                    p.parent() == Some(dir) && p.extension().and_then(|x| x.to_str()) == Some("md")
                })
                .cloned()
                .collect()
        }
        fn list_dirs(&self, dir: &Path) -> Vec<PathBuf> {
            let mut set = BTreeSet::new();
            for key in self.files.borrow().keys() {
                let Ok(rest) = key.strip_prefix(dir) else {
                    continue;
                };
                let mut comps = rest.components();
                if let (Some(first), Some(_)) = (comps.next(), comps.next()) {
                    set.insert(dir.join(first));
                }
            }
            set.into_iter().collect()
        }
    }

    fn plan(base: &str, doc: &str, disk: &dyn Disk) -> Result<Plan, String> {
        let items = parse(doc, DEFAULT_FENCE, ITEM_NAMES)?;
        let specs = compile(&items)?;
        simulate(Path::new(base), &specs, disk, "2026-06-27")
    }

    #[test]
    fn cascade_new_then_index_then_set_then_log() {
        let disk = MemDisk::default();
        let doc = "\
#% new file=a.md type=Note title=A
#% new file=b.md type=Note title=B
#% description
The B note.
#% index
#% set file=a.md field=timestamp value=2026-06-27
#% log kind=Creation
#% message
added a and b
";
        let p = plan("/bundle", doc, &disk).unwrap();
        let writes: BTreeMap<_, _> = p.writes.into_iter().collect();
        // index.md sees both new concepts (cascade), with B's description.
        let idx = &writes[&PathBuf::from("/bundle/index.md")];
        assert!(idx.contains("[A](a.md)"), "{idx}");
        assert!(idx.contains("[B](b.md) - The B note."), "{idx}");
        // set edited the freshly-created a.md (timestamp added before fence).
        let a = &writes[&PathBuf::from("/bundle/a.md")];
        assert!(a.contains("timestamp: 2026-06-27"), "{a}");
        // log.md got the dated entry.
        let log = &writes[&PathBuf::from("/bundle/log.md")];
        assert!(log.contains("**Creation**: added a and b"), "{log}");
    }

    #[test]
    fn new_refuses_to_clobber_disk_or_overlay() {
        let disk = MemDisk::with(&[("/b/exists.md", "---\ntype: X\n---\n")]);
        // Clobbering an on-disk file aborts the whole batch.
        let err = plan("/b", "#% new file=exists.md type=Note\n", &disk).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        // Clobbering a file created earlier in the same batch also aborts.
        let dup = "#% new file=x.md type=Note\n#% new file=x.md type=Note\n";
        assert!(
            plan("/b", dup, &disk)
                .unwrap_err()
                .contains("already exists")
        );
    }

    #[test]
    fn index_lists_sections_and_recurses_deepest_first() {
        let disk = MemDisk::with(&[
            ("/b/a.md", "# Alpha\n\nNo frontmatter.\n"),
            ("/b/design/d.md", "---\ntype: design\ntitle: D\n---\n"),
            ("/b/design/deep/e.md", "---\ntype: design\n---\n# Echo\n"),
            ("/b/empty/notes.txt", "not markdown"),
        ]);
        let p = plan("/b", "#% index recursive=true\n", &disk).unwrap();
        let order: Vec<&str> = p.actions.iter().map(|a| a.path.as_str()).collect();
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            order,
            [
                format!("design{sep}deep{sep}index.md"),
                format!("design{sep}index.md"),
                "index.md".to_string(),
            ],
            "a directory without Markdown gets no index"
        );
        let writes: BTreeMap<_, _> = p.writes.into_iter().collect();
        let design = &writes[&PathBuf::from("/b/design/index.md")];
        assert!(design.contains("* [D](d.md)"), "{design}");
        assert!(design.contains("* [deep](deep/index.md)"), "{design}");
        let deep = &writes[&PathBuf::from("/b/design/deep/index.md")];
        assert!(deep.contains("* [Echo](e.md)"), "heading fallback: {deep}");
        let root = &writes[&PathBuf::from("/b/index.md")];
        assert_eq!(
            root,
            "# Index\n\n* [Alpha](a.md)\n* [design](design/index.md)\n"
        );
    }

    #[test]
    fn index_keeps_frontmatter_and_titles_sections_by_their_index() {
        let disk = MemDisk::with(&[
            ("/b/index.md", "---\nokf_version: \"0.1\"\n---\n\n# Index\n"),
            (
                "/b/guides/index.md",
                "---\ntitle: User guides\ndescription: How to.\n---\n# Index\n",
            ),
            ("/b/tutorials/index.md", "# Tutorials\n"),
        ]);
        let p = plan("/b", "#% index\n", &disk).unwrap();
        let writes: BTreeMap<_, _> = p.writes.into_iter().collect();
        assert_eq!(
            writes[&PathBuf::from("/b/index.md")],
            "---\nokf_version: \"0.1\"\n---\n\n# Index\n\n\
             * [User guides](guides/index.md) - How to.\n\
             * [Tutorials](tutorials/index.md)\n"
        );
    }

    #[test]
    fn adopt_then_tag_then_index_in_one_batch() {
        let disk = MemDisk::with(&[("/b/g.md", "# Guide one\nBody.\n")]);
        let doc = "\
#% adopt file=g.md type=guide
#% set file=g.md field=tags value=intro,core
#% index
";
        let p = plan("/b", doc, &disk).unwrap();
        let writes: BTreeMap<_, _> = p.writes.into_iter().collect();
        let g = &writes[&PathBuf::from("/b/g.md")];
        assert_eq!(
            g,
            "---\ntype: guide\ntitle: Guide one\ntimestamp: 2026-06-27\ntags: [intro, core]\n---\n\n# Guide one\nBody.\n"
        );
        assert!(writes[&PathBuf::from("/b/index.md")].contains("[Guide one](g.md)"));
    }

    #[test]
    fn adopt_refuses_frontmatter_reserved_and_missing_files() {
        let disk = MemDisk::with(&[
            ("/b/has.md", "---\ntype: x\n---\n"),
            ("/b/README.md", "# Landing\n"),
        ]);
        for (doc, want) in [
            (
                "#% adopt file=has.md type=guide\n",
                "already has frontmatter",
            ),
            ("#% adopt file=README.md type=guide\n", "reserved file"),
            ("#% adopt file=ghost.md type=guide\n", "no such file"),
        ] {
            let err = plan("/b", doc, &disk).unwrap_err();
            assert!(err.contains(want), "{doc}: {err}");
        }
    }

    #[test]
    fn set_on_a_missing_concept_aborts() {
        let disk = MemDisk::default();
        let err = plan("/b", "#% set file=ghost.md field=x value=y\n", &disk).unwrap_err();
        assert!(err.contains("no such concept"), "{err}");
    }

    #[test]
    fn unknown_attribute_is_rejected() {
        let disk = MemDisk::default();
        let err = plan("/b", "#% new file=a.md type=Note bogus=1\n", &disk).unwrap_err();
        assert!(err.contains("unknown attribute 'bogus'"), "{err}");
    }

    #[test]
    fn missing_required_attribute_is_rejected() {
        let disk = MemDisk::default();
        let err = plan("/b", "#% new title=A\n", &disk).unwrap_err();
        assert!(err.contains("missing required 'file='"), "{err}");
    }
}
