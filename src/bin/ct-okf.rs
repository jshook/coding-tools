// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-okf` — author, query, and index Open Knowledge Format (OKF) bundles.
//!
//! OKF v0.1 bundles are directory trees of Markdown *concepts* whose YAML
//! frontmatter carries a required `type` plus optional metadata. `ct-okf` is
//! **subcommand**-shaped (reachable as `ct okf <verb>`): `search`/`find` query,
//! `roots`/`index`/`init` configure the project's content roots and the lazily
//! maintained full-text index, `validate`/`links` check, and `show`/`add`/`mv`/
//! `set`/`log`/`gen-index`/`script` author. The canonical reference is
//! `docs/explain/ct-okf.md`; the MCP tool-use definition is
//! `docs/explain/ct-okf.json`. Both are embedded below.
//!
//! `ct-okf` writes OKF bundle files (the authoring verbs) and so, like
//! `ct-rules`, is deliberately **not** on the read-only allowlist; read-only
//! OKF composability is provided by the OKF-aware `ct-search`/`ct-tree`/
//! `ct-view`/`ct-outline` and the in-process `okf` built-in check.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use coding_tools::cli::ct_okf::{
    AddArgs, CheckArgs, Cli, Command, FindArgs, Framing, GenIndexArgs, IndexArgs, IndexCmd,
    InitArgs, LogArgs, MvArgs, RootsArgs, RootsCmd, ScriptArgs, SearchArgs, SetArgs, ShowArgs,
    WatchCmd,
};
use coding_tools::explain::Format;
use coding_tools::okf::{self, Frontmatter};
use coding_tools::pulse::{self, PulseState};
use coding_tools::verdict::Expect;
use coding_tools::walk::Selector;
use coding_tools::{
    blockdoc, indexing, indexwatch, jsonout, okfindex, okfroots, okfscript, pattern, template,
};
use serde_json::{Value, json};

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-okf.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-okf.json");

// ----- shared helpers -----------------------------------------------------------------

/// Build the bundle selector (`.md`, honoring `--name`/`--hidden`/`--follow`).
fn selector(cli: &Cli) -> Result<Selector, String> {
    let names = match &cli.name {
        Some(spec) => Some(
            pattern::compile_name_set(spec).map_err(|e| format!("invalid --name pattern: {e}"))?,
        ),
        None => None,
    };
    Ok(okf::md_selector(
        cli.base.clone(),
        names,
        cli.hidden,
        cli.follow,
    ))
}

/// Render a [`Frontmatter`] as a JSON object (only present fields appear).
fn fm_json(fm: &Frontmatter) -> Value {
    okf::fm_to_json(fm)
}

/// The framed expectation for a check verb: default `none`, overridable via `--expect`.
fn check_expect(framing: &Framing) -> Result<Expect, String> {
    match &framing.expect {
        Some(s) => Expect::parse(s).map_err(|e| format!("invalid --expect: {e}")),
        None => Ok(Expect::Eq(0)),
    }
}

/// Print the `== question ==` banner (unless quiet) for a framed check.
fn banner(cli: &Cli, framing: &Framing) {
    if !cli.quiet
        && let Some(q) = &framing.question
    {
        println!("== {q} ==");
    }
}

/// Fire `--emit`/`--emit-stderr` for a framed check.
fn emit(cli: &Cli, framing: &Framing, result: &str, count: usize, total: usize, matches: &str) {
    if framing.emit.is_none() && framing.emit_stderr.is_none() {
        return;
    }
    let count_s = count.to_string();
    let total_s = total.to_string();
    let base_s = cli.base.display().to_string();
    let tokens = [
        ("RESULT", result),
        ("QUESTION", framing.question.as_deref().unwrap_or("")),
        ("COUNT", count_s.as_str()),
        ("TOTAL", total_s.as_str()),
        ("BASE", base_s.as_str()),
        ("MATCHES", matches),
    ];
    if let Some(t) = &framing.emit {
        println!("{}", template::render(t, &tokens));
    }
    if let Some(t) = &framing.emit_stderr {
        eprintln!("{}", template::render(t, &tokens));
    }
}

/// Discover the project root (the nearest `.ct` ancestor of `--base`) and its
/// detected OKF content roots.
fn project_and_roots(cli: &Cli) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let start = std::fs::canonicalize(&cli.base).unwrap_or_else(|_| cli.base.clone());
    let project = okfroots::project_root(&start);
    let roots: Vec<PathBuf> = okfroots::detect(&project)?
        .into_iter()
        .map(|r| r.dir)
        .collect();
    Ok((project, roots))
}

fn index_plan(project: &Path, roots: &[PathBuf]) -> Result<indexing::Plan, String> {
    indexing::Plan::load(project, roots)
}

