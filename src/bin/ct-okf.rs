// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-okf` — author and query Open Knowledge Format (OKF) bundles.
//!
//! OKF v0.1 bundles are directory trees of Markdown *concepts* whose YAML
//! frontmatter carries a required `type` plus optional metadata. `ct-okf`
//! reads them (`--validate` a conformance verdict, `--list`/`--show` to query
//! metadata, `--links` to audit cross-links) and authors them (`--new`,
//! `--init`, `--index`, `--log`, `--set`). Reachable directly or as `ct okf`.
//! The canonical reference is `docs/explain/ct-okf.md`; the MCP tool-use
//! definition is `docs/explain/ct-okf.json`. Both are embedded below.
//!
//! `ct-okf` writes OKF bundle files (the authoring verbs) and so, like
//! `ct-rules`, is deliberately **not** on the read-only allowlist; read-only
//! OKF composability is provided by the OKF-aware `ct-search`/`ct-tree`/
//! `ct-view`/`ct-outline` and the in-process `okf` built-in check.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use coding_tools::cli::ct_okf::Cli;
use coding_tools::explain::Format;
use coding_tools::okf::{self, Frontmatter};
use coding_tools::pulse::{self, PulseState};
use coding_tools::verdict::Expect;
use coding_tools::walk::Selector;
use coding_tools::{blockdoc, jsonout, okfscript, pattern, template};
use serde_json::{Value, json};

/// Agent documentation, embedded from the canonical `docs/explain` payloads.
const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-okf.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-okf.json");

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

/// Resolve the framed expectation for a check verb: default `none` (no
/// violations), overridable with `--expect`.
fn check_expect(cli: &Cli) -> Result<Expect, String> {
    match &cli.expect {
        Some(s) => Expect::parse(s).map_err(|e| format!("invalid --expect: {e}")),
        None => Ok(Expect::Eq(0)),
    }
}

/// Print the `== question ==` banner (unless quiet) for a framed check.
fn banner(cli: &Cli) {
    if !cli.quiet
        && let Some(q) = &cli.question
    {
        println!("== {q} ==");
    }
}

/// Fire `--emit`/`--emit-stderr` for a framed check.
fn emit(cli: &Cli, result: &str, count: usize, total: usize, matches: &str) {
    if cli.emit.is_none() && cli.emit_stderr.is_none() {
        return;
    }
    let count_s = count.to_string();
    let total_s = total.to_string();
    let base_s = cli.base.display().to_string();
    let tokens = [
        ("RESULT", result),
        ("QUESTION", cli.question.as_deref().unwrap_or("")),
        ("COUNT", count_s.as_str()),
        ("TOTAL", total_s.as_str()),
        ("BASE", base_s.as_str()),
        ("MATCHES", matches),
    ];
    if let Some(t) = &cli.emit {
        println!("{}", template::render(t, &tokens));
    }
    if let Some(t) = &cli.emit_stderr {
        eprintln!("{}", template::render(t, &tokens));
    }
}

// ----- read-only verbs ----------------------------------------------------------------

