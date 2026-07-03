# ct-edit — Verifiable Text Edits

> A find/replace that asserts its own effect: target files like `ct-search`,
> set an `--expect`ation over the replacement count, preview with `--dry-run`,
> and write only when the verdict holds. Multi-line payloads edit whole
> blocks; `--script` runs a batch of edits atomically.

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

> **Pair `ct view` reads with `ct edit` writes.** The harness's own `Edit` tool requires that it has *read* a file (via its `Read` tool) before it will modify it. A `ct view` read does not satisfy that precondition, so if you read with `ct view` and then reach for the harness `Edit`, it will refuse. Mutate with `ct edit` instead — it needs no prior harness read.

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
| `--no-ignore` | —        | Walk gitignored / `.ignore` files too (`.git` is always skipped). Default: skip what git would. |

Only regular files are edited. Files that are not valid UTF-8 text are skipped.

## The edit

| Option      | Argument  | Meaning                                                                                       |
| ----------- | --------- | -------------------------------------------------------------------------------------------- |
| `--find`    | `PATTERN` | Text to find (substring → glob → regex promoted). A single-line pattern is matched **per line**; a multi-line payload matches as a **block** (below). |
| `--replace` | `TEXT`    | Replacement. With a **regex** `--find`, `$1`/`${name}` expand (use `$$` for a literal `$`); otherwise the replacement is literal. For a block `--find`, an empty payload deletes the matched lines. |
| `--mode`    | `literal\|glob\|regex` | Pin how `--find` (and `--name`) is interpreted — promotion **off**. State this when the pattern is verbatim code: a literal anchor like `todo!("…")` would otherwise promote to a regex and miss its own text. |
| `--expect`  | `SPEC`    | Verdict expectation over the **total replacement count**: `any` (≥1, default), `none` (==0), `N` (≥N), `=N` (==N), `+N` (>N), `-N` (<N). |
| `--dry-run` | —         | Compute and show the change and verdict, but write nothing.                                   |

Replacements within a file preserve every untouched byte — line terminators,
indentation, and surrounding text are left exactly as they were.

### Payload schemes: `file:` / `text:`

`--find` and `--replace` are payload-typed: `file:PATH` reads the value
verbatim from a file (exact bytes; never promoted — its match mode defaults
to literal), and `text:VALUE` is the escape hatch for a value that genuinely
begins with `file:` or `text:`. Only those two exact prefixes are
recognised; `http://…` and `std::fmt` are unaffected. Writing payloads to
files and passing `file:` references avoids every shell-quoting hazard
around code (`$`, quotes, backslashes, newlines).

### Block find/replace

A multi-line `--find` payload matches as a **line-anchored literal block**:
a find block of K lines matches K consecutive source lines exactly,
byte-for-byte, whitespace significant (`--mode glob/regex` on a block is
reserved and refused). The whole matched block is replaced by the
`--replace` payload's lines; an empty replace payload deletes the block's
lines entirely. When a block matches nothing, the **nearest miss** is
reported: the candidate site with the longest matching prefix and the first
line where it diverged — so whitespace drift or an already-applied change
is visible without bisecting.

```sh
# One verbatim block edit, no quoting anywhere: write the payloads as
# files, then anchor on them.
ct-edit --base src --name '*.rs' \
  --find file:target/find.block --replace file:target/replace.block \
  --expect =1 --dry-run
```

#### How a payload is split into lines

The rule is the same for `file:`, `text:`, and inline multi-line payloads, and
matters because it decides whether a `--find` is a per-line pattern or a K-line
block:

- A **single trailing newline is a terminator, not a line** — a payload of K
  text lines plus one final `\n` is K lines, so a 2-line anchor file matches 2
  source lines (not 3). This is what every editor and file-writer produces.
- **`CRLF` is normalized to `LF`** — a trailing `\r` on each line is dropped, so
  an anchor file saved by a Windows editor still matches `LF` source.
- For `--find` only, **trailing blank lines are trimmed** — editors commonly
  leave a final empty line, which would otherwise become a phantom empty line at
  the tail of the block and make the match fail. Interior blank lines and
  whitespace-only lines are significant and kept; only exactly-empty trailing
  lines are removed. (`--replace` keeps every line, since a trailing blank line
  there may be intentional.)

When a block still fails to match, the nearest miss names the parsed block's
line count (`diverges at its line 3 of 3`) and, when the expected line is empty,
adds a `note:` flagging the likely stray blank line.

#### Blank-line tolerant matching: `--squeeze-blank`

By default a block match is exact, line for line — including how many blank
lines separate two anchors. Pass `--squeeze-blank` to make a **maximal run of
blank lines** (empty or whitespace-only) in the `--find` anchor match a run of
**one or more** blank lines in the source, regardless of count. This keeps an
anchor robust when the source has gained or lost blank lines between two
non-blank lines you are anchoring on. Non-blank lines still match byte-for-byte,
and a blank run in the anchor still requires at least one blank line in the
source. The flag has no effect on a single-line `--find` or on `--replace`; the
whole matched span (which can be wider than the anchor) is what `--replace`
overwrites, so the result's blank lines are whatever `--replace` specifies. In a
`--script`, request it per edit with `squeeze=true` instead.

