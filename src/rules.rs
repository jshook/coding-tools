// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The rule surface shared by `ct-rules` (say what the rules are) and
//! `ct-check` (verify them): the `.ct/rules.jsonc` store model, upward
//! discovery, def expansion, the probe gate, the external-tool **bridge**,
//! and the `expect` outcome adapters.
//!
//! A **rule** is one recorded, framed observation: an `id`, the `question` it
//! answers, the **probe** (an argv vector, never a shell) that answers it by
//! scanning for known violations, and the `why` behind it. **Defs** are the
//! store's named vocabulary, expanded as `{def:NAME}` inside probe argvs.
//! Probes are gated to the suite's fixed read-only set plus the compiled-in
//! [`BRIDGE`] of known read-only invocations of established Rust tools — a
//! store entry *selects from* the gate and can never extend it.
//!
//! The full specification is `docs/specs/rules.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::allowlist;
use crate::pattern;

/// The store's path relative to the `.ct` directory.
pub const STORE_FILE: &str = "rules.jsonc";

/// Walk upward from `start` to the nearest directory containing `.ct`,
/// git-style. Returns that project root, or `None` when no `.ct` exists up
/// to the filesystem root.
///
/// # Examples
///
/// ```
/// use coding_tools::rules::discover_root;
/// // No `.ct` above the filesystem root:
/// assert_eq!(discover_root(std::path::Path::new("/")), None);
/// ```
pub fn discover_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        if d.join(".ct").is_dir() {
            return Some(d);
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    None
}

/// The store path under a project root.
pub fn store_path(root: &Path) -> PathBuf {
    root.join(".ct").join(STORE_FILE)
}

/// The directory probes run from: store paths are **root-relative**, so a
/// probe's working directory is the project root (the parent of the `.ct`
/// directory holding the store), regardless of where the tool was invoked.
/// For a `--file` outside a `.ct` directory, the file's own directory.
pub fn probe_root(store: &Path) -> PathBuf {
    match store.parent() {
        Some(dir) if dir.file_name().is_some_and(|n| n == ".ct") => dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.to_path_buf()),
        Some(dir) => dir.to_path_buf(),
        None => PathBuf::from("."),
    }
}

// ----- Store model ----------------------------------------------------------------

/// A def: the store's named vocabulary. Untyped — a string expands in place;
/// a list expands to multiple argv elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Def {
    /// Expands inside an argv element.
    One(String),
    /// Expands to multiple argv elements (the element must be exactly the
    /// `{def:NAME}` token).
    Many(Vec<String>),
}

/// Rule severity: whether a violation reddens the exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Severity {
    /// A violation fails the run (exit `1`). The default.
    #[default]
    Fail,
    /// A violation is reported (`WARN` lane) but never affects exit status.
    Warn,
}

impl Severity {
    /// Parse the store's `severity` field.
    pub fn parse(s: &str) -> Result<Severity, String> {
        match s {
            "fail" => Ok(Severity::Fail),
            "warn" => Ok(Severity::Warn),
            other => Err(format!("invalid severity '{other}' (use fail or warn)")),
        }
    }
}

/// How a probe's outcome is read as a verdict. Observers speak the suite's
/// exit contract (`Exit`); bridge tools need an adapter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Adapter {
    /// Exit status under the suite contract: `0` holds, `1` violated,
    /// anything else broken. The default.
    #[default]
    Exit,
    /// Holds iff the probe exited `0` and printed nothing to stdout
    /// (whitespace ignored); exited `0` with output = violated; nonzero =
    /// broken. The `cargo tree -d` shape.
    Empty,
    /// `ct-test`-style matchers over the captured streams, with identical
    /// promotion and fail-closed precedence: an `err` hit is decisively a
    /// violation; an `ok` hit decisively holds; a supplied `ok` that did not
    /// appear is a violation; otherwise fall back to `Exit`.
    Match {
        /// Pattern whose presence (stdout or stderr) means the rule holds.
        ok: Option<String>,
        /// Pattern whose presence (stdout or stderr) means a violation.
        err: Option<String>,
    },
}

