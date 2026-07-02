// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-survey` — format-contextualized codebase survey.
//!
//! Reports a codebase by the units its build system defines — for Rust, the
//! workspace → crate → module hierarchy — each element carrying file, line, and
//! test counts; reachable directly or as `ct survey`. Read-only. Crate identity,
//! workspace membership, and cargo target kinds are authoritative (from `cargo
//! metadata`); file/line counts are exact; the module bucketing and the `#[test]`
//! tally are heuristic (marked `~`). The canonical reference is
//! `docs/explain/ct-survey.md` (emitted for `--explain md`); the MCP tool-use
//! definition is `docs/explain/ct-survey.json` (`--explain json`). Both embedded.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use clap::Parser;
use coding_tools::cli::ct_survey::Cli;
use coding_tools::explain::Format;
use coding_tools::pulse::{self, PulseState};
use coding_tools::survey::{self, CrateNode, FileStat, GroupKind, Metadata, PkgMeta, Survey};
use coding_tools::walk::{self, EntryType};
use coding_tools::{pattern, tree};

const EXPLAIN_MD: &str = include_str!("../../docs/explain/ct-survey.md");
const EXPLAIN_JSON: &str = include_str!("../../docs/explain/ct-survey.json");

/// Resolve the survey path to `(manifest_path, run_dir)`: the `Cargo.toml` to
/// probe and the directory to run `cargo metadata` in.
fn resolve_manifest(path: &Path) -> Result<(PathBuf, PathBuf), String> {
    if path.is_file() {
        if path.file_name().and_then(|n| n.to_str()) != Some("Cargo.toml") {
            return Err(format!(
                "{} is not a Cargo.toml (pass a crate/workspace directory or its Cargo.toml)",
                path.display()
            ));
        }
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        Ok((path.to_path_buf(), dir))
    } else if path.is_dir() {
        let manifest = path.join("Cargo.toml");
        if !manifest.is_file() {
            return Err(format!(
                "no Cargo.toml in {} (ct-survey surveys cargo workspaces and crates)",
                path.display()
            ));
        }
        Ok((manifest, path.to_path_buf()))
    } else {
        Err(format!("no such file or directory: {}", path.display()))
    }
}

/// One `cargo metadata --format-version 1 --no-deps --offline` run.
fn cargo_metadata(dir: &Path, timeout: Option<Duration>) -> Result<Metadata, String> {
    let mut command = Command::new("cargo");
    command
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(dir);
    let outcome = coding_tools::supervise::run_captured(command, None, timeout)
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if outcome.timed_out {
        return Err("cargo metadata timed out".to_string());
    }
    if !outcome.status.is_some_and(|s| s.success()) {
        return Err(format!(
            "cargo metadata failed: {}",
            outcome.stderr.lines().last().unwrap_or("(no output)")
        ));
    }
    survey::parse_metadata(&outcome.stdout)
}

/// The member ids to survey under `group`: every member for a workspace, or just
/// the package whose manifest is `manifest` for a lone crate.
fn select_members(
    meta: &Metadata,
    group: GroupKind,
    manifest: &Path,
) -> Result<Vec<String>, String> {
    match group {
        GroupKind::CargoWorkspace => Ok(meta.members.clone()),
        GroupKind::CargoCrate => {
            let want = std::fs::canonicalize(manifest).unwrap_or_else(|_| manifest.to_path_buf());
            let hit = meta.members.iter().find(|id| {
                meta.packages.get(*id).is_some_and(|p| {
                    let have = Path::new(&p.manifest_path);
                    std::fs::canonicalize(have).unwrap_or_else(|_| have.to_path_buf()) == want
                })
            });
            match hit {
                Some(id) => Ok(vec![id.clone()]),
                // The probed manifest is a virtual workspace root (no [package]),
                // yet --group cargo-crate was asked for: an impossible framing.
                None => Err(format!(
                    "no crate at {} (it looks like a workspace root; drop --group or use --group cargo-workspace)",
                    manifest.display()
                )),
            }
        }
    }
}

