// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Conservative index eligibility and observability.
//!
//! A filesystem event only says that a path may have changed. This module owns
//! the separate decision about whether that path may enter an index at all. The
//! first registered provider is the existing OKF Markdown indexer; unknown
//! formats are deliberately unsupported rather than fed to a generic tokenizer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::{Value, json};

use crate::okfindex::FileStat;

pub const CONFIG_FILE: &str = "index.jsonc";
pub const PROVIDER_OKF: &str = "okf-markdown";
pub const PROVIDER_OKF_VERSION: u64 = 1;
pub const DEFAULT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const DEFAULT_DEBOUNCE_MS: u64 = 150;
pub const DEFAULT_AUDIT_SECONDS: u64 = 300;
pub const DEFAULT_IDLE_SECONDS: u64 = 3600;
pub const MAX_AUTOMATIC_DAEMON_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

const DEFAULT_EXCLUDES: &[&str] = &[
    ".git/**",
    ".ct/okf/**",
    "target/**",
    "node_modules/**",
    "**/*.db",
    "**/*.sqlite*",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Derived,
    Project,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Derived => "derived",
            Origin::Project => "project",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub root: PathBuf,
    pub provider: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub project: PathBuf,
    pub scopes: Vec<Scope>,
    pub exclude: Vec<String>,
    pub watch: bool,
    pub debounce_ms: u64,
    pub audit_seconds: u64,
    pub idle_seconds: u64,
    pub max_file_bytes: u64,
    pub system_memory_bytes: u64,
    pub daemon_memory_limit_bytes: u64,
    pub daemon_memory_limit_automatic: bool,
    pub config_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub included: bool,
    pub reason: &'static str,
    pub provider: Option<String>,
    pub scope_root: Option<PathBuf>,
    pub matched: Option<String>,
}

impl Decision {
    fn no(reason: &'static str) -> Decision {
        Decision {
            included: false,
            reason,
            provider: None,
            scope_root: None,
            matched: None,
        }
    }

    pub fn to_json(&self, path: &Path) -> Value {
        json!({
            "path": path,
            "decision": if self.included { "INCLUDED" } else { "EXCLUDED" },
            "reason": self.reason,
            "provider": self.provider,
            "scope_root": self.scope_root,
            "matched": self.matched,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct ScanMetrics {
    pub entries_visited: usize,
    pub files_considered: usize,
    pub files_included: usize,
    pub logical_bytes: u64,
    pub elapsed_ms: u64,
}

impl ScanMetrics {
    pub fn to_json(&self) -> Value {
        json!({
            "entries_visited": self.entries_visited,
            "files_considered": self.files_considered,
            "files_included": self.files_included,
            "logical_bytes": self.logical_bytes,
            "elapsed_ms": self.elapsed_ms,
        })
    }
}

fn string_array(value: Option<&Value>, field: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let arr = value
        .as_array()
        .ok_or_else(|| format!("{field} must be an array of strings"))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{field} must contain only strings"))
        })
        .collect()
}

fn positive_u64(
    obj: &serde_json::Map<String, Value>,
    field: &str,
    default: u64,
) -> Result<u64, String> {
    match obj.get(field) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("{field} must be a positive integer")),
    }
}

/// Default daemon ceiling: five percent of physical RAM, capped at 2 GiB.
/// When the platform cannot report RAM, the fixed cap is the conservative
/// usable fallback.
pub fn automatic_daemon_memory_limit(system_bytes: u64) -> u64 {
    if system_bytes == 0 {
        MAX_AUTOMATIC_DAEMON_MEMORY_BYTES
    } else {
        (system_bytes / 20).clamp(1, MAX_AUTOMATIC_DAEMON_MEMORY_BYTES)
    }
}

fn system_memory_bytes() -> u64 {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    system.total_memory()
}

