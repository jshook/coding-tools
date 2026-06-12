# ct-tree — Annotated File-Tree Report

> Walk a directory for chosen file types and report the effective tree with
> per-file line, word, and character counts — filtered by predicates, sorted by
> any column, and summarised at the level you ask for.

`ct-tree` is `tree` + `wc` with predicates: it finds the files you care about,
counts their lines/words/characters, applies metric and per-folder filters, sorts,
and prints a clean report — an indented tree (default), a flat list, or grouped
aggregates. Reachable directly or as `ct tree`.

This document is the canonical reference for `ct-tree`. It is also what the tool
prints for `ct-tree --explain` (`--explain md`); `ct-tree --explain json` prints
the equivalent MCP / tool-use definition.

## When to use it

- See where the bulk of a codebase lives (biggest files/folders).
- Answer "which `*.rs` files have more than 5000 lines, largest first?"
  (`--ext rs --min-lines 5001 --flat --sort lines --desc`).
- Get a per-extension or per-directory size breakdown (`--summary`).
- Feed the structured report to a tool (`--json`).

## Selection

| Option     | Argument  | Meaning                                                                  |
| ---------- | --------- | ----------------------------------------------------------------------- |
| `--base`   | `DIR`     | Root to walk, relative or absolute. Default `.`                         |
| `--name`   | `PATTERN` | File-name pattern; `\|`-separated alternatives, promoted and anchored.   |
| `--mode`   | `literal\|glob\|regex` | Pin how `--name`/`--ext` are interpreted — promotion **off**. |
| `--ext`    | `LIST`    | Restrict to extensions (comma-separated, no dots), e.g. `--ext rs,toml`. Added to `--name` as alternatives. |
| `--hidden` | —         | Include dot-entries. Default: skipped.                                   |
| `--follow` | —         | Follow symlinks while traversing.                                        |

Only regular files are measured; the tree shows the directories that contain them.

## Predicates

Metric predicates keep only files whose counts fall in range; per-folder
predicates keep only folders that directly contain a given number of matching
files. All are AND-combined.

| Option                                       | Keeps a file/folder when…              |
| -------------------------------------------- | -------------------------------------- |
| `--min-lines` / `--max-lines`                | line count is `>= ` / `<=` N           |
| `--min-words` / `--max-words`                | word count is `>=` / `<=` N            |
| `--min-chars` / `--max-chars`                | character count is `>=` / `<=` N       |
| `--min-files-per-folder` / `--max-files-per-folder` | its directory directly holds `>=` / `<=` N matching files |

## Sorting

| Option   | Argument                                  | Meaning                          |
| -------- | ----------------------------------------- | -------------------------------- |
| `--sort` | `path`\|`name`\|`lines`\|`words`\|`chars`\|`ext` | Sort key. Default `path`.   |
| `--desc` | —                                         | Sort descending (default ascending). |

In `--flat` the sort is global; in `--tree` it orders entries within each folder.

## Output mode & summarisation

Mutually exclusive; default `--tree`.

| Option      | Output                                                                          |
| ----------- | ------------------------------------------------------------------------------- |
| `--tree`    | Indented file tree; each file shows its counts, each folder its recursive subtotal, plus a grand total. *(default)* |
| `--flat`    | One matching file per line: `lines words chars  path`, then a totals line. Best for ranked lists. |
| `--summary` | Aggregate counts only, grouped by `--group`.                                    |
| `--json`    | Structured result (overrides the text modes).                                   |

| Option    | Argument                | Meaning                                              |
| --------- | ----------------------- | --------------------------------------------------- |
| `--group` | `ext`\|`dir`\|`none`    | Grouping for `--summary`: by file extension *(default)*, by immediate directory, or a single grand total. |

### Documentation

| Option                 | Effect                                                            |
| ---------------------- | ---------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                      |
| `-V`, `--version`      | Version.                                                         |

## JSON result

`--json` emits the filtered, sorted files plus aggregates:

```json
{
  "tool": "ct-tree",
  "base": "src",
  "files": [ { "path": "big.rs", "ext": "rs", "lines": 5321, "words": 18234, "chars": 142000 } ],
  "by_ext": [ { "group": ".rs", "files": 12, "lines": 40000, "words": 0, "chars": 0 } ],
  "totals": { "files": 12, "lines": 40000, "words": 0, "chars": 0 }
}
```

## Run bounds and liveness

Every suite tool is bounded and observable the same way:

| Option             | Argument   | Effect                                                            |
| ------------------ | ---------- | ----------------------------------------------------------------- |
| `--timeout`        | `SECS`     | Abort the run (exit `2`, with a one-line message) if it exceeds SECS seconds (fractional allowed). |
| `--heartbeat`      | `SECS`     | Print a liveness pulse every SECS seconds while the run is in progress. |
| `--heartbeat-emit` | `TEMPLATE` | Pulse template. Tokens: `{ELAPSED}` (whole seconds so far) `{TOOL}`. Default: `[{ELAPSED}s]`. |
| `--heartbeat-to`   | `stderr\|stdout` | Stream for pulses. Default: `stderr`.                       |

## Exit status

| Code | Meaning                                  |
| ---- | ---------------------------------------- |
| `0`  | at least one file is in the report       |
| `1`  | no file matched the selection/predicates |
| `2`  | usage or runtime error                   |

## Examples

```sh
# All *.rs files over 5000 lines, largest first (the headline query).
ct-tree --ext rs --min-lines 5001 --flat --sort lines --desc

# Annotated tree of the source, biggest folders/files first.
ct-tree --base src --ext rs --sort lines --desc

# Size breakdown by extension across the repo.
ct-tree --ext rs,toml,md --summary --group ext

# Only folders that hold at least 10 matching files.
ct-tree --ext rs --min-files-per-folder 10 --summary --group dir
```