## Scripts: `--script` (.ctb)

`--script PATH` runs a **batch** of edits from a ct block document under the
suite's prepare/confirm/write standard: the whole script is simulated and
judged in memory first, and **no file changes unless every edit passes** —
there is no flag that makes a partial write possible.

```
#% edit expect="=1" file=src/ast.rs
#% find
            Value::U64(v) => v.to_string(),
#% replace
            Value::U64(v) => v.to_string(),
            Value::I64(v) => v.to_string(),
#% end
```

- `#% edit` opens an edit; attributes: `expect=` (same SPEC vocabulary;
  **default `=1` in scripts** — anchored structural edits mean "exactly
  here", and the stricter default is the safer one inside an atomic batch),
  `mode=` (`literal` default — promotion is off in scripts), `file=`
  (narrows **within** the invocation's `--base`/`--name` selection), and
  `squeeze=` (`true`/`false`, default `false` — the per-edit equivalent of
  `--squeeze-blank` for block finds).
- `#% find` / `#% replace` carry the payloads verbatim, including leading
  whitespace; an empty `replace` section deletes the matched lines.
  `#% end` closes the edit. Attribute values split at the first `=`
  (`expect==1` works), but `expect="=1"` is the preferred spelling.
- Outside edits, blank lines and `#`-comments are ignored. `--fence STR`
  changes the directive prefix for payloads containing `#%` at line start.

**Semantics.** Phase 1 simulates the script in memory, in order: each edit
matches the buffers *as transformed by earlier edits* (cascade — so "add a
variant, then extend the arm you just added" works), and its `expect` is
judged there; every changed file is also pre-flighted for writability.
Phase 2 writes the final buffers only when every edit passed, so the
verdict is exactly faithful to what gets written. `--no-cascade` matches
every edit against pristine content instead, and any two edits touching the
same line become a usage error. Any failing edit → batch `ERROR`, **zero
writes**, exit `1`; failing block edits carry their nearest miss.

## Output

| Option    | Effect                                                                 |
| --------- | --------------------------------------------------------------------- |
| `--quiet` | Suppress the per-site diff; print only the summary line.              |
| `--json`  | Emit a structured result instead of text.                            |
| `--json-pretty` | Like `--json`, but pretty-printed (indented).                  |

Text mode prints each changed line as `path:line:- before` then `path:line:+ after`
(block sites print one row per payload line, at the block's start line),
followed by a summary: `N replacement(s) in M file(s) -> RESULT (status)`, where
status is `applied`, `dry-run, not written`, or `verdict ERROR, not written`.
Script runs prefix each site row with its edit ordinal (`[3/12] …`), then print
a per-edit verdict table and the batch summary.

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

A script run replaces `sites` with a per-edit array (and reports
`"script"` and `"cascade"`):

```json
{
  "tool": "ct-edit", "script": "edits.ctb", "verdict": "ERROR",
  "cascade": true, "dry_run": false, "applied": false,
  "replacements": 1, "files_changed": 1,
  "edits": [
    { "ordinal": 1, "expect": "=1", "mode": "literal",
      "replacements": 1, "verdict": "SUCCESS", "sites": [ … ] },
    { "ordinal": 2, "expect": "=1", "mode": "literal",
      "replacements": 0, "verdict": "ERROR",
      "nearest_miss": { "path": "src/ast.rs", "line": 571,
                        "first_diverging_line": 3,
                        "expected": "…", "found": "…" } }
  ]
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
half-written. Script runs additionally pre-flight every changed file for
writability before the first write, so a write phase never starts that
cannot finish.

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

# A verbatim code anchor: pin literal so '(' and '!' are not regex.
ct-edit --base src --name '*.rs' --mode literal \
  --find 'todo!("wire this")' --replace 'wire()' --expect =1

# A block edit from payload files (zero quoting), previewed first.
ct-edit --base src --name '*.rs' \
  --find file:target/find.block --replace file:target/replace.block \
  --expect =1 --dry-run

# An atomic batch: all 12 edits verified in memory, then written together.
ct-edit --base polydat/src --name '*.rs' --script target/edits.ctb --dry-run
ct-edit --base polydat/src --name '*.rs' --script target/edits.ctb
```

- **Preview a rename across Rust sources before writing, instead of sed -i 's/foo_bar/foo_baz/g'.**
  ```sh
  ct edit --base src --name '*.rs' --find 'foo_bar' --replace 'foo_baz' --mode literal --dry-run
  ```
- **Rename in one file, refusing the write unless exactly the expected number of sites change.**
  ```sh
  ct edit --base src/steer.rs --find 'old_name' --replace 'new_name' --mode literal --expect 3
  ```
