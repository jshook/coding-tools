# ct-view — Focused File Viewer

> Show a file's lines by range, or the regions around a pattern with context —
> a bounded, deterministic read instead of dumping the whole file.

`ct-view` reads **one** file and prints only what you ask for: a line range, or
the windows around a `--match` pattern with `--context` lines on each side
(overlapping windows merge, like `grep -C`). It is read-only, so it carries no
allow-gate. Reachable directly or as `ct view`.

This document is the canonical reference for `ct-view`. It is also what the tool
prints for `ct-view --explain` (`--explain md`); `ct-view --explain json` prints
the equivalent MCP / tool-use definition.

## When to use it

- Read a specific span (`--range 40:80`) without paging a whole file.
- See just the neighbourhoods of a symbol or string (`--match foo --context 3`).
- Get line-numbered output you can feed straight into a follow-up `ct-edit`.
- Consume the result as data (`--json`) — `{n, text}` per line.

## Options

| Option        | Argument | Meaning                                                                                          |
| ------------- | -------- | ------------------------------------------------------------------------------------------------ |
| `PATH`        | —        | The file to view (positional, required).                                                         |
| `--range`     | `SPEC`   | Line range `A:B` (1-based, inclusive); also `A:` (to end), `:B` (from start), or `A` (one line). |
| `--match`     | `PATTERN`| Show only lines matching the pattern (promoted; see *Pattern matching*), with `--context` around each. Accepts `file:PATH` / `text:VALUE`; a multi-line payload matches as a line-anchored literal **block**. |
| `--mode`      | `literal\|glob\|regex` | Pin how `--match` is interpreted — promotion **off**. State `literal` when the pattern is verbatim code. |
| `--context`, `-C` | `N`  | Lines of context shown around each `--match` hit. Default: `2`.                                  |
| `--limit`     | `N`      | Cap the number of lines emitted.                                                                 |
| `--plain`     | —        | Suppress the line-number gutter in text output.                                                  |
| `--json`      | —        | Emit a structured JSON result instead of text (see *JSON result*).                               |
| `--json-pretty` | —      | Like `--json`, but pretty-printed (indented).                                                    |

If neither `--range` nor `--match` is given, the whole file is shown (subject to
`--limit`). `--match` and `--range` are independent selectors; `--match` takes
precedence if both are supplied.

### Documentation

| Option                 | Effect                                                            |
| ---------------------- | ---------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                      |
| `-V`, `--version`      | Version.                                                         |

## Text output

Each shown line is printed as a right-aligned line-number gutter, two spaces, then
the line text. Non-contiguous groups (from merged `--match` windows, or a sparse
selection) are separated by a `--` line. `--plain` drops the gutter and prints the
raw lines only.

## JSON result

`--json` emits a single object:

```json
{
  "tool": "ct-view",
  "path": "src/lib.rs",
  "total_lines": 240,
  "shown": 7,
  "lines": [ { "n": 40, "text": "..." }, { "n": 41, "text": "..." } ],
  "matched": true
}
```

`matched` is present only when `--match` was used (`true`/`false`). `shown` is the
number of `lines` emitted; line gaps are implied by non-consecutive `n`.

## Pattern matching

`--match` uses the suite's substring → glob → regex promotion and is searched
**unanchored** against each line: text with no metacharacters is a literal
substring; glob metacharacters (`*` `?` `[ ]`) that are not a valid regex are a
glob; otherwise it is a regex. (See `ct-search --explain` for the full table.)
`--mode literal|glob|regex` pins the interpretation (promotion off).

`--match` is payload-typed: `file:PATH` reads the pattern verbatim from a
file (literal by default), `text:VALUE` escapes the prefix. A **multi-line**
pattern matches as a line-anchored literal block — K lines match K
consecutive source lines byte-for-byte — and the context window expands
around the whole matched region; a block that matches nothing reports its
**nearest miss** to stderr (best-aligned candidate, first diverging line)
before the clean-negative exit. This is the "show me the block in context
before editing" step of a block edit.

## Run bounds and liveness

Every suite tool is bounded and observable the same way:

| Option             | Argument   | Effect                                                            |
| ------------------ | ---------- | ----------------------------------------------------------------- |
| `--timeout`        | `SECS`     | Abort the run (exit `2`, with a one-line message) if it exceeds SECS seconds (fractional allowed). |
| `--heartbeat`      | `SECS`     | Print a liveness pulse every SECS seconds while the run is in progress. |
| `--heartbeat-emit` | `TEMPLATE` | Pulse template. Tokens: `{ELAPSED}` (whole seconds so far) `{TOOL}`. Default: `[{ELAPSED}s]`. |
| `--heartbeat-to`   | `stderr\|stdout` | Stream for pulses. Default: `stderr`.                       |

## Exit status

| Code | Meaning                                                       |
| ---- | ------------------------------------------------------------- |
| `0`  | the file was read and the selection shown                     |
| `1`  | `--match` was given and matched nothing (a clean negative)    |
| `2`  | usage or runtime error (e.g. the file could not be read)      |

## Examples

```sh
# Read lines 40–80 of a file.
ct-view src/lib.rs --range 40:80

# Show every use of `Verdict` with 3 lines of context, as JSON.
ct-view src/bin/ct-test.rs --match Verdict --context 3 --json

# First 20 lines, no gutter.
ct-view README.md --range :20 --plain
```
