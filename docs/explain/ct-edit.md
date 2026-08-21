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
| `--find`    | `ANCHOR`  | Text to find, read as **literal** unless `--mode` says otherwise. A single-line anchor is matched **per line**; a multi-line payload matches as a **block** (below). |
| `--replace` | `TEXT`    | Replacement. With a **regex** `--find`, `$1`/`${name}` expand (use `$$` for a literal `$`); otherwise the replacement is literal. For a block `--find`, an empty payload deletes the matched lines. |
| `--mode`    | `literal\|glob\|regex\|auto` | How **every** pattern in the call is interpreted. Absent, `--find` is **literal** and `--name` promotes; `regex`/`glob` pin the other readings, and `auto` restores substring → glob → regex promotion for the whole call. Note that any explicit `--mode` pins `--name` too (see *`--mode` pins the file filter too*), so to change only the anchor's reading with a glob `--name`, prefer `file:`/`stdin:`. |
| `--nth`     | `N`       | Apply only the **Nth** match (1-based) in selection order: files in walk order, then top to bottom. Implies `--expect =1`. |
| `--at`      | `FILE:LINE` | Apply only the match **starting there**. `FILE` may be a whole-component path suffix (`cli/ct_edit.rs`); `LINE` is 1-based. Implies `--expect =1`. |
| `--expect`  | `SPEC`    | Verdict expectation over the **total replacement count**: `any` (≥1, default), `none` (==0), `N` (≥N), `=N` (==N), `+N` (>N), `-N` (<N). Under `--nth`/`--at` the default becomes `=1`. |
| `--dry-run` | —         | Compute and show the change and verdict, but write nothing.                                   |

Replacements within a file preserve every untouched byte — line terminators,
indentation, and surrounding text are left exactly as they were.

### `--find` is literal by default

The rest of the suite promotes a pattern — substring → glob → regex — because
the other tools take *queries*, and a query is usually meant loosely. `--find`
is not a query. It is an **anchor**: a piece of code, copied out of the file it
is meant to match, and code is made of `(`, `)`, `[`, `]`, `.`, `?`, `|` and
`*`. Promoting it reads those as syntax, which at best misses the line it was
copied from and at worst matches a *different* one.

So a bare `--find` is matched **verbatim**:

```sh
ct edit --base src --name '*.rs' \
  --find 'reg::<Foo[T]>();' --replace 'reg::<Bar>();' --expect =1
```

`--mode` asks for another reading. `regex` and `glob` pin one; `auto` restores
promotion, which is what every `--find` used to get:

```sh
ct edit --mode regex --find 'v(\d+)' --replace 'ver$1' --expect =1   # captures
ct edit --mode auto  --find 'foo|bar' --replace 'baz'                # promotion
```

`--mode` still pins **every** pattern in the call, `--name` included; absent, only
`--find` is literal and `--name` keeps promoting as a glob. A `file:`/`stdin:`
payload was always literal and still is.

### Picking one site: `--nth` / `--at`

The suite's answer to a non-unique anchor is to make it unique — widen `--find`
until it spans a distinguishing line. That stays the right default, because an
anchor that names its own site survives the file changing around it. But when
you can already see the list and just want the second one, capturing a whole
block first is overkill:

```sh
ct search --base src --grep 'let v = compute' --detail
# src/a.rs:12:    let v = compute(1);
# src/a.rs:13:    let v = compute(2);

ct edit --base src --find 'let v = compute' --replace 'let w = compute' --nth 2
ct edit --base src --find 'let v = compute' --replace 'let w = compute' --at a.rs:13
```

`--nth N` counts matches in **selection order**: files in walk order, then top to
bottom within a file. `--at FILE:LINE` names the site directly, which is what the
`path:line:text` rows of `ct search --detail` hand you. Both imply `--expect =1`,
so a pick is still a claim the run has to make good on — and a pick that lands on
nothing reports where the anchor *did* match, rather than reading as a bad
pattern:

```
ct-edit: --nth 9, but the selection holds 3 match(es); the anchor matched at
         src/a.rs:12, src/a.rs:13, src/a.rs:14
```

Neither combines with `--script`: inside a batch, an edit narrows with `file=`
and states its own `expect=`.

### Payload schemes: `file:` / `stdin:` / `text:`

`--find` and `--replace` are payload-typed:

| Scheme | The value is |
| ------ | ------------ |
| `file:PATH` | the file's contents, read verbatim (exact bytes, UTF-8) |
| `stdin:` | all of standard input, read verbatim (no argument) |
| `text:VALUE` | the literal remainder — the escape hatch for a value that genuinely begins with `file:`, `stdin:`, or `text:` |

Only those three exact prefixes are recognised; `http://…` and `std::fmt`
are unaffected. A `file:`- or `stdin:`-sourced payload is **verbatim**: never
promoted, so its match mode defaults to literal.