impl Adapter {
    /// Parse the store's `expect` field: `"exit"`, `"empty"`, or an object
    /// with `ok-match` / `err-match` keys.
    pub fn from_value(v: &serde_json::Value) -> Result<Adapter, String> {
        match v {
            serde_json::Value::String(s) => match s.as_str() {
                "exit" => Ok(Adapter::Exit),
                "empty" => Ok(Adapter::Empty),
                other => Err(format!(
                    "invalid expect '{other}' (use exit, empty, or a matcher object)"
                )),
            },
            serde_json::Value::Object(o) => {
                let get = |k: &str| -> Result<Option<String>, String> {
                    match o.get(k) {
                        None => Ok(None),
                        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
                        Some(_) => Err(format!("expect.{k} must be a string")),
                    }
                };
                let ok = get("ok-match")?;
                let err = get("err-match")?;
                if ok.is_none() && err.is_none() {
                    return Err("expect object needs ok-match and/or err-match".to_string());
                }
                for key in o.keys() {
                    if key != "ok-match" && key != "err-match" {
                        return Err(format!("unknown expect key '{key}'"));
                    }
                }
                Ok(Adapter::Match { ok, err })
            }
            _ => Err("expect must be a string or a matcher object".to_string()),
        }
    }
}

/// One recorded rule.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Unique slug; the rule's name everywhere.
    pub id: String,
    /// What this rule answers — the report line.
    pub question: String,
    /// The probe argv (pre-def-expansion, as stored).
    pub probe: Vec<String>,
    /// Why the invariant exists; printed when it fails.
    pub why: Option<String>,
    /// The verbatim human request that led to this rule, retained so the
    /// intent can be understood or revised later. Provenance only — never
    /// used by verification; stripped wholesale by `ct-rules --flatten`.
    pub prompt: Option<String>,
    /// Labels for `--tag` selection.
    pub tags: Vec<String>,
    /// Provenance date.
    pub added: Option<String>,
    /// Per-rule bound in seconds; overrides the CLI `--timeout`.
    pub timeout: Option<f64>,
    /// An aspiration, not yet held: reported, never enforced.
    pub pending: bool,
    /// Whether a violation reddens the exit status.
    pub severity: Severity,
    /// How the probe's outcome is read.
    pub expect: Adapter,
    /// Permit network access where the bridge entry deems it meaningful.
    pub network: bool,
}

/// The parsed store.
#[derive(Debug, Default)]
pub struct Store {
    /// Named vocabulary, expanded as `{def:NAME}` in probe argvs.
    pub defs: BTreeMap<String, Def>,
    /// The rules, in store (= run) order.
    pub rules: Vec<Rule>,
}

fn as_str(v: &serde_json::Value, what: &str) -> Result<String, String> {
    v.as_str()
        .map(String::from)
        .ok_or_else(|| format!("{what} must be a string"))
}

fn as_str_list(v: &serde_json::Value, what: &str) -> Result<Vec<String>, String> {
    v.as_array()
        .ok_or_else(|| format!("{what} must be an array of strings"))?
        .iter()
        .map(|e| as_str(e, what))
        .collect()
}

