// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! A lazily-maintained full-text index over OKF concept files, built on
//! BurntSushi's [`fst`] crate — the same immutable finite-state-transducer
//! machinery search engines like tantivy use for their term dictionaries.
//!
//! # Why this shape
//!
//! The index is a set of **immutable segments**. Each incremental update writes
//! one *new* segment for the batch of changed files and never rewrites the
//! existing ones — so "layering in new content" costs only the new segment,
//! exactly the property the suite wanted. Superseded or removed documents are
//! recorded as **tombstones** in the manifest and filtered at query time;
//! [`Index::condense`] later merges every segment into one and drops the
//! tombstones, reclaiming space without re-reading the source files.
//!
//! A segment is two files: `seg-NNNNN.fst`, an [`fst::Map`] from **term** to a
//! byte offset, and `seg-NNNNN.pos`, the postings blob those offsets point into
//! (a varint-encoded `(doc_id, term_frequency)` list per term). Global state —
//! the next document id, the live segment list, the per-file `(doc, mtime,
//! size)` records that drive staleness, per-document metadata, and the tombstone
//! set — lives in `manifest.json`. Segment bytes are read into memory on demand
//! rather than memory-mapped, keeping the dependency surface to just `fst`.
//!
//! # Query modes
//!
//! [`Index::search`] understands a small query grammar (see [`QueryTerm`]):
//! plain terms (exact), `term*` (prefix), `term~`/`term~2` (Levenshtein fuzzy),
//! and `/regex/`. Exact, prefix, and fuzzy are native fst automata; the regex
//! mode drives a `regex-automata` dense DFA *as* an [`fst::Automaton`] (the
//! modern equivalent of the `transducer` feature dropped from regex-automata
//! 0.4), so it prunes the term FST during traversal instead of scanning it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use fst::automaton::{Levenshtein, Str};
use fst::{Automaton, IntoStreamer, Map, MapBuilder, Streamer};
// Drives a regex-automata 0.4 dense DFA as an `fst::Automaton` (the modern
// equivalent of the `transducer` feature dropped from regex-automata 0.4).
use regex_automata::Anchored;
use regex_automata::dfa::Automaton as _;
use regex_automata::dfa::{StartKind, dense};
use regex_automata::util::primitives::StateID;

/// Manifest format version, bumped on any incompatible on-disk change.
const MANIFEST_VERSION: u64 = 1;

/// Maximum edit distance honoured for a `~N` fuzzy query (Levenshtein automata
/// grow quickly with distance; 2 is the usual practical ceiling).
const MAX_FUZZY: u32 = 2;

// ----- Tokenization -------------------------------------------------------------------

/// Split `text` into lowercased alphanumeric terms — the shared tokenizer for
/// both indexing and (per-token) querying. Deliberately minimal: Unicode
/// alphanumeric runs, lowercased, no stemming or stop-words, so behaviour is
/// predictable and dependency-free.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            cur.extend(ch.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ----- Varint postings ----------------------------------------------------------------

/// Append `val` to `buf` as an unsigned LEB128 varint.
fn put_uvarint(buf: &mut Vec<u8>, mut val: u64) {
    loop {
        let mut byte = (val & 0x7f) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if val == 0 {
            break;
        }
    }
}

/// Read an unsigned LEB128 varint from `buf` at `*pos`, advancing `*pos`.
fn get_uvarint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut val = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        val |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(val);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

/// Encode one term's postings (a `(doc_id, term_frequency)` list, doc ascending)
/// into `buf`, returning the byte offset the list begins at.
fn encode_postings(buf: &mut Vec<u8>, postings: &[(u64, u32)]) -> u64 {
    let offset = buf.len() as u64;
    put_uvarint(buf, postings.len() as u64);
    for &(doc, tf) in postings {
        put_uvarint(buf, doc);
        put_uvarint(buf, u64::from(tf));
    }
    offset
}

/// Decode the postings list stored at `offset` in a segment's `.pos` blob.
fn decode_postings(blob: &[u8], offset: u64) -> Vec<(u64, u32)> {
    let mut pos = offset as usize;
    let Some(n) = get_uvarint(blob, &mut pos) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let (Some(doc), Some(tf)) = (get_uvarint(blob, &mut pos), get_uvarint(blob, &mut pos))
        else {
            break;
        };
        out.push((doc, tf as u32));
    }
    out
}

// ----- The document feed --------------------------------------------------------------