/// Walk a crate directory for `.rs` files and build its [`FileStat`]s. Files
/// under a `target/` directory are skipped (belt-and-suspenders over the walker's
/// gitignore respect). `src_root` (when known) makes each in-source file's path
/// module-relative; files outside it get `rel_to_src: None`.
fn crate_files(
    crate_dir: &Path,
    src_root: Option<&Path>,
    names: &[regex::Regex],
) -> Result<Vec<FileStat>, String> {
    let selector = walk::Selector {
        base: crate_dir.to_path_buf(),
        names: Some(names.to_vec()),
        types: vec![EntryType::F],
        size: None,
        hidden: false,
        follow: false,
        no_ignore: false,
    };
    let mut files = Vec::new();
    for entry in selector.walk() {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if path
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case("target"))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue; // unreadable / non-UTF-8: skipped, like the other walkers
        };
        let (lines, words, chars) = tree::metrics(&text);
        let rel_to_src = src_root.and_then(|root| {
            path.strip_prefix(root)
                .ok()
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        });
        files.push(FileStat {
            rel_to_src,
            lines,
            words,
            chars,
            tests: survey::count_tests(&text),
        });
    }
    Ok(files)
}

/// Build a [`CrateNode`] for one package.
fn crate_node(pkg: &PkgMeta, names: &[regex::Regex]) -> Result<CrateNode, String> {
    let src_root = pkg.src_root();
    let files = crate_files(&pkg.dir(), src_root.as_deref(), names)?;
    let (counts, modules) = survey::roll_up(&files);
    Ok(CrateNode {
        name: pkg.name.clone(),
        version: pkg.version.clone(),
        counts,
        test_targets: pkg.test_targets(),
        bench_targets: pkg.bench_targets(),
        modules,
    })
}

fn run(mut cli: Cli) -> Result<ExitCode, String> {
    if cli.json_pretty {
        cli.json = true;
    }
    let _watchdog = pulse::watchdog("ct-survey", cli.timeout)?;
    let _pulse = cli.heartbeat.start("ct-survey", PulseState::new())?;
    let timeout = cli.timeout.map(Duration::from_secs_f64);

    let (manifest, run_dir) = resolve_manifest(&cli.path)?;
    let manifest_text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("read {}: {e}", manifest.display()))?;
    let group = cli
        .group
        .unwrap_or_else(|| survey::infer_group(&manifest_text));

    let meta = cargo_metadata(&run_dir, timeout)?;
    let member_ids = select_members(&meta, group, &manifest)?;

    let names = pattern::compile_name_set_with("*.rs", None)
        .map_err(|e| format!("internal: *.rs pattern: {e}"))?;

    let mut crates = Vec::new();
    for id in &member_ids {
        let pkg = meta
            .packages
            .get(id)
            .ok_or_else(|| format!("metadata is missing package {id}"))?;
        crates.push(crate_node(pkg, &names)?);
    }

    // Display name and root depend on the framing: a workspace labels by its root
    // directory, a lone crate by the crate itself.
    let (name, root) = match group {
        GroupKind::CargoWorkspace => {
            let root = &meta.workspace_root;
            let name = Path::new(root)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.clone());
            (name, root.clone())
        }
        GroupKind::CargoCrate => {
            let pkg = meta
                .packages
                .get(&member_ids[0])
                .expect("selected member exists");
            (pkg.name.clone(), pkg.dir().display().to_string())
        }
    };

    let mut result = Survey {
        group,
        name,
        root,
        crates,
    };
    result.sort(cli.sort);

    if cli.json {
        coding_tools::jsonout::print(&survey::to_json(&result), cli.json_pretty);
    } else {
        print!("{}", survey::render_text(&result, cli.depth));
    }
    Ok(ExitCode::SUCCESS)
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
            eprintln!("ct-survey: {msg}");
            ExitCode::from(2)
        }
    }
}
