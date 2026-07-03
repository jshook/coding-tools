# ct-outline — Structural Outline

> Report the declarations in a file or tree — `kind name start:end` — so the
> next read can be a bounded `ct-view --range`, not a whole-file dump.

Sometimes a user or agent needs to know the *shape* of a file — what it
declares and where — before deciding what to actually read. The traditional
answers are dumping the file, or squinting at `grep -n '^fn \|^class '` output
with hand-tuned patterns per language. `ct-outline` turns that into one
declarative command: heuristic, per-language detection of declarations, each
reported with its kind, name, and line span, filterable and framed by the
suite's usual verdict machinery.

This document is the canonical reference for `ct-outline`. It is also what the
tool prints for `ct-outline --explain` (`--explain md`); `ct-outline --explain
json` prints the equivalent MCP / tool-use definition.

## Replaces patterns like

```sh
# per-language guesswork, no spans, no nesting, no verdict
grep -n '^pub fn \|^fn \|^struct \|^impl ' src/patch.rs

# or worse: reading the whole file to find one function
cat src/patch.rs
```

with:

```sh
ct-outline --base src/patch.rs
ct-outline --base src/patch.rs --match apply_doc        # where is this symbol?
ct-view src/patch.rs --range 538:606                    # then read exactly that
```

## When to use it

- Learn what a file declares — and where — before reading any of it.
- Resolve a symbol name to a `start:end` span to feed `ct-view --range`.
- Assert structure as a test: "this file defines `Verdict` exactly once"
  (`--match Verdict --expect =1`).
- Sweep a tree for declarations by name or kind without content-grepping.

`ct-outline` is **read-only** and shares the suite's verdict-and-emit model:
a `--question`, an `--expect`ation over the matched-entry count, `--emit`
templates, and exit status following the verdict.

## What it is (and is not)

Detection is **heuristic**: per-language rule packs (see *Languages*) match
declaration forms by line patterns and derive spans from block structure
(braces for Rust, indentation for Python, heading levels for Markdown). It is
a *comprehension aid*, not a parser or ground truth — a declaration inside a
string literal or an unusual macro can fool it, and generated or minified code
may outline poorly. When the outline and the code disagree, the code wins;
verify with `ct-view` before acting on a span.

**Start lines are exact; end lines are best-effort.** When the block heuristic
cannot derive an end, the span renders as `start:?` (JSON: `"end": null`) —
the tool never implies precision it does not have. A `start:?` span is still
useful: `ct-view FILE --range START:` reads from there to the end of file.

## Targeting (same vocabulary as ct-search / ct-tree)

| Option     | Argument | Meaning                                                              |
| ---------- | -------- | -------------------------------------------------------------------- |
| `--base`   | `PATH`   | A file outlines just that file; a directory is descended. Default `.`. |
| `--name`   | `PATTERN`| Limit to files whose name matches; `'|'`-separated alternatives, promoted and anchored. |
| `--ext`    | `LIST`   | Restrict to extensions (comma-separated, no dots); added to `--name` as alternatives. |
| `--hidden` | —        | Include dot-entries; default skips them.                              |
| `--follow` | —        | Follow symlinks while traversing.                                     |
| `--no-ignore` | —     | Walk gitignored / `.ignore` files too (`.git` is always skipped). Default: the walk skips what git would. |

Files whose language is not recognised (see *Languages*) are skipped silently
in a directory walk, and reported as an error (`exit 2`) when named directly
as `--base`.

## Selecting entries

| Option    | Argument  | Meaning                                                              |
| --------- | --------- | -------------------------------------------------------------------- |
| `--match` | `PATTERN` | Keep entries whose **name** matches. Substring→glob→regex promoted and **anchored to the whole name**, exactly like `--name` — so `--expect` counts stay predictable. Want prefix semantics? Say so: `--match 'Verdict*'`. |
| `--mode`  | `literal\|glob\|regex` | Pin how `--match`/`--name` are interpreted — promotion **off**. |
| `--kind`  | `LIST`    | Keep entries of these kinds (comma-separated), e.g. `--kind fn,struct`. Kinds are per-language (see below). |
| `--depth` | `N`       | Keep entries nested at most `N` levels deep (`1` = top-level only).   |

Filters compose by AND. In tree output, a matched entry keeps its ancestors
visible for context, marked `(context)` — but **only matched entries count**
toward `{COUNT}` and `--expect`, and only matched entries appear in `--flat`
and `--json` output, so all three output modes agree on the count.

## Output

Default (tree): entries grouped by file, indented by nesting depth, one entry
per line; context-only ancestors are marked:

```
src/patch.rs
  133:209   impl    PatchDoc      (context)
    140:155 fn      apply_one
  214:?     macro   declare_ops
  538:606   fn      apply_doc
```

| Option    | Effect                                                              |
| --------- | ------------------------------------------------------------------- |
| `--flat`  | One grep-friendly row per matched entry: `path:start:end:kind:name` (`end` is `?` when unknown). |
| `--quiet` | Print nothing; report via exit status (and `--emit`, which still fires). |
| `--json`  | Structured result (below); overrides the text modes and `--emit`.   |
| `--json-pretty` | Like `--json`, but pretty-printed (indented).                 |

Spans are `start:end` (1-based, inclusive). The start line is exact; `?` marks
an end the heuristic could not derive.

### JSON result