/// A file's identity for staleness: its path-relative key plus the `mtime`/size
/// the indexer compares against the manifest to decide what to re-index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    /// Stable key for the file (e.g. project-relative path with `/` separators).
    pub key: String,
    /// Absolute path, for the loader to read when (re)indexing.
    pub path: PathBuf,
    /// Last-modified time in nanoseconds since the Unix epoch.
    pub mtime_ns: u64,
    /// File size in bytes.
    pub size: u64,
}

/// A document ready to index: its searchable text plus the metadata carried into
/// search results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocSource {
    /// Human title (frontmatter `title`, else the file stem).
    pub title: String,
    /// OKF `type`.
    pub type_: String,
    /// Tags.
    pub tags: Vec<String>,
    /// The full searchable text (frontmatter fields + body).
    pub text: String,
}

/// What an [`Index::update`] changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UpdateReport {
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    /// Whether a new segment was written (true iff added+changed > 0).
    pub wrote_segment: bool,
}

impl UpdateReport {
    /// Whether anything at all changed (so the manifest needs saving).
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.changed == 0 && self.removed == 0
    }
}

// ----- Manifest -----------------------------------------------------------------------

/// Per-document metadata, surfaced in search results and used for scoring.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DocMeta {
    key: String,
    title: String,
    type_: String,
    tags: Vec<String>,
    /// Total term count (document length), for length-aware scoring later.
    len: u32,
}

/// Per-file staleness record.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRec {
    doc: u64,
    mtime_ns: u64,
    size: u64,
}

/// The index's global state, persisted as `manifest.json`.
#[derive(Debug, Clone, Default)]
struct Manifest {
    next_doc: u64,
    segments: Vec<u32>,
    files: BTreeMap<String, FileRec>,
    docs: BTreeMap<u64, DocMeta>,
    deleted: BTreeSet<u64>,
}

impl Manifest {
    fn to_json(&self) -> serde_json::Value {
        let files: serde_json::Map<String, serde_json::Value> = self
            .files
            .iter()
            .map(|(k, r)| {
                (
                    k.clone(),
                    serde_json::json!({ "doc": r.doc, "mtime_ns": r.mtime_ns, "size": r.size }),
                )
            })
            .collect();
        let docs: serde_json::Map<String, serde_json::Value> = self
            .docs
            .iter()
            .map(|(id, m)| {
                (
                    id.to_string(),
                    serde_json::json!({
                        "key": m.key,
                        "title": m.title,
                        "type": m.type_,
                        "tags": m.tags,
                        "len": m.len,
                    }),
                )
            })
            .collect();
        serde_json::json!({
            "version": MANIFEST_VERSION,
            "next_doc": self.next_doc,
            "segments": self.segments,
            "files": files,
            "docs": docs,
            "deleted": self.deleted.iter().collect::<Vec<_>>(),
        })
    }

    fn from_json(v: &serde_json::Value) -> Result<Manifest, String> {
        let obj = v.as_object().ok_or("manifest is not an object")?;
        let mut m = Manifest {
            next_doc: obj.get("next_doc").and_then(|x| x.as_u64()).unwrap_or(0),
            ..Manifest::default()
        };
        if let Some(arr) = obj.get("segments").and_then(|x| x.as_array()) {
            m.segments = arr
                .iter()
                .filter_map(|x| x.as_u64().map(|n| n as u32))
                .collect();
        }
        if let Some(files) = obj.get("files").and_then(|x| x.as_object()) {
            for (k, r) in files {
                let doc = r.get("doc").and_then(|x| x.as_u64()).unwrap_or(0);
                let mtime_ns = r.get("mtime_ns").and_then(|x| x.as_u64()).unwrap_or(0);
                let size = r.get("size").and_then(|x| x.as_u64()).unwrap_or(0);
                m.files.insert(
                    k.clone(),
                    FileRec {
                        doc,
                        mtime_ns,
                        size,
                    },
                );
            }
        }
        if let Some(docs) = obj.get("docs").and_then(|x| x.as_object()) {
            for (id, d) in docs {
                let Ok(id) = id.parse::<u64>() else { continue };
                let tags = d
                    .get("tags")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                m.docs.insert(
                    id,
                    DocMeta {
                        key: d
                            .get("key")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        title: d
                            .get("title")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        type_: d
                            .get("type")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        tags,
                        len: d.get("len").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                    },
                );
            }
        }
        if let Some(arr) = obj.get("deleted").and_then(|x| x.as_array()) {
            m.deleted = arr.iter().filter_map(|x| x.as_u64()).collect();
        }
        Ok(m)
    }
}

// ----- Query grammar ------------------------------------------------------------------