/// Parse and validate the JSONC store text.
pub fn parse_store(text: &str) -> Result<Store, String> {
    let value = jsonc_parser::parse_to_serde_value(text, &jsonc_parser::ParseOptions::default())
        .map_err(|e| format!("store parse error: {e}"))?
        .ok_or("store is empty")?;
    let obj = value.as_object().ok_or("store root must be an object")?;
    for key in obj.keys() {
        if key != "defs" && key != "rules" {
            return Err(format!("unknown store key '{key}' (expected defs/rules)"));
        }
    }

    let mut defs = BTreeMap::new();
    if let Some(d) = obj.get("defs") {
        let d = d.as_object().ok_or("defs must be an object")?;
        for (name, val) in d {
            let def = match val {
                serde_json::Value::String(s) => Def::One(s.clone()),
                serde_json::Value::Array(_) => {
                    Def::Many(as_str_list(val, &format!("def '{name}'"))?)
                }
                _ => {
                    return Err(format!(
                        "def '{name}' must be a string or a list of strings"
                    ));
                }
            };
            defs.insert(name.clone(), def);
        }
    }

    let mut rules = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(r) = obj.get("rules") {
        let arr = r.as_array().ok_or("rules must be an array")?;
        for (i, entry) in arr.iter().enumerate() {
            let o = entry
                .as_object()
                .ok_or_else(|| format!("rules[{i}] must be an object"))?;
            let id = as_str(
                o.get("id")
                    .ok_or_else(|| format!("rules[{i}]: missing id"))?,
                "id",
            )?;
            if id.is_empty() || id.contains(char::is_whitespace) {
                return Err(format!("rules[{i}]: invalid id '{id}'"));
            }
            if !seen.insert(id.clone()) {
                return Err(format!("duplicate rule id '{id}'"));
            }
            let where_ = format!("rule '{id}'");
            let question = as_str(
                o.get("question")
                    .ok_or_else(|| format!("{where_}: missing question"))?,
                "question",
            )?;
            let probe = as_str_list(
                o.get("probe")
                    .ok_or_else(|| format!("{where_}: missing probe"))?,
                "probe",
            )?;
            if probe.is_empty() {
                return Err(format!("{where_}: probe must not be empty"));
            }
            let rule = Rule {
                id,
                question,
                probe,
                why: o.get("why").map(|v| as_str(v, "why")).transpose()?,
                prompt: o.get("prompt").map(|v| as_str(v, "prompt")).transpose()?,
                tags: match o.get("tags") {
                    Some(v) => as_str_list(v, "tags")?,
                    None => Vec::new(),
                },
                added: o.get("added").map(|v| as_str(v, "added")).transpose()?,
                timeout: match o.get("timeout") {
                    Some(v) => Some(
                        v.as_f64()
                            .ok_or_else(|| format!("{where_}: timeout must be a number"))?,
                    ),
                    None => None,
                },
                pending: o.get("pending").and_then(|v| v.as_bool()).unwrap_or(false),
                severity: match o.get("severity") {
                    Some(v) => Severity::parse(&as_str(v, "severity")?)
                        .map_err(|e| format!("{where_}: {e}"))?,
                    None => Severity::Fail,
                },
                expect: match o.get("expect") {
                    Some(v) => Adapter::from_value(v).map_err(|e| format!("{where_}: {e}"))?,
                    None => Adapter::Exit,
                },
                network: o.get("network").and_then(|v| v.as_bool()).unwrap_or(false),
            };
            // Built-in checks (`deps`/`mods`) classify their own outcome, so an
            // `expect` adapter is meaningless for them — reject it at load
            // rather than silently ignore it (matches the ct-rules add guard).
            if matches!(
                rule.probe.first().map(String::as_str),
                Some("deps") | Some("mods") | Some("okf")
            ) && rule.expect != Adapter::Exit
            {
                return Err(format!(
                    "{where_}: built-in check '{}' takes no expect adapter (it classifies its own outcome)",
                    rule.probe[0]
                ));
            }
            rules.push(rule);
        }
    }
    Ok(Store { defs, rules })
}

// ----- Def expansion --------------------------------------------------------------

