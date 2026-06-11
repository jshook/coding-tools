# ct-edit — Verifiable Text Edits

> A find/replace that asserts its own effect: target files like `ct-search`,
> set an `--expect`ation over the replacement count, preview with `--dry-run`,
> and write only when the verdict holds.

`ct-edit` turns "make this change" into a framed, self-checking operation. It
selects files with the same predicates as `ct-search`, computes **every**
replacement first, classifies the total against `--expect` into a
`SUCCESS`/`ERROR` verdict, and **writes only when the verdict is `SUCCESS` and
`--dry-run` is not set**. So an edit that matched the wrong number of sites fails
loudly and changes nothing, instead of silently doing the wrong thing. Reachable
directly or as `ct edit`.

This document is the canonical reference for `ct-edit`. It is also what the tool
prints for `ct-edit --explain` (`--explain md`); `ct-edit --explain json` prints
the equivalent MCP / tool-use definition.

## When to use it

- Rename or rewrite a token and assert the blast radius: `--expect =1` (exactly
  one), `--expect 3` (at least three), `--expect -10` (fewer than ten).
- Preview before touching disk with `--dry-run`, then re-run to apply.
- Confirm a string is gone after a refactor: `--find OLD --replace NEW`, or run a
  search with `ct-search --expect none`.
- Reversibility is via your VCS: review `git diff`, `git restore` to undo.

## Targeting (same vocabulary as ct-search)

| Option       | Argument  | Meaning                                                                              |
| ------------ | --------- | ----------------------------------------------------------------------------------- |
| `--base`     | `PATH`    | Root to edit. A **file** edits just that file; a **directory** is descended. Default `.` |
| `--name`     | `PATTERN` | Limit to files whose name matches; `\|`-separated alternatives, each promoted and anchored. |
| `--hidden`   | —         | Include dot-entries. Default: skipped.                                               |
| `--follow`   | —         | Follow symlinks while traversing.                                                    |

Only regular files are edited. Files that are not valid UTF-8 text are skipped.

## The edit

| Option      | Argument  | Meaning                                                                                       |
| ----------- | --------- | -------------------------------------------------------------------------------------------- |
| `--find`    | `PATTERN` | Text to find (substring → glob → regex promoted). Matched **per line** (patterns do not cross newlines). |
| `--replace` | `TEXT`    | Replacement. With a **regex** `--find`, `$1`/`${name}` expand (use `$$` for a literal `$`); with a **literal or glob** `--find`, the replacement is literal. |
| `--expect`  | `SPEC`    | Verdict expectation over the **total replacement count**: `any` (≥1, default), `none` (==0), `N` (≥N), `=N` (==N), `+N` (>N), `-N` (<N). |
| `--dry-run` | —         | Compute and show the change and verdict, but write nothing.                                   |

Replacements within a file preserve every untouched byte — line terminators,
indentation, and surrounding text are left exactly as they were.

## Output

| Option    | Effect                                                                 |
| --------- | --------------------------------------------------------------------- |
| `--quiet` | Suppress the per-site diff; print only the summary line.              |
| `--json`  | Emit a structured result instead of text.                            |

Text mode prints each changed line as `path:line:- before` then `path:line:+ after`,
followed by a summary: `N replacement(s) in M file(s) -> RESULT (status)`, where
status is `applied`, `dry-run, not written`, or `verdict ERROR, not written`.

`--json` emits:

```json
{
  "tool": "ct-edit",
  "verdict": "SUCCESS",
  "dry_run": false,
  "applied": true,
  "replacements": 3,
  "files_changed": 2,
  "sites": [ { "path": "src/a.rs", "line": 12, "before": "...", "after": "..." } ]
}
```

### Documentation

| Option                 | Effect                                                            |
| ---------------------- | ---------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                      |
| `-V`, `--version`      | Version.                                                         |

## Why it is not allowlisted

Unlike `ct-test`, `ct-edit` does not launch arbitrary programs — it only rewrites
text — so the command allow-gate does not apply. Its safety comes from `--dry-run`
(preview), `--expect` (a precondition that blocks a surprising blast radius), and
your VCS (review and undo). Scope edits with `--base`/`--name` and preview broad
changes before applying.

## Run bounds and liveness

Every suite tool is bounded and observable the same way:

| Option             | Argument   | Effect                                                            |
| ------------------ | ---------- | ----------------------------------------------------------------- |
| `--timeout`        | `SECS`     | Abort the scan (exit `2`, with a one-line message) if it exceeds SECS seconds (fractional allowed). |
| `--heartbeat`      | `SECS`     | Print a liveness pulse every SECS seconds while the run is in progress. |
| `--heartbeat-emit` | `TEMPLATE` | Pulse template. Tokens: `{ELAPSED}` (whole seconds so far) `{TOOL}`. Default: `[{ELAPSED}s]`. |
| `--heartbeat-to`   | `stderr\|stdout` | Stream for pulses. Default: `stderr`.                       |

The timeout bound covers the scan/compute phase only: once a `SUCCESS` verdict
begins writing, every write completes — a timeout can never leave a file
half-written.

## Exit status

| Code | Meaning                                                        |
| ---- | ------------------------------------------------------------- |
| `0`  | verdict `SUCCESS` (the expectation was met; written unless `--dry-run`) |
| `1`  | verdict `ERROR` (the expectation was not met; nothing written) |
| `2`  | usage or runtime error (e.g. a file could not be written)     |

## Examples

```sh
# Preview a one-site rename across the crate; nothing is written.
ct-edit --base src --name '*.rs' --find 'old_api(' --replace 'new_api(' \
  --expect =1 --dry-run

# Apply it for real, still asserting exactly one site.
ct-edit --base src --name '*.rs' --find 'old_api(' --replace 'new_api(' --expect =1

# Regex find with a capture; apply across one file.
ct-edit --base src/version.rs --find 'v(\d+)\.(\d+)' --replace 'v$1_$2'

# Machine-readable result for an agent.
ct-edit --base config --name '*.conf' --find DEBUG --replace INFO --json
```