/// Open the project's index and reconcile it against the content roots,
/// persisting the manifest only when something changed.
fn refresh_index(
    project: &Path,
    roots: &[PathBuf],
) -> Result<(okfindex::Index, okfindex::UpdateReport), String> {
    let plan = index_plan(project, roots)?;
    let (idx, report, _) = indexwatch::reconcile(project, &plan)?;
    Ok((idx, report))
}

/// Prefer a bounded watcher barrier; fall back to the authoritative full
/// reconciliation and then make one guarded attempt to start the optimization.
fn fresh_index(project: &Path, roots: &[PathBuf]) -> Result<okfindex::Index, String> {
    let plan = index_plan(project, roots)?;
    if plan.watch && indexwatch::barrier(project) {
        return okfindex::Index::open(&okfroots::index_dir(project));
    }
    let (idx, _, _) = indexwatch::reconcile(project, &plan)?;
    if let Ok(exe) = std::env::current_exe() {
        // Startup failure cannot make a correct synchronous query fail.
        let _ = indexwatch::ensure_started(&exe, project, &plan);
    }
    Ok(idx)
}

// ----- query verbs --------------------------------------------------------------------

fn cmd_search(cli: &Cli, args: &SearchArgs) -> Result<ExitCode, String> {
    let (project, roots) = project_and_roots(cli)?;
    if index_plan(&project, &roots)?.scopes.is_empty() {
        return Err(
            "no OKF content roots or index scopes configured — run `ct okf init`, `ct okf roots add <dir>`, or configure .ct/index.jsonc"
                .to_string(),
        );
    }
    let idx = fresh_index(&project, &roots)?;
    let query = args.query.join(" ");
    // Over-fetch so type/tag filters still fill the requested limit.
    let raw = idx.search(&query, args.limit.saturating_mul(4).max(args.limit))?;
    let hits: Vec<&okfindex::SearchHit> = raw
        .iter()
        .filter(|h| args.type_.as_deref().is_none_or(|t| h.type_ == t))
        .filter(|h| args.tag.iter().all(|t| h.tags.contains(t)))
        .take(args.limit)
        .collect();

    if cli.json {
        let arr: Vec<Value> = hits
            .iter()
            .map(|h| {
                json!({
                    "path": h.key, "title": h.title, "type": h.type_,
                    "tags": h.tags, "score": h.score,
                })
            })
            .collect();
        let obj = json!({
            "tool": "ct-okf", "verb": "search",
            "query": query, "count": hits.len(), "hits": arr,
        });
        jsonout::print(&obj, cli.json_pretty);
        return Ok(ExitCode::SUCCESS);
    }
    if !cli.quiet {
        for h in &hits {
            let ty = if h.type_.is_empty() {
                String::new()
            } else {
                format!("  [{}]", h.type_)
            };
            println!("{:.3}  {}{ty}  {}", h.score, h.key, h.title);
        }
        println!("{} hit(s)", hits.len());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_find(cli: &Cli, args: &FindArgs) -> Result<ExitCode, String> {
    let sel = selector(cli)?;
    let findings = okf::conformance(&sel)?;
    let mut rows: Vec<(PathBuf, Frontmatter)> = Vec::new();
    for f in findings {
        if f.reserved {
            continue;
        }
        let Some(fm) = f.fm else { continue };
        if let Some(t) = &args.type_
            && fm.type_.as_deref() != Some(t.as_str())
        {
            continue;
        }
        if !args.tag.is_empty() && !args.tag.iter().all(|t| fm.tags.contains(t)) {
            continue;
        }
        rows.push((f.path, fm));
    }

    if cli.json {
        let arr: Vec<Value> = rows
            .iter()
            .map(|(p, fm)| {
                let mut o = fm_json(fm);
                if let Value::Object(m) = &mut o {
                    m.insert("path".into(), json!(p.display().to_string()));
                }
                o
            })
            .collect();
        let obj = json!({
            "tool": "ct-okf", "verb": "find",
            "base": cli.base.display().to_string(), "count": rows.len(), "concepts": arr,
        });
        jsonout::print(&obj, cli.json_pretty);
        return Ok(ExitCode::SUCCESS);
    }
    if !cli.quiet {
        for (p, fm) in &rows {
            let ty = fm.type_.as_deref().unwrap_or("?");
            let title = fm.title.as_deref().unwrap_or("");
            let tags = if fm.tags.is_empty() {
                String::new()
            } else {
                format!("  ({})", fm.tags.join(","))
            };
            println!("{}  [{ty}]  {title}{tags}", p.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ----- roots --------------------------------------------------------------------------

fn cmd_roots(cli: &Cli, args: &RootsArgs) -> Result<ExitCode, String> {
    let (project, _) = project_and_roots(cli)?;
    match &args.action {
        RootsCmd::List => {
            let roots = okfroots::detect(&project)?;
            if cli.json {
                let arr: Vec<Value> = roots
                    .iter()
                    .map(|r| {
                        json!({
                            "key": r.key,
                            "via": r.via.iter().map(|v| v.label()).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                jsonout::print(
                    &json!({"tool":"ct-okf","verb":"roots","roots":arr}),
                    cli.json_pretty,
                );
            } else if !cli.quiet {
                for r in &roots {
                    let via: Vec<&str> = r.via.iter().map(|v| v.label()).collect();
                    println!("{}  ({})", r.key, via.join(","));
                }
                println!("{} root(s)", roots.len());
            }
            Ok(ExitCode::SUCCESS)
        }
        RootsCmd::Add { dir, marker } => {
            let abs = project.join(dir);
            let key = okfroots::rel_key(&project, &abs);
            let mut cfg = okfroots::Config::load(&project)?;
            let added = cfg.add(&key);
            cfg.save(&project)?;
            if *marker {
                okfroots::write_marker(&abs)?;
            }
            if !cli.quiet {
                println!(
                    "{} root '{key}'",
                    if added { "added" } else { "already present:" }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        RootsCmd::Rm { dir } => {
            let abs = project.join(dir);
            let key = okfroots::rel_key(&project, &abs);
            let mut cfg = okfroots::Config::load(&project)?;
            let removed = cfg.remove(&key);
            cfg.save(&project)?;
            if !cli.quiet {
                println!(
                    "{} root '{key}'",
                    if removed {
                        "removed"
                    } else {
                        "not configured:"
                    }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        RootsCmd::Scan { write } => {
            let cands = okfroots::scan_candidates(&project);
            let keys: Vec<String> = cands
                .iter()
                .map(|d| okfroots::rel_key(&project, d))
                .collect();
            if *write {
                let mut cfg = okfroots::Config::load(&project)?;
                for (key, dir) in keys.iter().zip(&cands) {
                    cfg.add(key);
                    okfroots::write_marker(dir)?;
                }
                cfg.save(&project)?;
            }
            if cli.json {
                jsonout::print(
                    &json!({"tool":"ct-okf","verb":"roots","scanned":keys,"written":*write}),
                    cli.json_pretty,
                );
            } else if !cli.quiet {
                for key in &keys {
                    println!("{key}");
                }
                let verb = if *write { "recorded" } else { "found" };
                println!("{verb} {} candidate root(s)", keys.len());
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

// ----- index maintenance --------------------------------------------------------------

fn cmd_index(cli: &Cli, args: &IndexArgs) -> Result<ExitCode, String> {
    let (project, roots) = project_and_roots(cli)?;
    let plan = index_plan(&project, &roots)?;
    match &args.action {
        IndexCmd::Status => {
            let idx = okfindex::Index::open(&okfroots::index_dir(&project))?;
            let watcher = indexwatch::read_status(&project);
            let watcher_start_failure = indexwatch::start_failure(&project);
            let watcher_clean = watcher
                .as_ref()
                .is_some_and(|status| status.healthy() && status.lane == "clean");
            let (added, changed, removed, scan) = if watcher_clean {
                (0, 0, 0, None)
            } else {
                let (files, scan) = indexing::scan(&plan);
                let pending = idx.pending(&files);
                (pending.0, pending.1, pending.2, Some(scan))
            };
            let storage_bytes = indexing::directory_bytes(&okfroots::index_dir(&project));
            let (provider, provider_version) = idx.provider();
            if cli.json {
                let obj = json!({
                    "tool":"ct-okf","verb":"index","action":"status",
                    "generation": idx.generation(),
                    "provider": {"id": provider, "version": provider_version},
                    "roots": roots.len(), "documents": idx.doc_count(),
                    "segments": idx.segment_count(), "tombstones": idx.tombstone_count(),
                    "source_bytes": idx.source_bytes(), "storage_bytes": storage_bytes,
                    "pending": {"added": added, "changed": changed, "removed": removed},
                    "watcher": watcher.as_ref().map(indexwatch::Status::to_json),
                    "daemon_memory": {
                        "rss_bytes": watcher.as_ref().map(|status| status.memory_rss_bytes),
                        "limit_bytes": plan.daemon_memory_limit_bytes,
                        "limit_origin": if plan.daemon_memory_limit_automatic { "automatic" } else { "project" },
                        "system_memory_bytes": plan.system_memory_bytes,
                    },
                    "watcher_start_failure": watcher_start_failure,
                    "scan": scan.as_ref().map(indexing::ScanMetrics::to_json),
                });
                jsonout::print(&obj, cli.json_pretty);
            } else if !cli.quiet {
                println!(
                    "generation {}, {} v{}, {} root(s), {} document(s), {} segment(s), {} tombstone(s)",
                    idx.generation(),
                    provider,
                    provider_version,
                    roots.len(),
                    idx.doc_count(),
                    idx.segment_count(),
                    idx.tombstone_count()
                );
                println!(
                    "source {} bytes, index/runtime {} bytes",
                    idx.source_bytes(),
                    storage_bytes
                );
                println!("pending: +{added} ~{changed} -{removed}");
                if let Some(w) = watcher {
                    println!(
                        "watcher: {} ({}, heartbeat age {} ms, dirty {}, last reconcile {} ms / {} entries)",
                        if w.healthy() {
                            w.lane.as_str()
                        } else {
                            "unavailable"
                        },
                        w.backend,
                        indexing::unix_millis().saturating_sub(w.heartbeat_ms),
                        w.dirty_paths,
                        w.last_reconcile_ms,
                        w.entries_visited
                    );
                    println!(
                        "last event batch: {} ms / {} path(s), event latency {} ms",
                        w.last_batch_ms, w.last_batch_paths, w.last_event_latency_ms
                    );
                    println!(
                        "daemon memory: {} / {} bytes (system {} bytes)",
                        w.memory_rss_bytes, w.memory_limit_bytes, w.system_memory_bytes
                    );
                } else {
                    println!("watcher: unavailable");
                    println!(
                        "daemon memory: unavailable / {} bytes (system {} bytes)",
                        plan.daemon_memory_limit_bytes, plan.system_memory_bytes
                    );
                    if let Some(failure) = watcher_start_failure {
                        println!("watcher start failure: {failure}");
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        IndexCmd::Scopes { effective: _ } => {
            if cli.json {
                jsonout::print(
                    &json!({"tool":"ct-okf","verb":"index","action":"scopes","plan":plan.to_json()}),
                    cli.json_pretty,
                );
            } else if !cli.quiet {
                println!("config: {}", plan.config_path.display());
                println!(
                    "watch={}, debounce={}ms, audit={}s, idle={}s, max-file={} bytes",
                    plan.watch,
                    plan.debounce_ms,
                    plan.audit_seconds,
                    plan.idle_seconds,
                    plan.max_file_bytes
                );
                println!(
                    "daemon memory limit: {} bytes ({}, system {} bytes)",
                    plan.daemon_memory_limit_bytes,
                    if plan.daemon_memory_limit_automatic {
                        "automatic"
                    } else {
                        "project"
                    },
                    plan.system_memory_bytes
                );
                for scope in &plan.scopes {
                    println!(
                        "{} [{} v{}; {}]",
                        scope.root.display(),
                        scope.provider,
                        indexing::PROVIDER_OKF_VERSION,
                        scope.origin.label()
                    );
                    println!("  include: {}", scope.include.join(", "));
                    if !scope.exclude.is_empty() {
                        println!("  exclude: {}", scope.exclude.join(", "));
                    }
                }
                println!("global exclude: {}", plan.exclude.join(", "));
                println!("hard exclude: {}", okfroots::index_dir(&project).display());
            }
            Ok(ExitCode::SUCCESS)
        }
        IndexCmd::Why { path } => {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                project.join(path)
            };
            let metadata = std::fs::metadata(&path).ok();
            let decision = plan.decide(&path, metadata.as_ref());
            if cli.json {
                jsonout::print(
                    &json!({"tool":"ct-okf","verb":"index","action":"why","result":decision.to_json(&path)}),
                    cli.json_pretty,
                );
            } else if !cli.quiet {
                println!("{}", path.display());
                println!(
                    "  decision: {}",
                    if decision.included {
                        "INCLUDED"
                    } else {
                        "EXCLUDED"
                    }
                );
                println!("  reason: {}", decision.reason);
                if let Some(provider) = decision.provider {
                    println!("  provider: {provider}");
                }
                if let Some(root) = decision.scope_root {
                    println!("  scope: {}", root.display());
                }
                if let Some(pattern) = decision.matched {
                    println!("  matched: {pattern}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        IndexCmd::Init { dry_run: _, write } => {
            let config =
                serde_json::to_string_pretty(&plan.config_json()).map_err(|e| e.to_string())?;
            if *write {
                if plan.config_path.exists() {
                    return Err(format!(
                        "{} already exists; edit it explicitly instead of replacing it",
                        plan.config_path.display()
                    ));
                }
                if let Some(parent) = plan.config_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("{}: {e}", parent.display()))?;
                }
                std::fs::write(
                    &plan.config_path,
                    format!("// ct event-assisted index policy\n{config}\n"),
                )
                .map_err(|e| format!("{}: {e}", plan.config_path.display()))?;
                if !cli.quiet {
                    println!("wrote {}", plan.config_path.display());
                }
            } else {
                println!("{config}");
            }
            Ok(ExitCode::SUCCESS)
        }
        IndexCmd::Watch { action } => match action {
            WatchCmd::Status => {
                let status = indexwatch::read_status(&project);
                let failure = indexwatch::start_failure(&project);
                if cli.json {
                    jsonout::print(
                        &json!({"tool":"ct-okf","verb":"index","action":"watch-status","watcher":status.as_ref().map(indexwatch::Status::to_json),"start_failure":failure}),
                        cli.json_pretty,
                    );
                } else if !cli.quiet {
                    match status {
                        Some(s) if s.healthy() => {
                            println!(
                                "{} watcher pid {}: {}, generation {}, dirty {}",
                                s.backend, s.pid, s.lane, s.generation, s.dirty_paths
                            );
                            println!(
                                "memory: {} / {} bytes (system {} bytes)",
                                s.memory_rss_bytes, s.memory_limit_bytes, s.system_memory_bytes
                            );
                        }
                        _ => {
                            println!("watcher unavailable");
                            println!(
                                "memory: unavailable / {} bytes (system {} bytes)",
                                plan.daemon_memory_limit_bytes, plan.system_memory_bytes
                            );
                            if let Some(failure) = failure {
                                println!("last start failure: {failure}");
                            }
                        }
                    }
                }
                Ok(ExitCode::SUCCESS)
            }
            WatchCmd::Start => {
                let exe =
                    std::env::current_exe().map_err(|e| format!("current executable: {e}"))?;
                let started = indexwatch::ensure_started(&exe, &project, &plan)?;
                if !cli.quiet {
                    println!(
                        "{}",
                        if started {
                            "watcher starting"
                        } else {
                            "watcher already running or disabled"
                        }
                    );
                }
                Ok(ExitCode::SUCCESS)
            }
            WatchCmd::Stop => {
                let requested = indexwatch::request_stop(&project)?;
                if !cli.quiet {
                    println!(
                        "{}",
                        if requested {
                            "watcher stop requested"
                        } else {
                            "watcher unavailable"
                        }
                    );
                }
                Ok(ExitCode::SUCCESS)
            }
            WatchCmd::Run => {
                indexwatch::run_daemon(&project, plan)?;
                Ok(ExitCode::SUCCESS)
            }
        },
        IndexCmd::Update => {
            let (_, report, _) = indexwatch::reconcile(&project, &plan)?;
            report_index(cli, "update", &report);
            Ok(ExitCode::SUCCESS)
        }
        IndexCmd::Condense => {
            let (idx, did) = indexwatch::condense(&project)?;
            if !cli.quiet {
                println!(
                    "{}; {} segment(s), {} tombstone(s)",
                    if did {
                        "condensed"
                    } else {
                        "nothing to condense"
                    },
                    idx.segment_count(),
                    idx.tombstone_count()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        IndexCmd::Rebuild => {
            let (_, report, _) = indexwatch::rebuild(&project, &plan)?;
            report_index(cli, "rebuild", &report);
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn report_index(cli: &Cli, action: &str, report: &okfindex::UpdateReport) {
    if cli.json {
        let obj = json!({
            "tool":"ct-okf","verb":"index","action": action,
            "added": report.added, "changed": report.changed, "removed": report.removed,
        });
        jsonout::print(&obj, cli.json_pretty);
    } else if !cli.quiet {
        println!(
            "index {action}: +{} ~{} -{}",
            report.added, report.changed, report.removed
        );
    }
}

// ----- onboarding ---------------------------------------------------------------------

fn cmd_init(cli: &Cli, args: &InitArgs) -> Result<ExitCode, String> {
    let start = std::fs::canonicalize(&cli.base).unwrap_or_else(|_| cli.base.clone());
    let project = okfroots::project_root(&start);
    let cands = okfroots::scan_candidates(&project);
    let mut cfg = okfroots::Config::load(&project)?;
    let mut keys = Vec::new();
    for dir in &cands {
        let key = okfroots::rel_key(&project, dir);
        cfg.add(&key);
        if args.marker {
            okfroots::write_marker(dir)?;
        }
        keys.push(key);
    }
    cfg.save(&project)?;
    // Build the initial index over the now-configured roots.
    let roots: Vec<PathBuf> = okfroots::detect(&project)?
        .into_iter()
        .map(|r| r.dir)
        .collect();
    let (_, report) = refresh_index(&project, &roots)?;

    if cli.json {
        let obj = json!({
            "tool":"ct-okf","verb":"init",
            "project": project.display().to_string(),
            "roots": keys, "indexed": report.added,
        });
        jsonout::print(&obj, cli.json_pretty);
    } else if !cli.quiet {
        println!("project {}", project.display());
        for key in &keys {
            println!("  root {key}");
        }
        println!(
            "{} root(s), {} concept(s) indexed",
            keys.len(),
            report.added
        );
    }
    Ok(ExitCode::SUCCESS)
}

// ----- check verbs --------------------------------------------------------------------

fn cmd_validate(cli: &Cli, args: &CheckArgs) -> Result<ExitCode, String> {
    let sel = selector(cli)?;
    let findings = okf::conformance(&sel)?;
    let total = findings.iter().filter(|f| !f.reserved).count();
    let mut issues: Vec<String> = findings
        .iter()
        .filter(|f| !f.conformant)
        .map(|f| format!("{}: {}", f.path.display(), f.issues.join("; ")))
        .collect();
    if args.strict {
        for (path, link) in okf::broken_links(&sel)? {
            issues.push(format!(
                "{}:{}: broken link {}",
                path.display(),
                link.line,
                link.target
            ));
        }
    }
    let violations = issues.len();
    let verdict = check_expect(&args.framing)?.eval(violations as u64);
    let matches = issues.join("\n");

    if cli.json {
        let obj = json!({
            "tool": "ct-okf", "verb": "validate", "verdict": verdict.label(),
            "base": cli.base.display().to_string(),
            "concepts": total, "violations": violations, "issues": issues,
        });
        jsonout::print(&obj, cli.json_pretty);
        return Ok(verdict.exit_code());
    }
    banner(cli, &args.framing);
    if !cli.quiet {
        for line in &issues {
            println!("{line}");
        }
        println!(
            "{}: {total} concept(s), {violations} violation(s)",
            verdict.label()
        );
    }
    emit(
        cli,
        &args.framing,
        verdict.label(),
        violations,
        total,
        &matches,
    );
    Ok(verdict.exit_code())
}

fn cmd_links(cli: &Cli, args: &CheckArgs) -> Result<ExitCode, String> {
    let sel = selector(cli)?;
    let broken = okf::broken_links(&sel)?;
    let lines: Vec<String> = broken
        .iter()
        .map(|(p, l)| format!("{}:{}: {}", p.display(), l.line, l.target))
        .collect();
    let count = lines.len();
    let verdict = check_expect(&args.framing)?.eval(count as u64);

    if cli.json {
        let arr: Vec<Value> = broken
            .iter()
            .map(|(p, l)| {
                json!({"path": p.display().to_string(), "line": l.line, "target": l.target, "absolute": l.absolute})
            })
            .collect();
        let obj = json!({
            "tool": "ct-okf", "verb": "links", "verdict": verdict.label(),
            "base": cli.base.display().to_string(), "broken": count, "links": arr,
        });
        jsonout::print(&obj, cli.json_pretty);
        return Ok(verdict.exit_code());
    }
    banner(cli, &args.framing);
    if !cli.quiet {
        for line in &lines {
            println!("{line}");
        }
        println!("{}: {count} broken link(s)", verdict.label());
    }
    emit(
        cli,
        &args.framing,
        verdict.label(),
        count,
        count,
        &lines.join("\n"),
    );
    Ok(verdict.exit_code())
}

fn cmd_show(cli: &Cli, args: &ShowArgs) -> Result<ExitCode, String> {
    let path = &args.path;
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let parsed = okf::parse(&text);
    let fm = match &parsed {
        Some(p) => p.fm.clone(),
        None => return Err(format!("{}: no frontmatter", path.display())),
    };
    if cli.json {
        let mut o = fm_json(&fm);
        if let Value::Object(m) = &mut o {
            m.insert("path".into(), json!(path.display().to_string()));
            m.insert(
                "parseable".into(),
                json!(parsed.as_ref().map(|p| p.parseable)),
            );
        }
        jsonout::print(&o, cli.json_pretty);
        return Ok(ExitCode::SUCCESS);
    }
    if !cli.quiet {
        for (k, v) in [
            ("type", fm.type_.as_deref()),
            ("title", fm.title.as_deref()),
            ("description", fm.description.as_deref()),
            ("resource", fm.resource.as_deref()),
            ("timestamp", fm.timestamp.as_deref()),
        ] {
            if let Some(v) = v {
                println!("{k}: {v}");
            }
        }
        if !fm.tags.is_empty() {
            println!("tags: {}", fm.tags.join(", "));
        }
        for (k, v) in &fm.extra {
            println!("{k}: {v}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ----- authoring verbs (these write) --------------------------------------------------

fn cmd_add(cli: &Cli, args: &AddArgs) -> Result<ExitCode, String> {
    let path = &args.path;
    if path.exists() {
        return Err(format!(
            "{} already exists; refusing to overwrite",
            path.display()
        ));
    }
    let title = args.title.clone().unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let content = okf::build_concept(
        &args.type_,
        &title,
        args.description.as_deref(),
        &args.tag,
        &okf::today_utc(),
        None,
    );
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    if !cli.quiet {
        println!("created {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_mv(cli: &Cli, args: &MvArgs) -> Result<ExitCode, String> {
    let (src, dst) = (&args.src, &args.dst);
    if !src.is_file() {
        return Err(format!("{} is not a file", src.display()));
    }
    if dst.exists() {
        return Err(format!(
            "{} already exists; refusing to overwrite",
            dst.display()
        ));
    }
    if let Some(dir) = dst.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    std::fs::rename(src, dst)
        .map_err(|e| format!("move {} -> {}: {e}", src.display(), dst.display()))?;
    let fixed = fix_links_after_move(&cli.base, src, dst)?;
    if !cli.quiet {
        println!(
            "moved {} -> {} ({fixed} link(s) updated)",
            src.display(),
            dst.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// After moving `src` to `dst`, rewrite every bundle cross-link under `base`
/// that resolved to `src` so it points at `dst`. Returns the number of links
/// rewritten. Both bundle-relative (`/…`) and document-relative links are
/// handled by resolving each candidate against its own file's directory. All
/// path math is **lexical** (no `canonicalize`), so a just-moved/missing target
/// still compares correctly.
fn fix_links_after_move(base: &Path, src: &Path, dst: &Path) -> Result<usize, String> {
    let src_key = lex_comps(src).join("/");
    let mut count = 0usize;
    for entry in ignore::WalkBuilder::new(base)
        .hidden(false)
        .build()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let dir = path.parent().unwrap_or(base);
        let mut new_text = text.clone();
        for link in okf::links(&text) {
            let target = link.target.split('#').next().unwrap_or("");
            if target.is_empty() {
                continue;
            }
            let resolved = if link.absolute {
                base.join(target.trim_start_matches('/'))
            } else {
                dir.join(target)
            };
            if lex_comps(&resolved).join("/") != src_key {
                continue;
            }
            // Replacement target, in the same flavor as the original link.
            let new_target = if link.absolute {
                format!("/{}", rel_path(base, dst))
            } else {
                rel_path(dir, dst)
            };
            new_text =
                new_text.replace(&format!("]({})", link.target), &format!("]({new_target})"));
            count += 1;
        }
        if new_text != text {
            std::fs::write(path, new_text).map_err(|e| format!("write {}: {e}", path.display()))?;
        }
    }
    Ok(count)
}

/// Lexically normalize `path` to absolute components (joining the cwd when
/// relative; collapsing `.`/`..`; `/`-flavored and drive-lowercased), without
/// touching the filesystem — so paths to missing files still compare.
fn lex_comps(path: &Path) -> Vec<String> {
    use std::path::Component;
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut parts = Vec::new();
    for c in abs.components() {
        match c {
            Component::Prefix(p) => parts.push(
                p.as_os_str()
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_lowercase(),
            ),
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
        }
    }
    parts
}

/// A `/`-separated relative path from directory `from` to file `to`, computed
/// lexically. Falls back to the target's file name when the two share no root.
fn rel_path(from: &Path, to: &Path) -> String {
    let (f, t) = (lex_comps(from), lex_comps(to));
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let ups = f.len().saturating_sub(common);
    let mut out: Vec<String> = std::iter::repeat_n("..".to_string(), ups).collect();
    out.extend_from_slice(&t[common..]);
    if out.is_empty() {
        t.last().cloned().unwrap_or_default()
    } else {
        out.join("/")
    }
}

fn cmd_set(cli: &Cli, args: &SetArgs) -> Result<ExitCode, String> {
    let path = &args.file;
    let (field, value) = args
        .spec
        .split_once('=')
        .ok_or_else(|| format!("set needs FIELD=VALUE, got '{}'", args.spec))?;
    let field = field.trim();
    if field.is_empty() || field.contains(char::is_whitespace) {
        return Err(format!("invalid field name '{field}'"));
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let (out, replaced) =
        okf::set_field(&text, field, value).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::write(path, out).map_err(|e| format!("write {}: {e}", path.display()))?;
    if !cli.quiet {
        println!(
            "{} {field} in {}",
            if replaced { "updated" } else { "added" },
            path.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_log(cli: &Cli, args: &LogArgs) -> Result<ExitCode, String> {
    let kind = args.kind.as_deref().unwrap_or("Update");
    let path = cli.base.join("log.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let new = okf::log_entry(&existing, &okf::today_utc(), kind, &args.message);
    std::fs::create_dir_all(&cli.base)
        .map_err(|e| format!("create {}: {e}", cli.base.display()))?;
    std::fs::write(&path, new).map_err(|e| format!("write {}: {e}", path.display()))?;
    if !cli.quiet {
        println!("logged to {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_gen_index(cli: &Cli, args: &GenIndexArgs) -> Result<ExitCode, String> {
    let path = cli.base.join("index.md");
    if args.scaffold {
        if path.is_file() {
            if !cli.quiet {
                println!("bundle index present at {}", path.display());
            }
            return Ok(ExitCode::SUCCESS);
        }
        std::fs::create_dir_all(&cli.base)
            .map_err(|e| format!("create {}: {e}", cli.base.display()))?;
        std::fs::write(&path, "---\nokf_version: \"0.1\"\n---\n\n# Index\n")
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        if !cli.quiet {
            println!("created {}", path.display());
        }
        return Ok(ExitCode::SUCCESS);
    }
    // Regenerate from the immediate concepts' frontmatter.
    let read = std::fs::read_dir(&cli.base)
        .map_err(|e| format!("read dir {}: {e}", cli.base.display()))?;
    let mut names: Vec<PathBuf> = read
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();
    names.sort();
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for p in names {
        let file = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if okf::is_reserved(&file) {
            continue;
        }
        let text = std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
        let fm = okf::parse(&text).map(|x| x.fm).unwrap_or_default();
        let title = fm
            .title
            .clone()
            .unwrap_or_else(|| file.trim_end_matches(".md").to_string());
        entries.push((file, title, fm.description.clone().unwrap_or_default()));
    }
    std::fs::write(&path, okf::render_index(&entries))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    if !cli.quiet {
        println!("wrote {} ({} concept(s))", path.display(), entries.len());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_script(cli: &Cli, args: &ScriptArgs) -> Result<ExitCode, String> {
    let src = std::fs::read_to_string(&args.path)
        .map_err(|e| format!("read {}: {e}", args.path.display()))?;
    let fence = args.fence.as_deref().unwrap_or(blockdoc::DEFAULT_FENCE);
    let items = blockdoc::parse(&src, fence, okfscript::ITEM_NAMES)?;
    let specs = okfscript::compile(&items)?;
    let plan = okfscript::simulate(&cli.base, &specs, &okfscript::FsDisk, &okf::today_utc())?;

    if cli.json {
        let actions: Vec<Value> = plan
            .actions
            .iter()
            .map(|a| json!({"ordinal": a.ordinal, "verb": a.verb, "path": a.path, "effect": a.effect}))
            .collect();
        let obj = json!({
            "tool": "ct-okf", "verb": "script", "dry_run": args.dry_run,
            "base": cli.base.display().to_string(),
            "ops": specs.len(), "writes": plan.writes.len(), "actions": actions,
        });
        jsonout::print(&obj, cli.json_pretty);
        if !args.dry_run {
            write_plan(&plan)?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    if !cli.quiet {
        let lead = if args.dry_run { "would " } else { "" };
        for a in &plan.actions {
            println!("{lead}{} {} ({})", a.verb, a.path, a.effect);
        }
    }
    if args.dry_run {
        if !cli.quiet {
            println!(
                "dry run: {} op(s), {} file(s) would be written; nothing written",
                plan.actions.len(),
                plan.writes.len()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    write_plan(&plan)?;
    if !cli.quiet {
        println!(
            "applied {} op(s); {} file(s) written",
            plan.actions.len(),
            plan.writes.len()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Pre-flight every target's parent directory, then write all files — only after
/// the whole batch simulated cleanly, so a failing op means no writes at all.
fn write_plan(plan: &okfscript::Plan) -> Result<(), String> {
    for (path, _) in &plan.writes {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        if path.is_dir() {
            return Err(format!("{} is a directory", path.display()));
        }
    }
    for (path, content) in &plan.writes {
        std::fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

// ----- dispatch -----------------------------------------------------------------------

fn run(mut cli: Cli) -> Result<ExitCode, String> {
    if cli.json_pretty {
        cli.json = true;
    }
    let _watchdog = pulse::watchdog("ct-okf", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-okf", PulseState::new())?;

    let Some(command) = cli.command.take() else {
        return Err("specify a subcommand (see `ct-okf --help`)".to_string());
    };
    match command {
        Command::Search(a) => cmd_search(&cli, &a),
        Command::Find(a) => cmd_find(&cli, &a),
        Command::Roots(a) => cmd_roots(&cli, &a),
        Command::Index(a) => cmd_index(&cli, &a),
        Command::Init(a) => cmd_init(&cli, &a),
        Command::Validate(a) => cmd_validate(&cli, &a),
        Command::Links(a) => cmd_links(&cli, &a),
        Command::Show(a) => cmd_show(&cli, &a),
        Command::Add(a) => cmd_add(&cli, &a),
        Command::Mv(a) => cmd_mv(&cli, &a),
        Command::Set(a) => cmd_set(&cli, &a),
        Command::Log(a) => cmd_log(&cli, &a),
        Command::GenIndex(a) => cmd_gen_index(&cli, &a),
        Command::Script(a) => cmd_script(&cli, &a),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Some(fmt) = cli.explain {
        let body = match fmt {
            Format::Md => EXPLAIN_MD,
            Format::Json => EXPLAIN_JSON,
        };
        print!("{body}");
        return ExitCode::SUCCESS;
    }

    match run(cli) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("ct-okf: {msg}");
            ExitCode::from(2)
        }
    }
}