impl Plan {
    /// Load `.ct/index.jsonc`, deriving conservative OKF scopes when absent.
    pub fn load(project: &Path, detected_roots: &[PathBuf]) -> Result<Plan, String> {
        let project = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
        let config_path = project.join(".ct").join(CONFIG_FILE);
        let parsed = match std::fs::read_to_string(&config_path) {
            Ok(text) => Some(
                jsonc_parser::parse_to_serde_value(&text, &jsonc_parser::ParseOptions::default())
                    .map_err(|e| format!("{}: {e}", config_path.display()))?
                    .ok_or_else(|| format!("{}: empty document", config_path.display()))?,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", config_path.display())),
        };

        let system_memory_bytes = system_memory_bytes();
        let mut plan = Plan {
            project: project.clone(),
            scopes: Vec::new(),
            exclude: DEFAULT_EXCLUDES.iter().map(|s| (*s).to_string()).collect(),
            watch: true,
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            audit_seconds: DEFAULT_AUDIT_SECONDS,
            idle_seconds: DEFAULT_IDLE_SECONDS,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            system_memory_bytes,
            daemon_memory_limit_bytes: automatic_daemon_memory_limit(system_memory_bytes),
            daemon_memory_limit_automatic: true,
            config_path,
        };

        let explicit_scopes = parsed
            .as_ref()
            .and_then(|v| v.as_object())
            .is_some_and(|o| o.contains_key("scopes"));

        if let Some(v) = parsed {
            let obj = v
                .as_object()
                .ok_or_else(|| format!("{}: root must be an object", plan.config_path.display()))?;
            let version = obj.get("version").and_then(Value::as_u64).unwrap_or(1);
            if version != 1 {
                return Err(format!(
                    "{}: unsupported version {version}",
                    plan.config_path.display()
                ));
            }
            plan.watch = obj.get("watch").and_then(Value::as_bool).unwrap_or(true);
            plan.debounce_ms = positive_u64(obj, "debounce_ms", DEFAULT_DEBOUNCE_MS)?;
            plan.audit_seconds = positive_u64(obj, "audit_seconds", DEFAULT_AUDIT_SECONDS)?;
            plan.idle_seconds = positive_u64(obj, "idle_seconds", DEFAULT_IDLE_SECONDS)?;
            plan.max_file_bytes = positive_u64(obj, "max_file_bytes", DEFAULT_MAX_FILE_BYTES)?;
            if obj.contains_key("max_daemon_memory_bytes") {
                plan.daemon_memory_limit_bytes = positive_u64(
                    obj,
                    "max_daemon_memory_bytes",
                    plan.daemon_memory_limit_bytes,
                )?;
                plan.daemon_memory_limit_automatic = false;
            }
            plan.exclude
                .extend(string_array(obj.get("exclude"), "exclude")?);

            if explicit_scopes {
                let scopes = obj
                    .get("scopes")
                    .and_then(Value::as_array)
                    .ok_or("scopes must be an array")?;
                for (i, raw) in scopes.iter().enumerate() {
                    let scope = raw
                        .as_object()
                        .ok_or_else(|| format!("scopes[{i}] must be an object"))?;
                    let root = scope
                        .get("root")
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("scopes[{i}].root must be a string"))?;
                    let provider = scope
                        .get("provider")
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("scopes[{i}].provider must be a string"))?;
                    if provider != PROVIDER_OKF {
                        return Err(format!(
                            "scopes[{i}]: unsupported provider '{provider}' (registered: {PROVIDER_OKF})"
                        ));
                    }
                    let mut include = string_array(scope.get("include"), "scope include")?;
                    if include.is_empty() {
                        include.push("**/*.md".to_string());
                    }
                    let root = PathBuf::from(root);
                    let root = if root.is_absolute() {
                        root
                    } else {
                        project.join(root)
                    };
                    plan.scopes.push(Scope {
                        root: std::fs::canonicalize(&root).unwrap_or(root),
                        provider: provider.to_string(),
                        include,
                        exclude: string_array(scope.get("exclude"), "scope exclude")?,
                        origin: Origin::Project,
                    });
                }
            }
        }

