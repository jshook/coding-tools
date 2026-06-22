# ct-patch — Structured, Format-Preserving Edits

> Address a node by path and `--set` or `--delete` it in JSON / JSONC / JSONL.
> Everything outside the changed node — comments, indentation, key order, blank
> lines, trailing commas — is preserved exactly.

`ct-patch` edits structured documents *surgically*: it parses the file, locates
the node at your path, and splices only that node's bytes. The rest of the file is
returned untouched, so a one-value change never reformats the document or drops a
comment. Like `ct-edit`, it is framed by `--expect` and previewable with
`--dry-run`, and writes only when the verdict holds. Reachable directly or as
`ct patch`.

This document is the canonical reference for `ct-patch`. It is also what the tool
prints for `ct-patch --explain` (`--explain md`); `ct-patch --explain json` prints
the equivalent MCP / tool-use definition.

## When to use it

- Change a config value without reflowing the file or losing comments
  (`--set .server.port=8080`).
- Add or remove a key (`--set .feature.enabled=true`, `--delete .legacy`).
- Edit a value inside arrays by index (`--set .users[0].role='"admin"'`).
- Apply the same change to every record in a `.jsonl` file.

## Formats

Detected from the file extension, or forced with `--format`:

| Format  | Extensions          | Backend / notes                                                  |
| ------- | ------------------- | --------------------------------------------------------------- |
| `json`  | `.json`             | `jsonc-parser` byte-span splice (strict preservation).          |
| `jsonc` | `.jsonc`            | JSON with comments and trailing commas (both preserved).       |
| `jsonl` | `.jsonl`, `.ndjson` | One JSON value per line; each op applies to **every** non-blank line. |
| `yaml`  | `.yaml`, `.yml`     | Pure-Rust `yaml-edit`; comment-preserving (a structural edit may relocate an adjacent comment). |

Files whose format is neither detected nor forced are skipped.

## Targeting (same vocabulary as ct-search / ct-edit)

| Option     | Argument  | Meaning                                                                  |
| ---------- | --------- | ----------------------------------------------------------------------- |
| `--base`   | `PATH`    | A **file** patches just that file; a **directory** is descended. Default `.` |
| `--name`   | `PATTERN` | Limit to files whose name matches (`\|`-separated, promoted, anchored).  |
| `--mode`   | `literal\|glob\|regex` | Pin how `--name` is interpreted (promotion off), as in `ct-search`/`ct-edit`. |
| `--hidden` | —         | Include dot-entries.                                                     |
| `--follow` | —         | Follow symlinks.                                                         |
| `--no-ignore` | —      | Walk gitignored / `.ignore` files too (`.git` is always skipped). Default: skip what git would. |
| `--format` | `FMT`     | Force `json`/`jsonc`/`jsonl`/`yaml` instead of detecting from the extension. |

## Operations

All operations are repeatable and applied in the order: `--set`, `--add`,
`--move-*`, `--delete`.

| Option         | Argument     | Meaning                                                              |
| -------------- | ------------ | ------------------------------------------------------------------- |
| `--set`        | `PATH=VALUE` | Set the node at `PATH` to `VALUE` (creates a missing object key; an index equal to the array length appends). |
| `--add`        | `PATH=VALUE` | Append `VALUE` to the array/sequence at `PATH` — no index to compute. |
| `--delete`     | `PATH`       | Remove the node at `PATH`, taking its separating comma. An unresolved path is a no-op. |
| `--move-first` | `PATH`       | Move the array element selected by `PATH` to the front of its list. |
| `--move-last`  | `PATH`       | …to the end of its list.                                            |
| `--move-up`    | `PATH`       | …one position earlier.                                              |
| `--move-down`  | `PATH`       | …one position later.                                                |

### Paths

A path is dot-separated keys with array selectors; a leading `.` is optional:

- `[N]` — array index (`.server.ports[0]`).
- `[key=value]` — the array element that is an object whose `key` equals `value`
  (`.servers[name=web].port`). Scalars compare by their literal text.

Examples: `.server.host`, `users[2].name`, `.servers[name=web]`. *(Keys
containing `.`, `[`, or `=` are not addressable in this version.)*

### Values

A `--set`/`--add` `VALUE` is parsed as JSON when it can be (`8080`, `true`,
`null`, `[1,2]`, `{"k":1}`); otherwise it is taken as a string. To force a string
that looks like JSON, quote it as JSON: `--set .name='"true"'`. Inserted values
are written compactly; the surrounding document formatting is preserved.

Values are payload-typed: `file:PATH` reads the value **verbatim as a
string node** — exact bytes, never re-parsed as JSON — which is how a
multi-line string gets into a document with zero quoting
(`--set 'tool.notes=file:notes.txt'`); `text:VALUE` escapes a literal value
that genuinely begins with `file:` or `text:` (it still gets the normal
JSON-or-string parse).

### YAML coverage

The YAML backend currently supports `--set` (replace an existing key) and
`--delete`, both comment-preserving. `--add`, the `--move-*` verbs, and
array-index/`[key=value]` paths are JSON-family only for now (yaml-edit 0.2
mis-indents structural inserts); they error clearly on YAML rather than risk
producing malformed output.

## Output

| Option      | Effect                                                          |
| ----------- | -------------------------------------------------------------- |
| `--dry-run` | Compute and report the changes and verdict, but write nothing. |
| `--quiet`   | Suppress the per-file lines; print only the summary.            |
| `--json`    | Emit a structured result (see below).                          |
| `--json-pretty` | Like `--json`, but pretty-printed (indented).              |

Text mode prints `path: N change(s)` per changed file, then a summary:
`N change(s) in M file(s) -> RESULT (status)`. `--json` emits:

```json
{
  "tool": "ct-patch",
  "verdict": "SUCCESS",
  "dry_run": false,
  "applied": true,
  "changes": 2,
  "files_changed": 1,
  "files": [ { "path": "config.json", "changes": 2 } ]
}
```

### Documentation

| Option                 | Effect                                                            |
| ---------------------- | ---------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                      |
| `-V`, `--version`      | Version.                                                         |

## Verdict & expectations

The verdict is the `--expect`ation applied to the **total number of changes** made
(across all ops and files): `any` (≥1, default), `none` (==0), `N` (≥N), `=N`,
`+N`, `-N`. The edit is written only when the verdict is `SUCCESS` and `--dry-run`
is not set. Like `ct-edit`, `ct-patch` runs no external programs, so it is not
subject to the `ct-test` allowlist; safety comes from `--dry-run`, `--expect`, and
your VCS.

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

| Code | Meaning                                                       |
| ---- | ----------------------------------------------------------- |
| `0`  | verdict `SUCCESS` (written unless `--dry-run`)               |
| `1`  | verdict `ERROR` (nothing written)                           |
| `2`  | usage or runtime error (bad path, parse failure, write error) |

## Examples

```sh
# Change a value, keeping comments and layout intact; preview first.
ct-patch --base tsconfig.json --set .compilerOptions.strict=true --dry-run

# Add a key and delete another, asserting exactly two changes.
ct-patch --base config.jsonc --set .features.beta=true --delete .deprecated --expect =2

# Select an array element by an object predicate, then set a field on it.
ct-patch --base data.json --set '.servers[name=web].port=8443'

# Append to a list without computing an index, and reorder it.
ct-patch --base data.json --add '.servers={"name":"cache","port":6379}'
ct-patch --base data.json --move-first '.servers[name=cache]'

# YAML: replace a value, comments and layout preserved.
ct-patch --base config.yaml --set .server.port=9090

# Stamp every record in a JSONL file.
ct-patch --base events.jsonl --set .processed=true --json
```
