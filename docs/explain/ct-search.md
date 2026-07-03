# ct-search — Coding Tools Search

> Recursively find files by name, type, size, and content from a chosen root.
> One declarative command in place of a `find … | xargs grep …` pipeline.

`ct-search` combines the predicates you normally assemble from `find`, `xargs`,
and `grep`. You state *what* you are looking for; the tool handles the
traversal, the per-file work, and the reporting. An entry matches only when
**all** supplied predicates hold. Output defaults to a list of matching paths;
the exit status reports whether anything matched.

This document is the canonical reference for `ct-search`. It is also what the tool
prints for `ct-search --explain` (`--explain md`); `ct-search --explain json`
prints the equivalent MCP / tool-use definition.

## When to use it

- Search a tree that is **not** the current directory, without `cd`-ing first (`--base`).
- Combine "name looks like X" **and** "contents contain Y" **and** "bigger than Z" in one pass.
- Ask only *"does anything match?"* (`--quiet` + exit code) or *"how many?"* (`--summary`).
- **Pose the search as a pass/fail test** — frame it with a `--question`, set an
  `--expect`ation over the match count (so "there must be **no** matches" passes
  when nothing is found), and `--emit` a templated verdict. This is the same
  framed-verdict model `ct-test` uses, so a search and a command-experiment read
  and compose the same way.

## Replaces patterns like

```sh
cd somedir \
  && find . -type f \( -name "*.java" -o -name "*.kt" \) \
  | xargs grep -l "SimpleMFD\|knn_entries\|DataSetLoaderSimple" 2>/dev/null \
  | head -20
```

with:

```sh
ct-search --base somedir \
  --type f \
  --name '*.java|*.kt' \
  --grep 'SimpleMFD|knn_entries|DataSetLoaderSimple' \
  --limit 20 \
  --list
```

## Options

### Predicates

An entry matches only when **all** supplied predicates hold.

| Option       | Argument  | Meaning                                                                                     |
| ------------ | --------- | ------------------------------------------------------------------------------------------- |
| `--base`     | `DIR`     | Search root, relative or absolute, regardless of the CWD at launch. Default: `.`            |
| `--name`     | `PATTERN` | Match the entry's file name. `\|`-separated alternatives; each is promoted (see *Pattern matching*) and anchored to the whole name. |
| `--type`     | `KINDS`   | Restrict to entry kinds: `f` (file), `d` (directory), `l` (symlink). Repeatable or comma-joined (`--type f,l`). |
| `--grep`     | `PATTERN` | Match file **contents** (promoted; searched unanchored). Implies regular files. Accepts `file:PATH` / `text:VALUE`; a multi-line payload matches as a line-anchored literal **block**. |
| `--mode`     | `literal\|glob\|regex` | Pin how patterns are interpreted — promotion **off** for every pattern in the invocation. State `literal` when the pattern is verbatim code. |
| `--size`     | `EXPR`    | Size predicate `[+\|-]N[k\|m\|g]`: `+N` larger than, `-N` smaller than, `N` at least N. Applies to regular files. |
| `--hidden`   | —         | Include dot-entries (names starting with `.`). Default: skipped, and dot-directories are not descended into. |
| `--follow`   | —         | Follow symlinks while traversing.                                                           |
| `--no-ignore` | —        | Walk gitignored / `.ignore` files too (`.git` is always skipped). Default: the walk skips what git would, so `target/` and the like are not descended. |
| `--limit`    | `N`       | Stop after `N` matches.                                                                      |

### Output mode

Mutually exclusive; defaults to `--list`.

| Option      | Output                                                              |
| ----------- | ------------------------------------------------------------------ |
| `--list`    | One matching path per line. *(default)*                            |
| `--summary` | Counts only — files matched, and with `--grep`, total matching lines. |
| `--detail`  | Matching paths plus, for `--grep`, each hit as `path:line:text`.   |
| `--quiet`   | No per-match output, and no `--question` banner; communicate via exit status (and `--emit`). |