        if !explicit_scopes {
            for root in detected_roots {
                let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
                plan.scopes.push(Scope {
                    root,
                    provider: PROVIDER_OKF.to_string(),
                    include: vec!["**/*.md".to_string()],
                    exclude: Vec::new(),
                    origin: Origin::Derived,
                });
            }
        }
        let mut seen = BTreeSet::new();
        plan.exclude.retain(|pattern| seen.insert(pattern.clone()));
        Ok(plan)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "config": self.config_path,
            "watch": self.watch,
            "debounce_ms": self.debounce_ms,
            "audit_seconds": self.audit_seconds,
            "idle_seconds": self.idle_seconds,
            "max_file_bytes": self.max_file_bytes,
            "system_memory_bytes": self.system_memory_bytes,
            "max_daemon_memory_bytes": self.daemon_memory_limit_bytes,
            "daemon_memory_limit_origin": if self.daemon_memory_limit_automatic { "automatic" } else { "project" },
            "exclude": self.exclude,
            "providers": [{"id": PROVIDER_OKF, "version": PROVIDER_OKF_VERSION}],
            "scopes": self.scopes.iter().map(|s| json!({
                "root": s.root,
                "provider": s.provider,
                "provider_version": PROVIDER_OKF_VERSION,
                "include": s.include,
                "exclude": s.exclude,
                "origin": s.origin.label(),
            })).collect::<Vec<_>>(),
        })
    }

    /// Materializable project configuration for the current effective scopes.
    /// Paths beneath the project are written relatively for portability.
    pub fn config_json(&self) -> Value {
        let mut config = json!({
            "version": 1,
            "watch": self.watch,
            "debounce_ms": self.debounce_ms,
            "audit_seconds": self.audit_seconds,
            "idle_seconds": self.idle_seconds,
            "max_file_bytes": self.max_file_bytes,
            "scopes": self.scopes.iter().map(|s| json!({
                "root": s.root.strip_prefix(&self.project).map(path_key).unwrap_or_else(|_| path_key(&s.root)),
                "provider": s.provider,
                "include": s.include,
                "exclude": s.exclude,
            })).collect::<Vec<_>>(),
            "exclude": self.exclude,
        });
        if !self.daemon_memory_limit_automatic {
            config.as_object_mut().expect("config is an object").insert(
                "max_daemon_memory_bytes".to_string(),
                json!(self.daemon_memory_limit_bytes),
            );
        }
        config
    }

    /// Explain whether `path` is eligible. `metadata` may be omitted for a
    /// deleted path; filename and scope policy can still be explained.
    pub fn decide(&self, path: &Path, metadata: Option<&std::fs::Metadata>) -> Decision {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.project.join(path)
        };
        let index_dir = self.project.join(".ct").join("okf");
        if path.starts_with(&index_dir) {
            return Decision::no("hard-excluded");
        }
        if let Some(meta) = metadata
            && !meta.is_file()
        {
            return Decision::no("not-regular");
        }

        for scope in &self.scopes {
            let Ok(rel) = path.strip_prefix(&scope.root) else {
                continue;
            };
            let rel = path_key(rel);
            let project_rel = path
                .strip_prefix(&self.project)
                .map(path_key)
                .unwrap_or_else(|_| rel.clone());
            if let Some(pat) = self.exclude.iter().find(|p| glob_matches(p, &project_rel)) {
                return Decision {
                    included: false,
                    reason: "excluded",
                    provider: Some(scope.provider.clone()),
                    scope_root: Some(scope.root.clone()),
                    matched: Some(pat.clone()),
                };
            }
            if let Some(pat) = scope.exclude.iter().find(|p| glob_matches(p, &rel)) {
                return Decision {
                    included: false,
                    reason: "excluded",
                    provider: Some(scope.provider.clone()),
                    scope_root: Some(scope.root.clone()),
                    matched: Some(pat.clone()),
                };
            }
            let matched = scope
                .include
                .iter()
                .find(|p| glob_matches(p, &rel))
                .cloned();
            if matched.is_none() {
                continue;
            }
            if scope.provider != PROVIDER_OKF {
                return Decision::no("unsupported-provider");
            }
            if metadata.is_some_and(|meta| meta.len() > self.max_file_bytes) {
                return Decision {
                    included: false,
                    reason: "too-large",
                    provider: Some(scope.provider.clone()),
                    scope_root: Some(scope.root.clone()),
                    matched,
                };
            }
            let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let markdown = path.extension().and_then(|s| s.to_str()) == Some("md");
            if !markdown || crate::okf::is_reserved(filename) {
                return Decision {
                    included: false,
                    reason: "unsupported-type",
                    provider: Some(scope.provider.clone()),
                    scope_root: Some(scope.root.clone()),
                    matched,
                };
            }
            return Decision {
                included: true,
                reason: "included",
                provider: Some(scope.provider.clone()),
                scope_root: Some(scope.root.clone()),
                matched,
            };
        }
        Decision::no("outside-scope")
    }
}

pub fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

fn glob_regex(pattern: &str) -> Result<Regex, regex::Error> {
    let p = pattern.replace('\\', "/");
    let mut out = String::from("^");
    let mut chars = p.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    out.push_str("(?:.*/)?");
                } else {
                    out.push_str(".*");
                }
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '/' => out.push('/'),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    Regex::new(&out)
}

pub fn glob_matches(pattern: &str, path: &str) -> bool {
    glob_regex(pattern).is_ok_and(|r| r.is_match(path))
}