```json
{
  "tool": "ct-outline",
  "verdict": "SUCCESS",
  "base": "src",
  "count": 2,
  "files": [
    { "path": "src/patch.rs",
      "entries": [
        { "kind": "macro", "name": "declare_ops", "start": 214, "end": null, "depth": 1 },
        { "kind": "fn", "name": "apply_doc", "start": 538, "end": 606, "depth": 1 }
      ] }
  ]
}
```

## Framing as a test

| Option       | Argument | Meaning                                                          |
| ------------ | -------- | ----------------------------------------------------------------- |
| `--question` | `TEXT`   | The question this outline answers; printed as a `== … ==` banner. |
| `--expect`   | `SPEC`   | Verdict over the matched-entry count: `any|none|N|=N|+N|-N` (default `any`). |
| `--emit`     | `TEMPLATE` | Written to stdout after the outline (alias `--emit-stdout`). Tokens: `{RESULT}` `{QUESTION}` `{COUNT}` `{BASE}` `{MATCHES}` (newline-joined `path:start:end:kind:name` rows). |
| `--emit-stderr` | `TEMPLATE` | Same tokens, written to stderr.                              |

## Run bounds and liveness

Every suite tool is bounded and observable the same way:

| Option             | Argument   | Effect                                                            |
| ------------------ | ---------- | ----------------------------------------------------------------- |
| `--timeout`        | `SECS`     | Abort the run (exit `2`, with a one-line message) if it exceeds SECS seconds (fractional allowed). |
| `--heartbeat`      | `SECS`     | Print a liveness pulse every SECS seconds while the run is in progress. |
| `--heartbeat-emit` | `TEMPLATE` | Pulse template. Tokens: `{ELAPSED}` (whole seconds so far) `{TOOL}`. Default: `[{ELAPSED}s]`. |
| `--heartbeat-to`   | `stderr\|stdout` | Stream for pulses. Default: `stderr`.                       |

## Languages

Rule packs are keyed by file extension. Coverage is deliberately honest over
broad — each pack ships with its own test corpus, and the three launch packs
exercise all three block heuristics:

| Language  | Extensions | Kinds reported                                        | Status  |
| --------- | ---------- | ----------------------------------------------------- | ------- |
| Rust      | `rs`       | `mod` `struct` `enum` `trait` `impl` `fn` `macro` `type` `const` `static` | shipped |
| Python    | `py`       | `class` `def` (incl. `async def`)                     | shipped |
| Markdown  | `md`       | `h1`…`h6` (headings as the document's outline; fenced code blocks are ignored) | shipped |
| JS / TS   | `js` `jsx` `ts` `tsx` | `function` `class` `interface` `type` `enum` `const-fn` — **named/bound forms only**; anonymous callbacks and inline handlers are code flow, not structure, and never outline | planned |
| Java      | `java`     | `class` `interface` `enum` `record` `method`          | planned |
| Go        | `go`       | `func` `type` `const` `var` (top-level)               | planned |
| Shell     | `sh` `bash`| `function`                                            | planned |

Unrecognised extensions are skipped in walks. The kind vocabulary is the
source language's own keywords, so `--kind` reads naturally per language and
an agent never has to learn a cross-language abstraction.

## Composing with the suite

`ct-outline` is read-only, so it is on the `ct-test` allowlist (and therefore
`ct-each`'s default gate):

```sh
# Locate, then read exactly the region — the bounded-read loop.
ct-outline --base src/verdict.rs --match Expect --flat
ct-view src/verdict.rs --range 97:156

# Assert structure: exactly one definition of Verdict in the crate.
ct-outline --base src --ext rs --match Verdict --kind enum --expect =1 \
  --question "Is Verdict defined exactly once?" --emit '{QUESTION} -> {RESULT}'

# Sweep: outline every Rust file's top level, one bounded run per item.
ct-search --base src --name '*.rs' | \
  ct-each --stdin -- ct-outline --base '{ITEM}' --depth 1 --flat
```

## Examples

- **List the functions in a file, then read one with ct view --range, instead of grepping for 'fn '.**
  ```sh
  ct outline --base src/steer.rs --kind fn
  ```
- **Find test functions across the tree as grep-friendly path:line rows.**
  ```sh
  ct outline --base src --ext rs --match 'test_*' --flat
  ```

## Exit status

| Code | Meaning                                                    |
| ---- | ---------------------------------------------------------- |
| `0`  | verdict `SUCCESS` (the expectation over matched entries was met) |
| `1`  | verdict `ERROR` (clean negative: the expectation was not met) |
| `2`  | usage or runtime error — bad options, an unreadable file, or an unrecognised language named directly |

### Documentation

| Option                 | Effect                                                           |
| ---------------------- | ---------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                      |
| `-V`, `--version`      | Version.                                                         |

## OKF awareness

`--frontmatter` makes a Markdown concept's YAML frontmatter visible in the
outline: each field becomes a synthetic `meta:KEY` entry (`meta:type`,
`meta:title`, `meta:description`, `meta:resource`, `meta:timestamp`, `meta:tags`)
prepended before the document's headings. They behave like any other entry — count
toward `--expect`, appear in `--flat`/`--json`, and can be selected with
`--kind meta:type`. The flag is off by default, so ordinary outlines are
unchanged.

```sh
# List just the concept types across a bundle.
ct-outline --base bundle --frontmatter --kind meta:type --flat
```