`--json` overrides these text modes (and `--emit`), emitting a structured result `{tool, verdict, base, count, lines, matches}`. `--json-pretty` does the same, pretty-printed (indented).

### Framing the search as a test

These turn a search into a framed check whose verdict is `SUCCESS` or `ERROR`.
They are additive: with none of them, `ct-search` behaves exactly as before.

| Option          | Argument   | Meaning                                                                                  |
| --------------- | ---------- | ---------------------------------------------------------------------------------------- |
| `--question`    | `TEXT`     | The question this search answers; printed as a `== … ==` banner unless `--quiet`.        |
| `--expect`      | `SPEC`     | Verdict expectation over the match **count**. Default `any`. See *Expectations* below.   |
| `--emit`        | `TEMPLATE` | Template written to **stdout** after the search (alias `--emit-stdout`). Tokens below.   |
| `--emit-stderr` | `TEMPLATE` | Template written to **stderr** after the search (same tokens).                           |

#### Expectations

`--expect` classifies the match count into the verdict. Its numeric forms reuse
the same `[+|-]N` threshold grammar as `--size`:

| Spec   | Passes (`SUCCESS`) when the count is | Use                              |
| ------ | ------------------------------------ | -------------------------------- |
| `any`  | `>= 1`                               | found something *(the default)*  |
| `none` | `== 0`                               | a negative assertion             |
| `N`    | `>= N`                               | at least `N`                     |
| `=N`   | `== N`                               | exactly `N`                      |
| `+N`   | `> N`                                | more than `N`                    |
| `-N`   | `< N`                                | fewer than `N`                   |

Because the default is `any`, a plain search's verdict is `SUCCESS` exactly when
it matched — identical to the historic exit status. `--expect none` is the key
inversion: the test passes when the search finds **nothing**.

#### Emit tokens

| Token        | Expands to                                            |
| ------------ | ----------------------------------------------------- |
| `{RESULT}`   | `SUCCESS` or `ERROR` (the verdict)                    |
| `{QUESTION}` | the `--question` text                                 |
| `{COUNT}`    | number of entries that matched                        |
| `{LINES}`    | total matching lines (with `--grep`), else `0`        |
| `{BASE}`     | the search root (`--base`)                            |
| `{MATCHES}`  | the matched paths, newline-joined                     |

### Documentation

| Option                 | Effect                                                            |
| ---------------------- | ---------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                      |
| `-V`, `--version`      | Version.                                                         |

## Pattern matching

Every pattern argument (`--name`, `--grep`) is promoted to a regular expression
with one predictable rule — write the simplest thing that expresses your intent:

| The pattern contains…                                  | …it is treated as | Match semantics                          |
| ------------------------------------------------------ | ----------------- | ---------------------------------------- |
| no metacharacters at all                               | literal substring | matched verbatim (regex-escaped)         |
| glob metacharacters only, and is **not** a valid regex | glob              | converted to an equivalent regex         |
| regex metacharacters, and **is** a valid regex         | regex             | used exactly as written                  |