/// One parsed query token and how it should match the term dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryTerm {
    /// Exact term match.
    Exact(String),
    /// Prefix match (`term*`).
    Prefix(String),
    /// Levenshtein fuzzy match within `dist` edits (`term~` / `term~N`).
    Fuzzy(String, u32),
    /// Regex over the term dictionary (`/pattern/`) — a slower full-term scan.
    Regex(String),
}

/// Parse a query string into its [`QueryTerm`]s. Whitespace separates tokens;
/// `/.../` is one regex token. The non-regex modes share the index's
/// [`tokenize`] so a query splits exactly the way the stored terms did: a token
/// that tokenizes to several terms contributes leading [`Exact`](QueryTerm::Exact)
/// terms, and any `*`/`~` operator applies to the final term (the typical token
/// is a single term, so this is just `Prefix`/`Fuzzy`). Empty/operator-only
/// tokens are dropped.
pub fn parse_query(query: &str) -> Vec<QueryTerm> {
    let mut out = Vec::new();
    for tok in query.split_whitespace() {
        // /regex/ — the pattern matches whole dictionary terms verbatim.
        if tok.len() >= 2 && tok.starts_with('/') && tok.ends_with('/') {
            let inner = &tok[1..tok.len() - 1];
            if !inner.is_empty() {
                out.push(QueryTerm::Regex(inner.to_string()));
            }
            continue;
        }
        // Split off a trailing operator, then tokenize the core the same way the
        // index did, applying the operator to the final term.
        enum Op {
            Exact,
            Prefix,
            Fuzzy(u32),
        }
        let (core, op) = if let Some(tilde) = tok.rfind('~') {
            let dist = tok[tilde + 1..]
                .parse::<u32>()
                .unwrap_or(1)
                .clamp(1, MAX_FUZZY);
            (&tok[..tilde], Op::Fuzzy(dist))
        } else if let Some(head) = tok.strip_suffix('*') {
            (head, Op::Prefix)
        } else {
            (tok, Op::Exact)
        };
        let mut toks = tokenize(core);
        if let Some(last) = toks.pop() {
            for t in toks {
                out.push(QueryTerm::Exact(t));
            }
            out.push(match op {
                Op::Exact => QueryTerm::Exact(last),
                Op::Prefix => QueryTerm::Prefix(last),
                Op::Fuzzy(d) => QueryTerm::Fuzzy(last, d),
            });
        }
    }
    out
}

// ----- A loaded segment ---------------------------------------------------------------

/// One immutable segment held in memory for querying.
struct Segment {
    map: Map<Vec<u8>>,
    pos: Vec<u8>,
}

/// Accumulate, into `merged`, the `(dictionary term -> live postings)` of every
/// segment key accepted by `aut`, dropping tombstoned documents. Generic over
/// the automaton so the prefix/fuzzy/exact/regex modes share one walk.
fn collect_matches<A: Automaton>(
    segments: &[Segment],
    aut: &A,
    deleted: &BTreeSet<u64>,
    merged: &mut HashMap<String, Vec<(u64, u32)>>,
) {
    for seg in segments {
        let mut stream = seg.map.search(aut).into_stream();
        while let Some((key, off)) = stream.next() {
            let live: Vec<(u64, u32)> = decode_postings(&seg.pos, off)
                .into_iter()
                .filter(|(doc, _)| !deleted.contains(doc))
                .collect();
            if !live.is_empty()
                && let Ok(term) = std::str::from_utf8(key)
            {
                merged.entry(term.to_string()).or_default().extend(live);
            }
        }
    }
}

/// An [`fst::Automaton`] backed by a regex-automata dense DFA, so `/regex/`
/// queries prune the term FST during traversal instead of scanning every term.
/// The DFA is compiled **anchored**, because fst feeds a key's bytes from the
/// start and a match means the whole key matched.
struct DfaAutomaton {
    dfa: dense::DFA<Vec<u32>>,
    start: StateID,
}

impl DfaAutomaton {
    fn new(pattern: &str) -> Result<DfaAutomaton, String> {
        let dfa = dense::Builder::new()
            .configure(dense::Config::new().start_kind(StartKind::Anchored))
            .build(pattern)
            .map_err(|e| format!("invalid regex: {e}"))?;
        let start = dfa
            .universal_start_state(Anchored::Yes)
            .ok_or("regex start depends on look-around, unsupported here")?;
        Ok(DfaAutomaton { dfa, start })
    }
}

impl Automaton for DfaAutomaton {
    type State = StateID;

    fn start(&self) -> StateID {
        self.start
    }

    fn is_match(&self, state: &StateID) -> bool {
        // A DFA reports a whole-input match only after the end-of-input step.
        self.dfa.is_match_state(self.dfa.next_eoi_state(*state))
    }