/// Expand `{def:NAME}` tokens in a probe argv. An element that is exactly one
/// `{def:NAME}` token whose def is a list splices to multiple elements; a
/// string def expands inside elements. A list def referenced inside a larger
/// element, or an unknown def, is an error (the rule is broken).
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// use coding_tools::rules::{expand_defs, Def};
///
/// let mut defs = BTreeMap::new();
/// defs.insert("layer".into(), Def::One("src/domain".into()));
/// defs.insert("types".into(), Def::Many(vec!["A".into(), "B".into()]));
///
/// let argv: Vec<String> = ["--base", "{def:layer}", "--items", "{def:types}"]
///     .iter().map(|s| s.to_string()).collect();
/// assert_eq!(
///     expand_defs(&argv, &defs).unwrap(),
///     ["--base", "src/domain", "--items", "A", "B"]
/// );
/// assert!(expand_defs(&["x{def:types}".to_string()], &defs).is_err());
/// assert!(expand_defs(&["{def:nope}".to_string()], &defs).is_err());
/// ```
pub fn expand_defs(argv: &[String], defs: &BTreeMap<String, Def>) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(argv.len());
    for element in argv {
        // Whole-element list splice.
        if let Some(name) = element
            .strip_prefix("{def:")
            .and_then(|r| r.strip_suffix('}'))
            && !name.contains('{')
        {
            match defs.get(name) {
                Some(Def::Many(items)) => {
                    out.extend(items.iter().cloned());
                    continue;
                }
                Some(Def::One(s)) => {
                    out.push(s.clone());
                    continue;
                }
                None => return Err(format!("unknown def '{name}'")),
            }
        }
        // In-place string expansion (possibly several defs per element).
        let mut text = element.clone();
        while let Some(start) = text.find("{def:") {
            let rest = &text[start + 5..];
            let Some(end) = rest.find('}') else {
                break; // unbalanced: leave verbatim
            };
            let name = &rest[..end];
            match defs.get(name) {
                Some(Def::One(s)) => {
                    text = format!("{}{}{}", &text[..start], s, &rest[end + 1..]);
                }
                Some(Def::Many(_)) => {
                    return Err(format!(
                        "def '{name}' is a list and can only stand alone as one argv element"
                    ));
                }
                None => return Err(format!("unknown def '{name}'")),
            }
        }
        out.push(text);
    }
    Ok(out)
}

// ----- The bridge -----------------------------------------------------------------

/// One compiled-in external invocation rules may leverage. The table is
/// immutable: a store entry selects from it and can never extend it.
pub struct BridgeEntry {
    /// The argv prefix that must match (program name gated by basename).
    pub prefix: &'static [&'static str],
    /// Flags appended unconditionally (when not already present).
    pub enforced: &'static [&'static str],
    /// The hermetic flag appended unless the rule's `network` opt-in applies.
    pub offline_flag: Option<&'static str>,
    /// Whether `network: true` is meaningful for this entry.
    pub network_meaningful: bool,
}

/// The compiled-in bridge: known read-only invocations of established Rust
/// tools. See `docs/specs/rules.md` §5.
pub const BRIDGE: &[BridgeEntry] = &[
    BridgeEntry {
        prefix: &["cargo", "metadata"],
        enforced: &["--locked", "--offline", "--format-version", "1"],
        offline_flag: None, // already in enforced
        network_meaningful: false,
    },
    BridgeEntry {
        prefix: &["cargo", "tree"],
        enforced: &["--locked"],
        offline_flag: Some("--offline"),
        network_meaningful: false,
    },
    BridgeEntry {
        prefix: &["cargo", "deny", "check"],
        enforced: &[],
        offline_flag: Some("--offline"),
        network_meaningful: true,
    },
    BridgeEntry {
        prefix: &["rust-analyzer", "search"],
        enforced: &[],
        offline_flag: None,
        network_meaningful: false,
    },
    BridgeEntry {
        prefix: &["rust-analyzer", "symbols"],
        enforced: &[],
        offline_flag: None,
        network_meaningful: false,
    },
];

/// A built-in check type — run in-process (not spawned) by [`run_probe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// The crate-graph check ([`crate::deps::check`]).
    Deps,
    /// The module-graph check ([`crate::modgraph::check`]).
    Mods,
    /// The OKF bundle-conformance check ([`crate::okf::check`]).
    Okf,
}

/// What the gate resolved a probe to.
pub enum Gated<'a> {
    /// A suite observer (read-only tools, `ct-test`, `ct-each`).
    Observer,
    /// A bridge entry; run with [`bridge_argv`]-adjusted arguments.
    Bridge(&'a BridgeEntry),
    /// A built-in check (`deps`/`mods`) run in-process from the rule layer.
    Builtin(Builtin),
}