* **Glob metacharacters:** `*` `?` `[ … ]` — and `*`/`?` do not cross `/`.
* **Regex metacharacters:** `^ $ ( ) | + { } \ .`
* `--name` matches are **anchored to the whole name** (so `*.java` means "ends in
  `.java`"); `--grep` matches are **unanchored** (match anywhere).
* `--name` accepts `|`-separated alternatives (`*.java|*.kt`), matching any;
  `--grep` keeps `|` as ordinary regex alternation.

Examples: `ERROR:` → literal; `*.java` → glob (a leading `*` is not a valid
regex); `^ERROR`, `foo|bar`, `\d+` → regex.

`--mode literal|glob|regex` switches promotion **off** and pins the stated
interpretation — the right tool when the pattern is verbatim code whose `(`
`!` `?` would otherwise promote to a regex and miss its own text.

### Payload schemes and block matching

`--grep` is payload-typed: `file:PATH` reads the pattern verbatim from a
file (never promoted; literal by default), `text:VALUE` escapes a value
that genuinely begins with `file:`/`text:`; everything else is unaffected
(`http://…`, `std::fmt`). A **multi-line** pattern matches as a
line-anchored literal block: K lines match K consecutive source lines
byte-for-byte, whitespace significant. Each block occurrence counts as a
matching "line" at its start line; under `--detail`, a block that matched
nothing reports its **nearest miss** to stderr (best-aligned candidate and
the first diverging line).

```sh
# Does this exact arm group exist anywhere, and where?
ct search --base src --name '*.rs' --grep file:arm.block --detail

# After an edit: assert the new block is present exactly once.
ct search --base src --grep file:new.block --expect =1
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

The exit status follows the **verdict** = `--expect` applied to the match count:

| Code | Meaning                                          |
| ---- | ------------------------------------------------ |
| `0`  | verdict `SUCCESS` (the expectation was met)      |
| `1`  | verdict `ERROR` (the expectation was not met)    |
| `2`  | usage or runtime error                           |

With the default `any` expectation this reduces to the familiar "`0` if anything
matched, `1` if not", so existing pipelines are unaffected; `--expect none`
inverts it (a search that finds nothing is `0`). The `0`/`1` split lets you chain
`ct-search` in `&&`/`||` pipelines without parsing output; a distinct `2` keeps
real errors from looking like a clean verdict.

## Examples

```sh
# Any Rust file under ./src mentioning "TODO" — just tell me yes/no.
ct-search --base src --type f --name '*.rs' --grep TODO --quiet

# Count config files larger than 4 KiB anywhere under the repo.
ct-search --name '*.toml|*.yaml|*.json' --size +4k --summary

# Detailed grep-style report, capped at 20 hits, across Java/Kotlin.
ct-search --name '*.java|*.kt' --grep 'load(Simple|Bulk)Data' --detail --limit 20

# Search as a test: assert there are NO leftover debug prints under ./src.
# Passes (exit 0) only when the search finds nothing.
ct-search --base src --type f --name '*.rs' --grep 'dbg!\(' \
  --question "Are all debug prints removed from src?" \
  --expect none \
  --emit '{QUESTION} -> {RESULT} ({COUNT} stray in {BASE})'

# Search as a test: assert the migration emitted at least one marker file.
ct-search --base out --name 'migrated-*.json' --expect +0 \
  --question "Did the migration emit markers?" --emit '{RESULT}: {COUNT} markers'

# Hand an agent the machine-readable tool definition.
ct-search --explain json
```

- **Find every TODO/FIXME in Rust sources under src/ in one call, instead of grep -rn -E 'TODO|FIXME' --include='*.rs' src.**
  ```sh
  ct search --grep 'TODO|FIXME' --name '*.rs' --base src
  ```
- **List Rust file paths under src/, instead of find src -name '*.rs' -type f.**
  ```sh
  ct search --name '*.rs' --type f --base src --list
  ```
- **Assert there are no panic! calls as a pass/fail gate (exit 1 if any exist), with no piping into wc or grep.**
  ```sh
  ct search --grep 'panic!' --name '*.rs' --quiet --expect none
  ```

## OKF awareness

When searching a knowledge bundle, two predicates filter Markdown concepts by
their frontmatter — both narrow the candidate set before any `--grep` runs, and a
file with no conforming frontmatter never matches them:

- `--okf-type PATTERN` — keep concepts whose frontmatter `type` matches (the
  usual substring→glob→regex promotion, pinned by `--mode`).
- `--okf-tag TAG` — keep concepts carrying *all* the given tags (repeatable or
  comma-joined).

```sh
# Every PII-tagged BigQuery table mentioning "ssn".
ct-search --base bundle --okf-type 'BigQuery Table' --okf-tag pii --grep ssn
```