**Payloads containing `` ` ``, `$`, `!`, or quotes must not be written
inline.** Under a POSIX shell an inline `text:` payload is expanded before
`ct-edit` ever sees it — a backtick inside a code comment becomes a command
substitution, `$foo` becomes empty, and the anchor that reaches the tool is
not the one you wrote. `file:` and `stdin:` both dodge this completely,
because the bytes never pass through word expansion:

```sh
# the cheap safe path: no temp file, no quoting to get right
printf '%s' "$BLOCK" | ct-edit --base src --find stdin: --replace 'text:' --expect =1

# the reusable safe path: write the payload once, reference it by path
ct-edit --base src --find file:/tmp/anchor.txt --replace file:/tmp/new.txt --expect =1
```

Standard input is read **once** per process, so several `stdin:` payloads in
one call all resolve to the same text — use `file:` when `--find` and
`--replace` both need out-of-band values.

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

#### `--mode` pins the file filter too

`--mode` switches promotion off for **every** pattern in the invocation, and
that includes `--name`. So

```sh
ct-edit --base src --name '*.rs' --mode literal --find 'reg::<Foo[T]>();' ...
```

looks for a file literally named `*.rs`, selects nothing, and returns a
verdict that says nothing about the anchor. To pin *only* the anchor, source
it out of band — a `file:`/`stdin:` payload is verbatim by construction, so
`--mode` is not needed at all:

```sh
printf '%s' 'reg::<Foo[T]>();' \
  | ct-edit --base src --name '*.rs' --find stdin: --replace 'text:' --expect =1
```

`--mode literal` is still right when the selection carries no glob — a single
file in `--base`, or a literal `--name`. When it does, the empty-selection
diagnosis below names this interaction rather than leaving it to be
rediscovered.

### When nothing matched: the diagnosis

A `0 replacement(s) -> ERROR` never stands on its own. Every zero-match run
says *why*, positioned in the source, so the next call is a correction rather
than an investigation.

A **block** `--find` reports the nearest miss described above. A **single-line**
`--find` gets the equivalent, in one of four shapes:

- **The text is there, but the pattern was promoted past it.** Only reachable
  under an explicit `--mode glob`/`regex`/`auto`, since a bare `--find` is
  literal. A line like `reg::<Foo[T]>();` is a *valid* regex whose `[T]` is a
  character class — so read as one, it does not match its own text. ct-edit
  finds the text verbatim, points at it, and names the fix:

  ```
  nearest miss: src/app.rs:3243: the --find text is here, but the pattern was promoted
    pattern:  'reg::<Foo[T]>();'
    found:    '    reg::<Foo[T]>();'
    note:     the --find text carries regex metacharacters, and the stated --mode
              read it as a regex, which did not match; the text is present
              verbatim on 1 line(s) — drop the --mode (a bare --find is already
              literal), or pass the anchor as file:PATH or stdin: to pin only
              --find
  ```

  This was the single most common single-line failure while `--find` promoted
  by default; dropping the `--mode` prevents it outright.
- **Whitespace drift.** A line matches once leading/trailing whitespace is
  ignored. Both rows are printed quoted, because the difference is invisible
  otherwise.
- **Nothing close.** No line matched and none contains the text even ignoring
  whitespace; the note reports how many files were scanned and how the pattern
  was read, which separates "wrong pattern" from "wrong files".
- **Nothing was read at all.** The selection matched **0 files**, so the
  verdict says nothing about the pattern — the fault is in `--base`/`--name`.
  This one is reported for block finds too.
- **The pick took nothing.** With `--nth`/`--at`, the anchor matched but the
  ordinal or position selected none of it. The note says how many matches the
  anchor found and where, so the fix is to move the pick rather than rewrite
  the pattern. Under `--json` this is a `pick_miss` object instead of
  `nearest_miss`.

Under `--json`, the same diagnosis is the `nearest_miss` object (with
`"kind": "line"` or `"kind": "block"`), alongside a `files_scanned` count.

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

# A verbatim code anchor, sourced out of band so it is never promoted --
# unlike --mode, that pins only the anchor and leaves --name a glob.
printf '%s' 'todo!("wire this")' \
  | ct-edit --base src --name '*.rs' --find stdin: --replace 'text:wire()' --expect =1

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
  ct edit --base src --name '*.rs' --find 'foo_bar' --replace 'foo_baz' --dry-run
  ```
- **Change one site of a repeated anchor by position, taken straight from a ct search --detail row; --at implies --expect =1.**
  ```sh
  ct edit --base src --find 'let v = compute' --replace 'let w = compute' --at a.rs:13 --dry-run
  ```
- **Rename in one file, refusing the write unless exactly the expected number of sites change.**
  ```sh
  ct edit --base src/steer.rs --find 'old_name' --replace 'new_name' --expect 3
  ```
