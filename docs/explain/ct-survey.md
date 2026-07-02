# ct-survey

Survey a codebase by the units its build system defines, not by raw filesystem
shape. For Rust that is the **workspace → crate → module** hierarchy, each
element carrying file, line, word, character, and test counts. Reachable as
`ct survey ...` or `ct-survey ...`. Read-only.

This is the format-contextualized companion to `ct-tree`. `ct-tree` counts files
generically (by extension, by directory); `ct-survey` attributes those same
exact counts to the crates and modules cargo actually resolves.

## Honesty classes

The output keeps three classes distinct and never conflates them:

- **authoritative** — crate identity, version, workspace membership, and cargo
  target kinds, read from `cargo metadata --format-version 1 --no-deps
  --offline` (the same mechanism the `deps` built-in check uses). This is the
  only source of crate grouping; there is no filesystem-heuristic fallback.
- **exact** — file, line, word, and character counts.
- **heuristic** — the module bucketing (a file is attributed to the nearest
  source root and its path mapped to a module name, the same honesty class as
  `ct-outline` and the module graph) and the `#[test]` tally.

In text output, heuristic values wear a trailing `~`. In `--json`, an `honesty`
block tags each metric.

## Grouping

With no `--group`, the contextual group type is **inferred** from the path's
`Cargo.toml`:

- a `[workspace]` table → `cargo-workspace`: survey every member crate;
- a lone `[package]` → `cargo-crate`: survey just that crate — even when it sits
  inside a larger workspace.

`--group cargo-workspace` / `--group cargo-crate` overrides the inference. Under
`cargo-crate` the surveyed crate is the one whose `Cargo.toml` the path names, so
pointing at a member directory inside a workspace surveys only that member.

## Options

- `--group` — `cargo-workspace` or `cargo-crate`; inferred from the path when
  omitted.
- `--depth` — `crate` (per-crate rows only) or `module` (per-crate then
  per-module). Default: `module`.
- `--sort` — order crates and their modules by `name` (ascending) or by `files`,
  `lines`, or `tests` (largest first). Default: `name`.
- `--json` — emit a structured JSON result instead of text; `--json-pretty` is
  the same, indented.
- `--timeout SECS` — abort with exit 2 if the run (including `cargo metadata`)
  exceeds `SECS` seconds.
- `--heartbeat SECS` — print a liveness pulse while the run is in progress;
  `--heartbeat-emit` sets the line template (tokens `{ELAPSED}` `{TOOL}`) and
  `--heartbeat-to` its stream (`stderr` or `stdout`).

## The path argument

The positional path is a directory (its `Cargo.toml` is used) or a `Cargo.toml`
file directly. Default `.`.

## Test counts

Two test numbers sit side by side, by design:

- `tests` (heuristic, `~`) — a scan for attributes whose final path segment is
  `test`: `#[test]`, `#[tokio::test]`, `#[test_case::test]`. It **excludes**
  `#[cfg(test)]` (a module gate, not a test) and does not discount attributes
  inside strings or comments.
- `test-targets` and `benches` (authoritative) — cargo's own `test` and `bench`
  target counts from metadata.

A crate's total can exceed the sum of its modules: integration tests under
`tests/` and benches under `benches/` count toward the crate but belong to no
source module.

## Example

```
$ ct survey
crate coding_tools   [grouping: authoritative via cargo metadata]
  coding_tools v0.8.4  files 34  lines 9210  tests 112~  test-targets 9  benches 0
    deps      files 1  lines 1112  tests 41~
    modgraph  files 1  lines 380   tests 9~
    ...
totals  files 34  lines 9210  tests 112~  test-targets 9  benches 0
(~ = heuristic; file/line counts exact; grouping and target counts authoritative)
```

## Exit status

`0` on a rendered survey. `2` on a usage error, an unresolvable path, or a
`cargo metadata` failure (with a one-line message on stderr).
