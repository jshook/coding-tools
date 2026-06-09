# ct-test — Framed Experiment

> Run a command as a labelled experiment: pose a question, classify the result
> from stdout/stderr matches, and emit a tidy, templated verdict.

Sometimes a user or agent wants to run a test that posits or checks an
assumption, but doing it coherently means framing the output with surrounding
context. `ct-test` turns that ad-hoc ritual into one declarative command —
pass/fail is decided by **what the command prints**, not only its exit code, so
a tool that exits `0` while printing `ERROR:` can still be classified as a
failure.

This document is the canonical reference for `ct-test`. It is also what the tool
prints for `ct-test --explain` (`--explain md`); `ct-test --explain json` prints
the equivalent MCP / tool-use definition.

## Replaces patterns like

```sh
# what are we checking?
echo "== Is the config free of deprecated keys? =="

# how we check it — pass/fail from content, not exit code
cat config.toml | grep -q 'old_key' && echo "result: ERROR" || echo "result: SUCCESS"
```

with:

```sh
ct-test --question "Is the config free of deprecated keys?" \
  --cmd cat -- config.toml \
  --err-match 'old_key' \
  --emit 'result: {RESULT}'
```

(`cat` exits `0`, but the `--err-match` makes the verdict `ERROR` if the key
appears — pass/fail decided by **what the command prints**.)

## When to use it

- Record a test's intent (the *question*) alongside its outcome.
- Decide pass/fail from **output content** via match predicates, not just exit code.
- Produce a single, predictable line (`{RESULT}`) an agent or a `&&` chain can act on.

`ct-test` shares its verdict-and-emit model with `ct-search`: both pose a question,
classify a probe into a `SUCCESS`/`ERROR` verdict, and emit a templated line, and
both tie exit status to that verdict. `ct-test`'s probe is a command; `ct-search`'s
is a search (see `ct-search --explain`, especially `--expect`/`--emit`).

## Command allowlist

Because `ct-test` runs an arbitrary program, it runs **only** commands on a fixed,
compiled-in list of read-only commands:

```
cat ct-search ct-tree ct-view echo false file grep head ls pwd stat tail true wc
```

The suite's read-only `ct-search`/`ct-tree`/`ct-view` are included — so `ct-test` is
a ready **conditional wrapper** around them (see *Composing with the suite*); the
umbrella `ct` and the mutating `ct-test`/`ct-edit`/`ct-patch` are not, since they
can change state. **The list is static and immutable** — there is deliberately no
flag or file to extend it, so an agent driving `ct-test` cannot grant itself new
commands.

Gating is by **program name** — the file-name component of `--cmd` (so `ls`,
`/bin/ls`, and `./ls` all gate on `ls`), or `sh` under `--shell`. Since `sh` is not
on the list, `--shell` command lines are not currently runnable. It guards against
unintended side effects; it is not a sandbox and does not inspect arguments.

A command that is not on the list is **refused** (exit `2`, nothing is run), and
`ct-test` prints the full set of permitted commands.

## Composing with the suite

The read-only `ct-*` tools are on the allowlist, so `ct-test` is a ready
**conditional wrapper** around them — pose a question, run a suite tool, decide
from its output or exit status. Because every tool shares the same exit contract
(`0` found / `1` clean-negative / `2` error), the wrap usually needs nothing more
than the command:

```sh
# True when any .rs file exceeds 5000 lines (ct-tree exits 0 when it lists any).
ct-test --question "Any huge Rust files?" \
  --cmd ct-tree -- --base src --ext rs --min-lines 5001 --flat

# Or decide from the tool's stdout content:
ct-test --question "Is ct-patch the largest?" --ok-match-stdout 'ct-patch.rs' \
  --cmd ct-tree -- --base src --ext rs --flat --sort lines --desc
```

`ct-test` resolves a bare `ct-*` command to a sibling of its own executable before
falling back to `PATH`, exactly as the `ct` umbrella does, so wrapping works the
same whether the suite is installed or run from a build directory. (Put `ct-test`'s
own options **before** the `--`; everything after `--` goes to the wrapped tool.)

## Invocation

| Option       | Argument | Meaning                                                                 |
| ------------ | -------- | ----------------------------------------------------------------------- |
| `--question` | `TEXT`   | The question this experiment answers; printed as a `== … ==` banner.    |
| `--cmd`      | `PROG`   | Program to run (must be on the allowlist). Trailing `-- ARGS…` are passed through to it. |
| `--shell`    | —        | Interpret `--cmd` as a shell line via `sh -c`. Gated on `sh`, which is not on the allowlist, so this is currently unavailable. |
| `--stdin`    | `TEXT`   | Literal text written to the child's standard input.                     |

## Classifying the result

Each pattern below is promoted (see *Pattern matching*) and searched
**unanchored** against the captured stream(s).

| Option                 | Hits when the pattern is found in… | Implies   |
| ---------------------- | ---------------------------------- | --------- |
| `--err-match`          | stdout **or** stderr               | `ERROR`   |
| `--err-match-stdout`   | stdout                             | `ERROR`   |
| `--err-match-stderr`   | stderr                             | `ERROR`   |
| `--ok-match`           | stdout **or** stderr               | `SUCCESS` |
| `--ok-match-stdout`    | stdout                             | `SUCCESS` |
| `--ok-match-stderr`    | stderr                             | `SUCCESS` |

`--err-match` is exactly a synonym for supplying both `--err-match-stdout` and
`--err-match-stderr`; likewise `--ok-match`. The `-stdout`/`-stderr` variants
search **only that one stream** — important for tools that split results from
progress (e.g. `cargo test` writes `test result: ok` to **stdout**, while build
errors go to **stderr**, so `--ok-match-stderr 'test result: ok'` would never
match; use `--ok-match` to search both).