fn cmd_validate(cli: &Cli) -> Result<ExitCode, String> {
    let sel = selector(cli)?;
    let findings = okf::conformance(&sel)?;
    let concepts: Vec<_> = findings.iter().filter(|f| !f.reserved).collect();
    let total = concepts.len();
    let mut issues: Vec<String> = findings
        .iter()
        .filter(|f| !f.conformant)
        .map(|f| format!("{}: {}", f.path.display(), f.issues.join("; ")))
        .collect();
    if cli.strict {
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
    let expect = check_expect(cli)?;
    let verdict = expect.eval(violations as u64);
    let matches = issues.join("\n");

    if cli.json {
        let obj = json!({
            "tool": "ct-okf",
            "verb": "validate",
            "verdict": verdict.label(),
            "base": cli.base.display().to_string(),
            "concepts": total,
            "violations": violations,
            "issues": issues,
        });
        jsonout::print(&obj, cli.json_pretty);
        return Ok(verdict.exit_code());
    }
    banner(cli);
    if !cli.quiet {
        for line in &issues {
            println!("{line}");
        }
        println!(
            "{}: {total} concept(s), {violations} violation(s)",
            verdict.label()
        );
    }
    emit(cli, verdict.label(), violations, total, &matches);
    Ok(verdict.exit_code())
}

fn cmd_list(cli: &Cli) -> Result<ExitCode, String> {
    let sel = selector(cli)?;
    let findings = okf::conformance(&sel)?;
    let want_tags = &cli.tag;
    let mut rows: Vec<(PathBuf, Frontmatter)> = Vec::new();
    for f in findings {
        if f.reserved {
            continue;
        }
        let Some(fm) = f.fm else { continue };
        if let Some(t) = &cli.type_
            && fm.type_.as_deref() != Some(t.as_str())
        {
            continue;
        }
        if !want_tags.is_empty() && !want_tags.iter().all(|t| fm.tags.contains(t)) {
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
            "tool": "ct-okf",
            "verb": "list",
            "base": cli.base.display().to_string(),
            "count": rows.len(),
            "concepts": arr,
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

fn cmd_show(cli: &Cli, path: &Path) -> Result<ExitCode, String> {
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
        if let Some(v) = &fm.type_ {
            println!("type: {v}");
        }
        if let Some(v) = &fm.title {
            println!("title: {v}");
        }
        if let Some(v) = &fm.description {
            println!("description: {v}");
        }
        if let Some(v) = &fm.resource {
            println!("resource: {v}");
        }
        if let Some(v) = &fm.timestamp {
            println!("timestamp: {v}");
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

fn cmd_links(cli: &Cli) -> Result<ExitCode, String> {
    let sel = selector(cli)?;
    let broken = okf::broken_links(&sel)?;
    let lines: Vec<String> = broken
        .iter()
        .map(|(p, l)| format!("{}:{}: {}", p.display(), l.line, l.target))
        .collect();
    let count = lines.len();
    let expect = check_expect(cli)?;
    let verdict = expect.eval(count as u64);

    if cli.json {
        let arr: Vec<Value> = broken
            .iter()
            .map(|(p, l)| {
                json!({
                    "path": p.display().to_string(),
                    "line": l.line,
                    "target": l.target,
                    "absolute": l.absolute,
                })
            })
            .collect();
        let obj = json!({
            "tool": "ct-okf",
            "verb": "links",
            "verdict": verdict.label(),
            "base": cli.base.display().to_string(),
            "broken": count,
            "links": arr,
        });
        jsonout::print(&obj, cli.json_pretty);
        return Ok(verdict.exit_code());
    }
    banner(cli);
    if !cli.quiet {
        for line in &lines {
            println!("{line}");
        }
        println!("{}: {count} broken link(s)", verdict.label());
    }
    emit(cli, verdict.label(), count, count, &lines.join("\n"));
    Ok(verdict.exit_code())
}

// ----- authoring verbs (these write) --------------------------------------------------

fn cmd_new(cli: &Cli, path: &Path) -> Result<ExitCode, String> {
    let type_ = cli
        .type_
        .as_deref()
        .ok_or("--new requires --type (the concept kind)")?;
    if path.exists() {
        return Err(format!(
            "{} already exists; refusing to overwrite",
            path.display()
        ));
    }
    let title = cli.title.clone().unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let content = okf::build_concept(
        type_,
        &title,
        cli.description.as_deref(),
        &cli.tag,
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

fn cmd_init(cli: &Cli) -> Result<ExitCode, String> {
    let path = cli.base.join("index.md");
    if path.is_file() {
        if !cli.quiet {
            println!("bundle index present at {}", path.display());
        }
        return Ok(ExitCode::SUCCESS);
    }
    std::fs::create_dir_all(&cli.base)
        .map_err(|e| format!("create {}: {e}", cli.base.display()))?;
    let body = "---\nokf_version: \"0.1\"\n---\n\n# Index\n";
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
    if !cli.quiet {
        println!("created {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_index(cli: &Cli) -> Result<ExitCode, String> {
    // List the immediate concept files in --base (non-reserved .md).
    let mut entries: Vec<(String, String, String)> = Vec::new(); // (file, title, description)
    let read = std::fs::read_dir(&cli.base)
        .map_err(|e| format!("read dir {}: {e}", cli.base.display()))?;
    let mut names: Vec<PathBuf> = read
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();
    names.sort();
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
        let desc = fm.description.clone().unwrap_or_default();
        entries.push((file, title, desc));
    }
    let path = cli.base.join("index.md");
    std::fs::write(&path, okf::render_index(&entries))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    if !cli.quiet {
        println!("wrote {} ({} concept(s))", path.display(), entries.len());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_log(cli: &Cli, message: &str) -> Result<ExitCode, String> {
    let kind = cli.log_kind.as_deref().unwrap_or("Update");
    let path = cli.base.join("log.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let new = okf::log_entry(&existing, &okf::today_utc(), kind, message);
    std::fs::create_dir_all(&cli.base)
        .map_err(|e| format!("create {}: {e}", cli.base.display()))?;
    std::fs::write(&path, new).map_err(|e| format!("write {}: {e}", path.display()))?;
    if !cli.quiet {
        println!("logged to {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_set(cli: &Cli, spec: &str) -> Result<ExitCode, String> {
    let path = cli
        .file
        .as_ref()
        .ok_or("--set requires --file (the concept to edit)")?;
    let (field, value) = spec
        .split_once('=')
        .ok_or_else(|| format!("--set needs FIELD=VALUE, got '{spec}'"))?;
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
        let how = if replaced { "updated" } else { "added" };
        println!("{how} {field} in {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// `--script`: run a `.ctb` batch of OKF mutations atomically. The whole batch
/// is simulated in memory (cascading, so a later op sees earlier ops' writes);
/// nothing is written unless every op succeeds — and `--dry-run` writes nothing
/// either way, printing the plan instead.
fn cmd_script(cli: &Cli, path: &Path) -> Result<ExitCode, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let fence = cli.fence.as_deref().unwrap_or(blockdoc::DEFAULT_FENCE);
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
            "tool": "ct-okf",
            "verb": "script",
            "dry_run": cli.dry_run,
            "base": cli.base.display().to_string(),
            "ops": specs.len(),
            "writes": plan.writes.len(),
            "actions": actions,
        });
        jsonout::print(&obj, cli.json_pretty);
        if !cli.dry_run {
            write_plan(&plan)?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !cli.quiet {
        let lead = if cli.dry_run { "would " } else { "" };
        for a in &plan.actions {
            println!("{lead}{} {} ({})", a.verb, a.path, a.effect);
        }
    }
    if cli.dry_run {
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

/// Pre-flight every target's parent directory, then write all files. Run only
/// after the whole batch simulated cleanly, so the atomic guarantee holds:
/// a failing op means no writes at all.
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

fn run(mut cli: Cli) -> Result<ExitCode, String> {
    if cli.json_pretty {
        cli.json = true;
    }
    let _watchdog = pulse::watchdog("ct-okf", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-okf", PulseState::new())?;

    let verbs = [
        cli.validate,
        cli.list,
        cli.show.is_some(),
        cli.links,
        cli.new.is_some(),
        cli.init,
        cli.index,
        cli.log.is_some(),
        cli.set.is_some(),
        cli.script.is_some(),
    ];
    match verbs.iter().filter(|v| **v).count() {
        0 => cmd_validate(&cli), // default verb
        1 => {
            if cli.validate {
                cmd_validate(&cli)
            } else if cli.list {
                cmd_list(&cli)
            } else if let Some(p) = cli.show.clone() {
                cmd_show(&cli, &p)
            } else if cli.links {
                cmd_links(&cli)
            } else if let Some(p) = cli.new.clone() {
                cmd_new(&cli, &p)
            } else if cli.init {
                cmd_init(&cli)
            } else if cli.index {
                cmd_index(&cli)
            } else if let Some(m) = cli.log.clone() {
                cmd_log(&cli, &m)
            } else if let Some(s) = cli.set.clone() {
                cmd_set(&cli, &s)
            } else if let Some(p) = cli.script.clone() {
                cmd_script(&cli, &p)
            } else {
                unreachable!("verb dispatch covered above")
            }
        }
        _ => Err(
            "choose exactly one verb: --validate/--list/--show/--links/--new/--init/--index/--log/--set/--script"
                .to_string(),
        ),
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