/// Gate a (def-expanded) probe argv. Returns how it may run, or a refusal
/// naming the reason. The gate is fail-closed and compiled in.
///
/// # Examples
///
/// ```
/// use coding_tools::rules::{gate_probe, Gated};
///
/// let ok = |argv: &[&str]| gate_probe(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>());
/// assert!(matches!(ok(&["ct-search", "--base", "src"]), Ok(Gated::Observer)));
/// assert!(matches!(ok(&["cargo", "tree", "-d"]), Ok(Gated::Bridge(_))));
/// assert!(ok(&["ct-each", "--mutating", "--", "ct-edit"]).is_err()); // mutating never
/// assert!(ok(&["ct-check"]).is_err());                               // no self-recursion
/// assert!(ok(&["cargo", "publish"]).is_err());                       // unlisted prefix
/// assert!(ok(&["rm", "-rf", "x"]).is_err());
/// ```
pub fn gate_probe(argv: &[String]) -> Result<Gated<'static>, String> {
    let name = allowlist::gated_name(&argv[0]);
    if name == "deps" {
        return Ok(Gated::Builtin(Builtin::Deps));
    }
    if name == "mods" {
        return Ok(Gated::Builtin(Builtin::Mods));
    }
    if name == "okf" {
        return Ok(Gated::Builtin(Builtin::Okf));
    }
    if name == "ct-check" {
        return Err(
            "a probe may not run ct-check (no self-recursion through the store)".to_string(),
        );
    }
    if name == "ct-rules" {
        return Err("a probe may not run ct-rules (probes observe; they never write)".to_string());
    }
    if name == "ct-each" {
        if argv.iter().any(|a| a == "--mutating") {
            return Err(
                "a probe may not pass --mutating (rules observe; they never change anything)"
                    .to_string(),
            );
        }
        return Ok(Gated::Observer);
    }
    if allowlist::is_allowed(&name) || name == "ct-test" {
        return Ok(Gated::Observer);
    }
    for entry in BRIDGE {
        if name == entry.prefix[0]
            && argv.len() >= entry.prefix.len()
            && argv[1..entry.prefix.len()]
                .iter()
                .zip(&entry.prefix[1..])
                .all(|(a, p)| a == p)
        {
            return Ok(Gated::Bridge(entry));
        }
    }
    Err(format!(
        "'{}' is not a permitted probe: probes run the suite's read-only tools \
         or a compiled-in bridge invocation ({}); the gate is immutable",
        argv.iter().take(3).cloned().collect::<Vec<_>>().join(" "),
        BRIDGE
            .iter()
            .map(|b| b.prefix.join(" "))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The argv actually launched for a bridge probe: the rule's argv plus the
/// entry's enforced flags and hermetic flag (skipping flags already present).
/// `network` drops the hermetic flag only where the entry deems it meaningful.
///
/// # Examples
///
/// ```
/// use coding_tools::rules::{bridge_argv, BRIDGE};
///
/// let deny = &BRIDGE[2]; // cargo deny check
/// let argv: Vec<String> = ["cargo", "deny", "check", "bans"].iter().map(|s| s.to_string()).collect();
/// assert!(bridge_argv(deny, &argv, false).contains(&"--offline".to_string()));
/// assert!(!bridge_argv(deny, &argv, true).contains(&"--offline".to_string()));
///
/// let tree = &BRIDGE[1]; // cargo tree: network is not meaningful — offline stays
/// let argv: Vec<String> = ["cargo", "tree", "-d"].iter().map(|s| s.to_string()).collect();
/// assert!(bridge_argv(tree, &argv, true).contains(&"--offline".to_string()));
/// assert!(bridge_argv(tree, &argv, true).contains(&"--locked".to_string()));
/// ```
pub fn bridge_argv(entry: &BridgeEntry, argv: &[String], network: bool) -> Vec<String> {
    fn append(out: &mut Vec<String>, flag: &str) {
        if !out.iter().any(|a| a == flag) {
            out.push(flag.to_string());
        }
    }
    let mut out = argv.to_vec();
    let mut i = 0;
    while i < entry.enforced.len() {
        let flag = entry.enforced[i];
        // A flag taking a value (next entry not starting with '-') is
        // appended as a pair when absent.
        if i + 1 < entry.enforced.len() && !entry.enforced[i + 1].starts_with('-') {
            if !argv.iter().any(|a| a == flag) {
                out.push(flag.to_string());
                out.push(entry.enforced[i + 1].to_string());
            }
            i += 2;
        } else {
            append(&mut out, flag);
            i += 1;
        }
    }
    if let Some(offline) = entry.offline_flag
        && !(network && entry.network_meaningful)
    {
        append(&mut out, offline);
    }
    out
}

// ----- Probe execution ---------------------------------------------------------------

/// Run one gated, def-expanded probe to completion and classify it. The
/// probe runs from `root` — store paths are root-relative, so rules behave
/// identically wherever the tool was invoked. Launch failures (e.g. a bridge
/// binary not installed) and timeouts are *broken*, never errors: a
/// defective probe is a maintenance signal the caller reports, not a crash.
pub fn run_probe(
    expanded: &[String],
    gated: &Gated,
    root: &Path,
    network: bool,
    timeout: Option<std::time::Duration>,
    adapter: &Adapter,
) -> (ProbeOutcome, String, crate::supervise::Outcome) {
    if let Gated::Builtin(kind) = gated {
        let (outcome, reason, report) = match kind {
            Builtin::Deps => crate::deps::check(&expanded[1..], root, timeout),
            Builtin::Mods => crate::modgraph::check(&expanded[1..], root, timeout),
            Builtin::Okf => crate::okf::check(&expanded[1..], root, timeout),
        };
        return (
            outcome,
            reason,
            crate::supervise::Outcome {
                stdout: report,
                stderr: String::new(),
                status: None,
                timed_out: false,
            },
        );
    }
    let argv = match gated {
        Gated::Observer => expanded.to_vec(),
        Gated::Bridge(entry) => bridge_argv(entry, expanded, network),
        Gated::Builtin(_) => unreachable!("built-in checks handled above"),
    };
    let name = allowlist::gated_name(&argv[0]);
    let mut command =
        std::process::Command::new(crate::supervise::resolve_program(&argv[0], &name));
    command.args(&argv[1..]).current_dir(root);
    let empty = || crate::supervise::Outcome {
        stdout: String::new(),
        stderr: String::new(),
        status: None,
        timed_out: false,
    };
    match crate::supervise::run_captured(command, None, timeout) {
        Err(e) => (
            ProbeOutcome::Broken,
            format!("could not launch '{}': {e}", argv[0]),
            empty(),
        ),
        Ok(outcome) if outcome.timed_out => {
            let label = timeout.map(crate::pulse::limit_label).unwrap_or_default();
            (
                ProbeOutcome::Broken,
                format!("timed out after {label}; probe killed"),
                outcome,
            )
        }
        Ok(outcome) => {
            let code = outcome.status.and_then(|s| s.code());
            let (result, reason) = classify(adapter, code, &outcome.stdout, &outcome.stderr);
            (result, reason, outcome)
        }
    }
}

// ----- Outcome classification -------------------------------------------------------

/// A probe's classified outcome (before lane mapping).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Zero violations: the rule holds.
    Holds,
    /// Violations found.
    Violated,
    /// The probe itself is defective (could not conclude).
    Broken,
}

/// Classify a finished probe through its adapter. `code` is the exit code
/// (`None` for a signal death — broken). Returns the outcome and a one-line
/// reason. Timeouts are handled by the caller (always broken).
pub fn classify(
    adapter: &Adapter,
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> (ProbeOutcome, String) {
    let by_exit = |code: Option<i32>| -> (ProbeOutcome, String) {
        match code {
            Some(0) => (ProbeOutcome::Holds, "probe exited 0".to_string()),
            Some(1) => (ProbeOutcome::Violated, "probe exited 1".to_string()),
            Some(c) => (ProbeOutcome::Broken, format!("probe exited {c}")),
            None => (ProbeOutcome::Broken, "probe died on a signal".to_string()),
        }
    };
    match adapter {
        Adapter::Exit => by_exit(code),
        Adapter::Empty => match code {
            Some(0) if stdout.trim().is_empty() => {
                (ProbeOutcome::Holds, "probe printed nothing".to_string())
            }
            Some(0) => (
                ProbeOutcome::Violated,
                "expect empty: probe printed output".to_string(),
            ),
            Some(c) => (ProbeOutcome::Broken, format!("probe exited {c}")),
            None => (ProbeOutcome::Broken, "probe died on a signal".to_string()),
        },
        Adapter::Match { ok, err } => {
            let hit = |pat: &str| -> Result<bool, String> {
                Ok(pattern::compile(pat)
                    .map_err(|e| format!("invalid expect pattern '{pat}': {e}"))?
                    .is_match(stdout)
                    || pattern::compile(pat).unwrap().is_match(stderr))
            };
            if let Some(p) = err {
                match hit(p) {
                    Ok(true) => {
                        return (ProbeOutcome::Violated, format!("err-match '{p}' matched"));
                    }
                    Ok(false) => {}
                    Err(e) => return (ProbeOutcome::Broken, e),
                }
            }
            if let Some(p) = ok {
                return match hit(p) {
                    Ok(true) => (ProbeOutcome::Holds, format!("ok-match '{p}' matched")),
                    Ok(false) => (ProbeOutcome::Violated, format!("ok-match '{p}' not found")),
                    Err(e) => (ProbeOutcome::Broken, e),
                };
            }
            by_exit(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
  // a comment, because the store is JSONC
  "defs": {
    "layer": "src/domain",
    "types": ["A", "B"]
  },
  "rules": [
    {
      "id": "one",
      "question": "Q1?",
      "probe": ["ct-search", "--base", "{def:layer}", "--expect", "none"],
      "why": "because",
      "tags": ["t1"]
    },
    {
      "id": "two",
      "question": "Q2?",
      "probe": ["cargo", "tree", "-d"],
      "expect": "empty",
      "severity": "warn",
      "pending": true,
      "timeout": 5
    }
  ]
}"#;

    #[test]
    fn parses_defs_rules_and_optional_fields() {
        let store = parse_store(SAMPLE).unwrap();
        assert_eq!(store.defs.len(), 2);
        assert_eq!(store.rules.len(), 2);
        let two = &store.rules[1];
        assert_eq!(two.severity, Severity::Warn);
        assert!(two.pending);
        assert_eq!(two.expect, Adapter::Empty);
        assert_eq!(two.timeout, Some(5.0));
        assert_eq!(store.rules[0].severity, Severity::Fail);
        assert_eq!(store.rules[0].expect, Adapter::Exit);
    }

    #[test]
    fn rejects_duplicates_and_malformed_entries() {
        let dup = r#"{"rules":[
          {"id":"x","question":"q","probe":["ls"]},
          {"id":"x","question":"q","probe":["ls"]}]}"#;
        assert!(parse_store(dup).unwrap_err().contains("duplicate rule id"));
        let bad = r#"{"rules":[{"id":"x","question":"q","probe":[]}]}"#;
        assert!(parse_store(bad).unwrap_err().contains("must not be empty"));
        let unknown = r#"{"stuff": 1}"#;
        assert!(
            parse_store(unknown)
                .unwrap_err()
                .contains("unknown store key")
        );
        let badsev = r#"{"rules":[{"id":"x","question":"q","probe":["ls"],"severity":"high"}]}"#;
        assert!(
            parse_store(badsev)
                .unwrap_err()
                .contains("invalid severity")
        );
        // A built-in check carries its own outcome; an expect adapter on one is
        // rejected at load, not silently ignored (mirrors the ct-rules guard).
        let builtin_adapter = r#"{"rules":[{"id":"x","question":"q","probe":["deps","--acyclic"],"expect":"empty"}]}"#;
        assert!(
            parse_store(builtin_adapter)
                .unwrap_err()
                .contains("takes no expect adapter")
        );
    }

    #[test]
    fn adapter_parsing_accepts_strings_and_matcher_objects() {
        assert_eq!(
            Adapter::from_value(&serde_json::json!("exit")).unwrap(),
            Adapter::Exit
        );
        assert_eq!(
            Adapter::from_value(&serde_json::json!("empty")).unwrap(),
            Adapter::Empty
        );
        let m = Adapter::from_value(&serde_json::json!({"ok-match": "fine"})).unwrap();
        assert_eq!(
            m,
            Adapter::Match {
                ok: Some("fine".into()),
                err: None
            }
        );
        assert!(Adapter::from_value(&serde_json::json!("sometimes")).is_err());
        assert!(Adapter::from_value(&serde_json::json!({})).is_err());
        assert!(Adapter::from_value(&serde_json::json!({"oops": "x"})).is_err());
    }

    #[test]
    fn gate_admits_observers_and_bridge_only() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(matches!(
            gate_probe(&argv(&["ct-outline", "--base", "."])),
            Ok(Gated::Observer)
        ));
        assert!(matches!(
            gate_probe(&argv(&["ct-test", "--cmd", "cat"])),
            Ok(Gated::Observer)
        ));
        assert!(matches!(
            gate_probe(&argv(&["cargo", "deny", "check", "bans"])),
            Ok(Gated::Bridge(_))
        ));
        assert!(matches!(
            gate_probe(&argv(&["rust-analyzer", "symbols"])),
            Ok(Gated::Bridge(_))
        ));
        // Refusals: mutating tools, self-recursion, unlisted prefixes.
        assert!(gate_probe(&argv(&["ct-edit", "--find", "a", "--replace", "b"])).is_err());
        assert!(gate_probe(&argv(&["cargo", "build"])).is_err());
        assert!(gate_probe(&argv(&["cargo"])).is_err());
        assert!(gate_probe(&argv(&["sh", "-c", "true"])).is_err());
    }

    #[test]
    fn gate_classifies_builtin_checks() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // `deps`/`mods` are reserved heads → built-in checks run in-process.
        assert!(matches!(
            gate_probe(&argv(&["deps", "--acyclic"])),
            Ok(Gated::Builtin(Builtin::Deps))
        ));
        assert!(matches!(
            gate_probe(&argv(&["mods", "--forbid", "a=>b"])),
            Ok(Gated::Builtin(Builtin::Mods))
        ));
        // The retired binary names are not probes (no longer allowlisted).
        assert!(gate_probe(&argv(&["ct-deps", "--acyclic"])).is_err());
        assert!(gate_probe(&argv(&["ct-mods", "--acyclic"])).is_err());
    }

    #[test]
    fn classify_exit_empty_and_matchers() {
        use ProbeOutcome::*;
        assert_eq!(classify(&Adapter::Exit, Some(0), "", "").0, Holds);
        assert_eq!(classify(&Adapter::Exit, Some(1), "", "").0, Violated);
        assert_eq!(classify(&Adapter::Exit, Some(101), "", "").0, Broken);

        assert_eq!(classify(&Adapter::Empty, Some(0), " \n", "").0, Holds);
        assert_eq!(
            classify(&Adapter::Empty, Some(0), "dupe v1\n", "").0,
            Violated
        );
        assert_eq!(classify(&Adapter::Empty, Some(2), "", "").0, Broken);

        let m = Adapter::Match {
            ok: Some("did not match any packages".into()),
            err: None,
        };
        // cargo tree -i on an absent crate: error exit, but the ok proof appears.
        assert_eq!(
            classify(&m, Some(101), "", "error: ... did not match any packages").0,
            Holds
        );
        let m = Adapter::Match {
            ok: None,
            err: Some("^openssl".into()),
        };
        assert_eq!(classify(&m, Some(0), "openssl v1.0\n", "").0, Violated);
        // No hit, only err supplied: fall back to exit.
        assert_eq!(classify(&m, Some(0), "clean", "").0, Holds);
        // Required ok absent: violated even on exit 0 (fail-closed).
        let m = Adapter::Match {
            ok: Some("proof".into()),
            err: None,
        };
        assert_eq!(classify(&m, Some(0), "no luck", "").0, Violated);
    }
}