`--otherwise <success|error|exit>` sets the verdict for an *inconclusive* run —
when neither an `--ok-match` nor an `--err-match` matched (see below).

### Verdict

`ct-test` is **fail-closed**: it reports `SUCCESS` only when success is positively
established. `{RESULT}` resolves in this order:

1. **Any** `--err-match*` hits → `ERROR`. *(A failure signal is decisive and is
   never overridden — not by an exit code, not by `--otherwise`.)*
2. Else **any** `--ok-match*` hits → `SUCCESS`. *(A supplied `--ok-match` is a
   **required** proof of success: a clean `exit 0` does **not** substitute for it.)*
3. Else the run is **inconclusive** (no assertion fired) → the `--otherwise`
   policy decides:

   | `--otherwise` | Inconclusive verdict                  |
   | ------------- | ------------------------------------- |
   | `success`     | `SUCCESS`                             |
   | `error`       | `ERROR`                               |
   | `exit`        | `SUCCESS` if the child exited `0`, else `ERROR` |

   **Default** (no `--otherwise`): `error` when an `--ok-match` was supplied (the
   proof you required did not appear), otherwise `exit`. This keeps the
   conservative behaviour while letting a caller opt into, say, `--otherwise exit`
   to accept a clean exit when the success marker is on a stream you did not check.

On `ERROR`, `ct-test` prints a one-line **reason** to stderr (e.g.
`ct-test: --ok-match-stderr 'test result: ok' not found in stderr; exit=0`), so a
red verdict is never unexplained. The same text is available as the `{REASON}`
emit token.

## Reporting the result

Emit templates are printed **after** the command finishes. Tokens substituted:

| Token        | Expands to                                          |
| ------------ | --------------------------------------------------- |
| `{RESULT}`   | `SUCCESS` or `ERROR`                                |
| `{CODE}`     | the child's exit code (or `signal:N`)               |
| `{QUESTION}` | the `--question` text                               |
| `{CMD}`      | the command line that was run                       |
| `{STDOUT}`   | captured standard output (trailing newline trimmed) |
| `{STDERR}`   | captured standard error (trailing newline trimmed)  |
| `{REASON}`   | one-line explanation of the verdict (which rule fired) |
| `{FOCUS}`    | the `--focus` distilled slice (empty without `--focus`) |

| Option          | Argument   | Effect                                               |
| --------------- | ---------- | ---------------------------------------------------- |
| `--emit`        | `TEMPLATE` | Write the expanded template to **stdout** (alias `--emit-stdout`). |
| `--emit-stderr` | `TEMPLATE` | Write the expanded template to **stderr**.           |
| `--show-output` | —          | Also pass the child's stdout/stderr through verbatim. |
| `--focus`       | `PATTERN`  | Distil the captured output to the lines matching `PATTERN`, with `--context` lines around each (overlapping windows merge, separated by `--`, line-numbered). Printed to **stderr** and available as `{FOCUS}`. |
| `--context`     | `N`        | Lines of context around each `--focus` match. Default `2`. |
| `--quiet`       | —          | Suppress the `== question ==` banner.                |

`--focus` turns a noisy command into just the lines that matter — e.g. run a build
or test and `--focus 'error\[|FAILED'` to see only the failures with surrounding
context, instead of scrolling the whole log.

### Documentation

| Option                 | Effect                                                           |
| ---------------------- | --------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                     |
| `-V`, `--version`      | Version.                                                        |

## Pattern matching

Every match pattern is promoted to a regular expression with one predictable
rule — write the simplest thing that expresses your intent:

| The pattern contains…                                  | …it is treated as | Match semantics                  |
| ------------------------------------------------------ | ----------------- | -------------------------------- |
| no metacharacters at all                               | literal substring | matched verbatim (regex-escaped) |
| glob metacharacters only, and is **not** a valid regex | glob              | converted to an equivalent regex |
| regex metacharacters, and **is** a valid regex         | regex             | used exactly as written          |

* **Glob metacharacters:** `*` `?` `[ … ]`
* **Regex metacharacters:** `^ $ ( ) | + { } \ .`
* All `ct-test` matchers are searched **unanchored** (anywhere in the stream).

Examples: `ERROR:` → literal; `WARN*` → glob; `^FATAL`, `ok|done`, `\d+ errors`
→ regex.

## Exit status

Tied to the verdict, so the experiment itself composes (`ct-test … && echo confirmed`):

| Code | Meaning                                                    |
| ---- | ---------------------------------------------------------- |
| `0`  | `{RESULT}` is `SUCCESS`                                    |
| `1`  | `{RESULT}` is `ERROR`                                      |
| `2`  | usage or runtime error — bad options, the command could not launch, or it was refused by the allowlist |

## Examples

```sh
# Pass/fail from content, not exit code: cat exits 0 but the verdict is ERROR
# if the file still mentions a forbidden token.
ct-test --question "Is the config free of deprecated keys?" \
  --cmd cat -- config.toml \
  --err-match 'old_key' \
  --emit 'result: {RESULT}'

# Require a positive signal in the command's output.
ct-test --question "Does the changelog mention v2?" \
  --cmd grep -- -F v2 CHANGELOG.md \
  --ok-match 'v2' \
  --emit '{QUESTION} -> {RESULT}'

# Frame a read-only suite tool as a check (ct-search is on the allowlist).
ct-test --question "Is there a Cargo.toml at the root?" \
  --cmd ct-search -- --name Cargo.toml --limit 1 --quiet

# Hand an agent the machine-readable tool definition.
ct-test --explain json
```