fn mtime_ns(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

pub fn file_stat(plan: &Plan, path: &Path, meta: &std::fs::Metadata) -> FileStat {
    let key = path
        .strip_prefix(&plan.project)
        .map(path_key)
        .unwrap_or_else(|_| path_key(path));
    FileStat {
        key,
        path: path.to_path_buf(),
        mtime_ns: mtime_ns(meta),
        size: meta.len(),
    }
}

/// Reconcile candidates from the effective scopes. The provider whitelist is
/// applied before a path is returned to the indexer.
pub fn scan(plan: &Plan) -> (Vec<FileStat>, ScanMetrics) {
    let started = Instant::now();
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    let mut metrics = ScanMetrics::default();
    for scope in &plan.scopes {
        for item in ignore::WalkBuilder::new(&scope.root).hidden(false).build() {
            metrics.entries_visited += 1;
            let Ok(entry) = item else { continue };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            metrics.files_considered += 1;
            let path = entry.path();
            let Ok(meta) = std::fs::metadata(path) else {
                continue;
            };
            let decision = plan.decide(path, Some(&meta));
            if !decision.included {
                continue;
            }
            let stat = file_stat(plan, path, &meta);
            let key = stat.key.clone();
            if !seen.insert(key.clone()) {
                continue;
            }
            metrics.files_included += 1;
            metrics.logical_bytes += meta.len();
            files.push(stat);
        }
    }
    metrics.elapsed_ms = started.elapsed().as_millis() as u64;
    (files, metrics)
}

pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

pub fn directory_bytes(path: &Path) -> u64 {
    ignore::WalkBuilder::new(path)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .build()
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doublestar_and_star_have_path_semantics() {
        assert!(glob_matches("**/*.md", "a.md"));
        assert!(glob_matches("**/*.md", "deep/a.md"));
        assert!(!glob_matches("*.md", "deep/a.md"));
        assert!(glob_matches("archive/**", "archive/old/a.md"));
    }

    #[test]
    fn provider_boundary_rejects_unknown_and_reserved_files() {
        let root = std::env::temp_dir().join("ct-indexing-policy-test");
        let plan = Plan {
            project: root.clone(),
            scopes: vec![Scope {
                root: root.join("knowledge"),
                provider: PROVIDER_OKF.to_string(),
                include: vec!["**/*".to_string()],
                exclude: vec![],
                origin: Origin::Derived,
            }],
            exclude: vec![],
            watch: true,
            debounce_ms: 1,
            audit_seconds: 1,
            idle_seconds: 1,
            max_file_bytes: 100,
            system_memory_bytes: 8 * 1024 * 1024 * 1024,
            daemon_memory_limit_bytes: MAX_AUTOMATIC_DAEMON_MEMORY_BYTES,
            daemon_memory_limit_automatic: true,
            config_path: root.join(".ct/index.jsonc"),
        };
        assert_eq!(
            plan.decide(&root.join("knowledge/a.db"), None).reason,
            "unsupported-type"
        );
        assert_eq!(
            plan.decide(&root.join("knowledge/index.md"), None).reason,
            "unsupported-type"
        );
        assert!(plan.decide(&root.join("knowledge/a.md"), None).included);
    }

    #[test]
    fn automatic_memory_limit_is_five_percent_capped_at_two_gib() {
        assert_eq!(
            automatic_daemon_memory_limit(8 * 1024 * 1024 * 1024),
            429_496_729
        );
        assert_eq!(
            automatic_daemon_memory_limit(128 * 1024 * 1024 * 1024),
            MAX_AUTOMATIC_DAEMON_MEMORY_BYTES
        );
        assert_eq!(
            automatic_daemon_memory_limit(0),
            MAX_AUTOMATIC_DAEMON_MEMORY_BYTES
        );
    }

    #[test]
    fn idle_and_memory_limits_are_effective_but_only_overrides_persist() {
        let project = std::env::temp_dir().join(format!(
            "ct-indexing-config-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        std::fs::create_dir_all(project.join(".ct")).unwrap();
        let automatic = Plan::load(&project, &[]).unwrap();
        assert_eq!(automatic.idle_seconds, 3600);
        assert!(automatic.daemon_memory_limit_automatic);
        assert!(
            automatic
                .config_json()
                .get("max_daemon_memory_bytes")
                .is_none()
        );

        std::fs::write(
            project.join(".ct/index.jsonc"),
            "{\"idle_seconds\": 42, \"max_daemon_memory_bytes\": 123456}\n",
        )
        .unwrap();
        let configured = Plan::load(&project, &[]).unwrap();
        assert_eq!(configured.idle_seconds, 42);
        assert_eq!(configured.daemon_memory_limit_bytes, 123456);
        assert!(!configured.daemon_memory_limit_automatic);
        assert_eq!(configured.config_json()["max_daemon_memory_bytes"], 123456);
        let _ = std::fs::remove_dir_all(project);
    }
}