    fn can_match(&self, state: &StateID) -> bool {
        // A dead state can never reach a match, so fst can prune this branch.
        !self.dfa.is_dead_state(*state)
    }

    fn accept(&self, state: &StateID, byte: u8) -> StateID {
        self.dfa.next_state(*state, byte)
    }
}

// ----- The index ----------------------------------------------------------------------

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// The document's stable key (project-relative path).
    pub key: String,
    pub title: String,
    pub type_: String,
    pub tags: Vec<String>,
    pub score: f32,
}

/// A lazily-maintained fst-segment index rooted at a directory (`.ct/okf/`).
pub struct Index {
    dir: PathBuf,
    manifest: Manifest,
}

impl Index {
    /// Open the index at `dir`, loading `manifest.json` if present (an absent or
    /// unreadable manifest yields an empty index). The directory is created on
    /// the first [`Index::save`].
    pub fn open(dir: &Path) -> Result<Index, String> {
        let manifest_path = dir.join("manifest.json");
        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => {
                let v: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
                Manifest::from_json(&v)?
            }
            Err(_) => Manifest::default(),
        };
        Ok(Index {
            dir: dir.to_path_buf(),
            manifest,
        })
    }

    /// Number of live (non-tombstoned) documents.
    pub fn doc_count(&self) -> usize {
        self.manifest.docs.len()
    }

    /// Number of live segments.
    pub fn segment_count(&self) -> usize {
        self.manifest.segments.len()
    }

    /// Number of tombstoned documents awaiting [`condense`](Index::condense).
    pub fn tombstone_count(&self) -> usize {
        self.manifest.deleted.len()
    }

    /// How many of `current` are new / changed / removed relative to the
    /// manifest — a read-only staleness probe for `index status` that mutates
    /// nothing (the same diff [`update`](Index::update) would act on).
    pub fn pending(&self, current: &[FileStat]) -> (usize, usize, usize) {
        let present: BTreeSet<&str> = current.iter().map(|f| f.key.as_str()).collect();
        let removed = self
            .manifest
            .files
            .keys()
            .filter(|k| !present.contains(k.as_str()))
            .count();
        let (mut added, mut changed) = (0, 0);
        for f in current {
            match self.manifest.files.get(&f.key) {
                None => added += 1,
                Some(r) if r.mtime_ns != f.mtime_ns || r.size != f.size => changed += 1,
                _ => {}
            }
        }
        (added, changed, removed)
    }

    fn seg_paths(&self, num: u32) -> (PathBuf, PathBuf) {
        let base = format!("seg-{num:05}");
        (
            self.dir.join(format!("{base}.fst")),
            self.dir.join(format!("{base}.pos")),
        )
    }

    fn load_segment(&self, num: u32) -> Result<Segment, String> {
        let (fst_path, pos_path) = self.seg_paths(num);
        let fst_bytes =
            std::fs::read(&fst_path).map_err(|e| format!("{}: {e}", fst_path.display()))?;
        let pos = std::fs::read(&pos_path).map_err(|e| format!("{}: {e}", pos_path.display()))?;
        let map = Map::new(fst_bytes).map_err(|e| format!("{}: {e}", fst_path.display()))?;
        Ok(Segment { map, pos })
    }

    /// Reconcile the index against `current` (the live files of the OKF roots),
    /// re-indexing only what changed. `load` is called to read a file's
    /// [`DocSource`] only when it is new or modified. Writes at most one new
    /// segment. Call [`save`](Index::save) afterwards to persist the manifest.
    pub fn update<F>(&mut self, current: &[FileStat], load: F) -> Result<UpdateReport, String>
    where
        F: Fn(&FileStat) -> Result<DocSource, String>,
    {
        let mut report = UpdateReport::default();
        let present: BTreeSet<&str> = current.iter().map(|f| f.key.as_str()).collect();

        // Removals: files in the manifest that are gone from the roots.
        let removed_keys: Vec<String> = self
            .manifest
            .files
            .keys()
            .filter(|k| !present.contains(k.as_str()))
            .cloned()
            .collect();
        for key in removed_keys {
            if let Some(rec) = self.manifest.files.remove(&key) {
                self.manifest.docs.remove(&rec.doc);
                self.manifest.deleted.insert(rec.doc);
                report.removed += 1;
            }
        }

        // Additions / modifications: collect the docs to put in a new segment.
        // term -> (doc -> tf)
        let mut postings: BTreeMap<String, BTreeMap<u64, u32>> = BTreeMap::new();
        for f in current {
            let unchanged = self
                .manifest
                .files
                .get(&f.key)
                .is_some_and(|r| r.mtime_ns == f.mtime_ns && r.size == f.size);
            if unchanged {
                continue;
            }
            let is_change = self.manifest.files.contains_key(&f.key);
            // Supersede any prior doc for this key.
            if let Some(old) = self.manifest.files.get(&f.key) {
                self.manifest.docs.remove(&old.doc);
                self.manifest.deleted.insert(old.doc);
            }
            let src = load(f)?;
            let doc = self.manifest.next_doc;
            self.manifest.next_doc += 1;
            let terms = tokenize(&format!(
                "{} {} {} {}",
                src.title,
                src.type_,
                src.tags.join(" "),
                src.text
            ));
            let len = terms.len() as u32;
            for term in terms {
                *postings.entry(term).or_default().entry(doc).or_insert(0) += 1;
            }
            self.manifest.docs.insert(
                doc,
                DocMeta {
                    key: f.key.clone(),
                    title: src.title,
                    type_: src.type_,
                    tags: src.tags,
                    len,
                },
            );
            self.manifest.files.insert(
                f.key.clone(),
                FileRec {
                    doc,
                    mtime_ns: f.mtime_ns,
                    size: f.size,
                },
            );
            if is_change {
                report.changed += 1;
            } else {
                report.added += 1;
            }
        }

        if !postings.is_empty() {
            let num = self
                .manifest
                .segments
                .iter()
                .copied()
                .max()
                .map(|n| n + 1)
                .unwrap_or(0);
            self.write_segment(num, &postings)?;
            self.manifest.segments.push(num);
            report.wrote_segment = true;
        }
        Ok(report)
    }

    /// Write a segment from a sorted `term -> (doc -> tf)` map.
    fn write_segment(
        &self,
        num: u32,
        postings: &BTreeMap<String, BTreeMap<u64, u32>>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let (fst_path, pos_path) = self.seg_paths(num);
        let mut pos_blob = Vec::new();
        let wtr = std::io::BufWriter::new(
            std::fs::File::create(&fst_path).map_err(|e| format!("{}: {e}", fst_path.display()))?,
        );
        let mut builder = MapBuilder::new(wtr).map_err(|e| e.to_string())?;
        // BTreeMap iterates terms in sorted order, as fst requires.
        for (term, docs) in postings {
            let list: Vec<(u64, u32)> = docs.iter().map(|(&d, &tf)| (d, tf)).collect();
            let off = encode_postings(&mut pos_blob, &list);
            builder
                .insert(term.as_bytes(), off)
                .map_err(|e| e.to_string())?;
        }
        builder.finish().map_err(|e| e.to_string())?;
        std::fs::write(&pos_path, &pos_blob).map_err(|e| format!("{}: {e}", pos_path.display()))?;
        Ok(())
    }

    /// Search the index, returning up to `limit` hits ranked by a tf-idf score.
    /// Reads every live segment; tombstoned documents are filtered out.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
        let terms = parse_query(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let segments: Vec<Segment> = self
            .manifest
            .segments
            .iter()
            .map(|&n| self.load_segment(n))
            .collect::<Result<_, _>>()?;
        let n_docs = self.manifest.docs.len().max(1) as f32;

        let deleted = &self.manifest.deleted;
        let mut scores: HashMap<u64, f32> = HashMap::new();
        for qt in &terms {
            // Build the mode's automaton once, then walk every segment with it,
            // merging each matched dictionary term's postings — so document
            // frequency (and thus idf) is computed over the whole index.
            let mut merged: HashMap<String, Vec<(u64, u32)>> = HashMap::new();
            match qt {
                QueryTerm::Exact(t) => {
                    collect_matches(&segments, &Str::new(t), deleted, &mut merged)
                }
                QueryTerm::Prefix(t) => {
                    collect_matches(&segments, &Str::new(t).starts_with(), deleted, &mut merged)
                }
                QueryTerm::Fuzzy(t, dist) => {
                    if let Ok(lev) = Levenshtein::new(t, *dist) {
                        collect_matches(&segments, &lev, deleted, &mut merged);
                    }
                }
                QueryTerm::Regex(pat) => {
                    if let Ok(dfa) = DfaAutomaton::new(pat) {
                        collect_matches(&segments, &dfa, deleted, &mut merged);
                    }
                }
            }
            for list in merged.values() {
                let df = list.len().max(1) as f32;
                let idf = (1.0 + n_docs / df).ln();
                for &(doc, tf) in list {
                    *scores.entry(doc).or_insert(0.0) += idf * (1.0 + (tf as f32).ln());
                }
            }
        }

        let mut hits: Vec<SearchHit> = scores
            .into_iter()
            .filter_map(|(doc, score)| {
                self.manifest.docs.get(&doc).map(|m| SearchHit {
                    key: m.key.clone(),
                    title: m.title.clone(),
                    type_: m.type_.clone(),
                    tags: m.tags.clone(),
                    score,
                })
            })
            .collect();
        // Highest score first; ties broken by key for deterministic output.
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.key.cmp(&b.key))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// Merge every segment into one, dropping tombstoned documents and their
    /// postings, then delete the old segment files. Reclaims space without
    /// re-reading any source file. A no-op when there is nothing to gain
    /// (≤1 segment and no tombstones).
    pub fn condense(&mut self) -> Result<bool, String> {
        if self.manifest.segments.len() <= 1 && self.manifest.deleted.is_empty() {
            return Ok(false);
        }
        let old_segments = self.manifest.segments.clone();
        let segments: Vec<Segment> = old_segments
            .iter()
            .map(|&n| self.load_segment(n))
            .collect::<Result<_, _>>()?;

        // Rebuild a single term -> (doc -> tf) map from the live postings.
        let mut postings: BTreeMap<String, BTreeMap<u64, u32>> = BTreeMap::new();
        for seg in &segments {
            let mut s = seg.map.stream();
            while let Some((term_bytes, off)) = s.next() {
                let Ok(term) = std::str::from_utf8(term_bytes) else {
                    continue;
                };
                for (doc, tf) in decode_postings(&seg.pos, off) {
                    if self.manifest.deleted.contains(&doc) {
                        continue;
                    }
                    *postings
                        .entry(term.to_string())
                        .or_default()
                        .entry(doc)
                        .or_insert(0) += tf;
                }
            }
        }

        let num = old_segments.iter().copied().max().unwrap_or(0) + 1;
        if !postings.is_empty() {
            self.write_segment(num, &postings)?;
            self.manifest.segments = vec![num];
        } else {
            self.manifest.segments.clear();
        }
        self.manifest.deleted.clear();
        for old in old_segments {
            let (fst_path, pos_path) = self.seg_paths(old);
            let _ = std::fs::remove_file(fst_path);
            let _ = std::fs::remove_file(pos_path);
        }
        Ok(true)
    }

    /// Discard the whole index (segments + manifest state), so the next
    /// [`update`](Index::update) re-indexes every file from scratch.
    pub fn reset(&mut self) {
        for num in std::mem::take(&mut self.manifest.segments) {
            let (fst_path, pos_path) = self.seg_paths(num);
            let _ = std::fs::remove_file(fst_path);
            let _ = std::fs::remove_file(pos_path);
        }
        self.manifest = Manifest::default();
    }

    /// Persist `manifest.json` (creating the index directory if needed).
    pub fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let path = self.dir.join("manifest.json");
        let text =
            serde_json::to_string_pretty(&self.manifest.to_json()).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TAG: AtomicU32 = AtomicU32::new(0);

    /// A fresh, empty scratch dir for one test (cleared if a prior run left it).
    fn scratch() -> PathBuf {
        let n = TAG.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ct-okfindex-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn stat(key: &str, mtime_ns: u64) -> FileStat {
        FileStat {
            key: key.to_string(),
            path: PathBuf::from(key),
            mtime_ns,
            size: mtime_ns,
        }
    }

    fn doc(title: &str, type_: &str, tags: &[&str], text: &str) -> DocSource {
        DocSource {
            title: title.to_string(),
            type_: type_.to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            text: text.to_string(),
        }
    }

    #[test]
    fn tokenize_lowercases_and_splits_on_nonalnum() {
        assert_eq!(
            tokenize("Hello, World! foo_bar"),
            ["hello", "world", "foo", "bar"]
        );
        assert_eq!(tokenize("  "), Vec::<String>::new());
    }

    #[test]
    fn parse_query_recognizes_all_modes() {
        let q = parse_query("plain data* typo~ deep~2 /sch.*ma/");
        assert_eq!(q[0], QueryTerm::Exact("plain".into()));
        assert_eq!(q[1], QueryTerm::Prefix("data".into()));
        assert_eq!(q[2], QueryTerm::Fuzzy("typo".into(), 1));
        assert_eq!(q[3], QueryTerm::Fuzzy("deep".into(), 2));
        assert_eq!(q[4], QueryTerm::Regex("sch.*ma".into()));
    }

    #[test]
    fn varint_roundtrips() {
        for v in [0u64, 1, 127, 128, 300, 16384, u32::MAX as u64, u64::MAX] {
            let mut buf = Vec::new();
            put_uvarint(&mut buf, v);
            let mut pos = 0;
            assert_eq!(get_uvarint(&buf, &mut pos), Some(v));
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn index_search_exact_prefix_and_fuzzy() {
        let dir = scratch();
        let mut idx = Index::open(&dir).unwrap();
        let files = [stat("a.md", 1), stat("b.md", 1)];
        idx.update(&files, |f| {
            Ok(match f.key.as_str() {
                "a.md" => doc(
                    "Customers",
                    "BigQuery Table",
                    &["pii"],
                    "the customers dimension table",
                ),
                _ => doc(
                    "Orders",
                    "BigQuery Table",
                    &["core"],
                    "the orders fact table schema",
                ),
            })
        })
        .unwrap();
        idx.save().unwrap();

        // Exact term hits only the doc that has it.
        let hits = idx.search("customers", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "a.md");

        // Prefix matches both ("table" appears in both via type/text).
        let hits = idx.search("custom*", 10).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.key.as_str()).collect::<Vec<_>>(),
            ["a.md"]
        );

        // Fuzzy tolerates a typo.
        let hits = idx.search("ordrs~", 10).unwrap();
        assert_eq!(hits[0].key, "b.md");

        // Reopen and the persisted index still searches.
        let idx2 = Index::open(&dir).unwrap();
        assert_eq!(idx2.doc_count(), 2);
        assert_eq!(idx2.search("schema", 10).unwrap()[0].key, "b.md");
    }

    #[test]
    fn regex_mode_matches_substrings_via_the_dfa_adapter() {
        let dir = scratch();
        let mut idx = Index::open(&dir).unwrap();
        idx.update(&[stat("a.md", 1), stat("b.md", 1)], |f| {
            Ok(match f.key.as_str() {
                "a.md" => doc("Orders", "Table", &[], "the orders fact table"),
                _ => doc("Customers", "Table", &[], "the customers dimension"),
            })
        })
        .unwrap();

        // /.*omer.*/ matches the term "customers" (substring) but not "orders".
        let hits = idx.search("/.*omer.*/", 10).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.key.as_str()).collect::<Vec<_>>(),
            ["b.md"]
        );

        // An anchored whole-key pattern still works.
        let hits = idx.search("/orders/", 10).unwrap();
        assert_eq!(hits[0].key, "a.md");

        // A pattern matching no term yields nothing (and does not error).
        assert!(idx.search("/zzz.*/", 10).unwrap().is_empty());
    }

    #[test]
    fn incremental_update_supersedes_changed_and_drops_removed() {
        let dir = scratch();
        let mut idx = Index::open(&dir).unwrap();
        idx.update(&[stat("a.md", 1), stat("b.md", 1)], |f| {
            Ok(if f.key == "a.md" {
                doc("Alpha", "Note", &[], "alpha content markalpha")
            } else {
                doc("Beta", "Note", &[], "beta content markbeta")
            })
        })
        .unwrap();

        // Change a.md (new mtime) and remove b.md.
        let report = idx
            .update(&[stat("a.md", 2)], |_| {
                Ok(doc("Alpha", "Note", &[], "rewritten markgamma"))
            })
            .unwrap();
        assert_eq!((report.changed, report.removed, report.added), (1, 1, 0));
        assert_eq!(idx.doc_count(), 1);
        assert_eq!(idx.segment_count(), 2); // a second segment was layered on
        assert_eq!(idx.tombstone_count(), 2); // old a.md doc + removed b.md

        // The old content is gone; the new content is found; b.md is gone.
        assert!(idx.search("markalpha", 10).unwrap().is_empty());
        assert!(idx.search("markbeta", 10).unwrap().is_empty());
        assert_eq!(idx.search("markgamma", 10).unwrap()[0].key, "a.md");

        // No-op update writes no new segment.
        let report = idx
            .update(&[stat("a.md", 2)], |_| {
                panic!("should not load unchanged file")
            })
            .unwrap();
        assert!(report.is_empty());
        assert_eq!(idx.segment_count(), 2);
    }

    #[test]
    fn condense_merges_segments_and_drops_tombstones() {
        let dir = scratch();
        let mut idx = Index::open(&dir).unwrap();
        idx.update(&[stat("a.md", 1)], |_| {
            Ok(doc("A", "Note", &[], "first markone"))
        })
        .unwrap();
        idx.update(&[stat("a.md", 1), stat("b.md", 1)], |f| {
            Ok(if f.key == "b.md" {
                doc("B", "Note", &[], "second marktwo")
            } else {
                doc("A", "Note", &[], "first markone")
            })
        })
        .unwrap();
        // Force a tombstone by rewriting a.md.
        idx.update(&[stat("a.md", 9), stat("b.md", 1)], |f| {
            Ok(if f.key == "a.md" {
                doc("A", "Note", &[], "third markthree")
            } else {
                doc("B", "Note", &[], "second marktwo")
            })
        })
        .unwrap();
        assert!(idx.segment_count() >= 2);
        assert!(idx.tombstone_count() >= 1);

        assert!(idx.condense().unwrap());
        assert_eq!(idx.segment_count(), 1);
        assert_eq!(idx.tombstone_count(), 0);
        assert_eq!(idx.doc_count(), 2);

        // Live content still searchable; superseded content gone.
        assert_eq!(idx.search("markthree", 10).unwrap()[0].key, "a.md");
        assert_eq!(idx.search("marktwo", 10).unwrap()[0].key, "b.md");
        assert!(idx.search("markone", 10).unwrap().is_empty());

        // Old segment files were removed.
        let leftover = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "fst"))
            .count();
        assert_eq!(leftover, 1);
    }

    #[test]
    fn pending_counts_added_changed_removed_without_mutating() {
        let dir = scratch();
        let mut idx = Index::open(&dir).unwrap();
        idx.update(&[stat("a.md", 1), stat("b.md", 1)], |f| {
            Ok(doc(&f.key, "Note", &[], "content"))
        })
        .unwrap();
        // a unchanged, b changed (new mtime), c new -> (1 added, 1 changed, 0 removed).
        assert_eq!(
            idx.pending(&[stat("a.md", 1), stat("b.md", 2), stat("c.md", 1)]),
            (1, 1, 0)
        );
        // Omitting b makes it a removal.
        assert_eq!(idx.pending(&[stat("a.md", 1)]), (0, 0, 1));
        // pending is read-only.
        assert_eq!(idx.doc_count(), 2);
    }

    #[test]
    fn reset_clears_then_reindexes_from_scratch() {
        let dir = scratch();
        let mut idx = Index::open(&dir).unwrap();
        idx.update(&[stat("a.md", 1)], |_| {
            Ok(doc("A", "Note", &[], "unique markx"))
        })
        .unwrap();
        idx.save().unwrap();
        assert_eq!(idx.doc_count(), 1);

        idx.reset();
        assert_eq!((idx.doc_count(), idx.segment_count()), (0, 0));
        assert!(idx.search("markx", 10).unwrap().is_empty());

        // Indexing works again after a reset.
        idx.update(&[stat("a.md", 1)], |_| {
            Ok(doc("A", "Note", &[], "unique marky"))
        })
        .unwrap();
        assert_eq!(idx.search("marky", 10).unwrap()[0].key, "a.md");
    }

    #[test]
    fn search_ranks_more_relevant_documents_higher() {
        let dir = scratch();
        let mut idx = Index::open(&dir).unwrap();
        idx.update(&[stat("hi.md", 1), stat("lo.md", 1)], |f| {
            Ok(if f.key == "hi.md" {
                doc("Hi", "Note", &[], "alpha alpha alpha beta")
            } else {
                doc("Lo", "Note", &[], "alpha gamma delta epsilon")
            })
        })
        .unwrap();
        let hits = idx.search("alpha", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].key, "hi.md",
            "the higher term frequency should rank first"
        );
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn empty_query_returns_no_hits() {
        let dir = scratch();
        let mut idx = Index::open(&dir).unwrap();
        idx.update(&[stat("a.md", 1)], |_| Ok(doc("A", "Note", &[], "content")))
            .unwrap();
        assert!(idx.search("", 10).unwrap().is_empty());
        assert!(idx.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn search_merges_postings_across_segments() {
        let dir = scratch();
        let mut idx = Index::open(&dir).unwrap();
        // Two updates -> two segments, each holding a doc with the term "shared".
        idx.update(&[stat("a.md", 1)], |_| {
            Ok(doc("A", "Note", &[], "shared onlya"))
        })
        .unwrap();
        idx.update(&[stat("a.md", 1), stat("b.md", 1)], |f| {
            Ok(if f.key == "b.md" {
                doc("B", "Note", &[], "shared onlyb")
            } else {
                doc("A", "Note", &[], "shared onlya")
            })
        })
        .unwrap();
        assert!(idx.segment_count() >= 2);
        let hits = idx.search("shared", 10).unwrap();
        let keys: BTreeSet<&str> = hits.iter().map(|h| h.key.as_str()).collect();
        assert!(keys.contains("a.md") && keys.contains("b.md"), "{keys:?}");
    }
}
