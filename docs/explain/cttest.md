# cttest — Framed Experiment

> Run a command as a labelled experiment: pose a question, classify the result
> from stdout/stderr matches, and emit a tidy, templated verdict.

Sometimes a user or agent wants to run a test that posits or checks an
assumption, but doing it coherently means framing the output with surrounding
context. `cttest` turns that ad-hoc ritual into one declarative command —
pass/fail is decided by **what the command prints**, not only its exit code, so
a tool that exits `0` while printing `ERROR:` can still be classified as a
failure.

This document is the canonical reference for `cttest`. It is also what the tool
prints for `cttest --explain` (`--explain md`); `cttest --explain json` prints
the equivalent MCP / tool-use definition.

## Replaces patterns like

```sh
# what are we testing?
echo "== Does the parser accept full precision notation? =="

# how we test it
echo "2234323.234:f64" | ./parse | grep -v 'ERROR:'
echo "result: $?"
```

with:

```sh
cttest --question "Does the parser accept full precision notation?" \
  --cmd ./parse --stdin '2234323.234:f64' \
  --err-match 'ERROR:' \
  --emit 'result: {RESULT}'
```

## When to use it

- Record a test's intent (the *question*) alongside its outcome.
- Decide pass/fail from **output content** via match predicates, not just exit code.
- Produce a single, predictable line (`{RESULT}`) an agent or a `&&` chain can act on.

## Invocation

| Option       | Argument | Meaning                                                                 |
| ------------ | -------- | ----------------------------------------------------------------------- |
| `--question` | `TEXT`   | The question this experiment answers; printed as a `== … ==` banner.    |
| `--cmd`      | `PROG`   | Program to run. Trailing `-- ARGS…` are passed through to it.            |
| `--shell`    | —        | Interpret `--cmd` as a shell line via `sh -c` (enables pipes, redirection). |
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
`--err-match-stderr`; likewise `--ok-match`.

`{RESULT}` resolves in this order:

1. **Any** `err-match` hits → `ERROR`.
2. Otherwise, if any `ok-match` was supplied → `SUCCESS` if one hits, else `ERROR`.
3. Otherwise → `SUCCESS` if the child exited `0`, else `ERROR`.

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

| Option          | Argument   | Effect                                               |
| --------------- | ---------- | ---------------------------------------------------- |
| `--emit`        | `TEMPLATE` | Write the expanded template to **stdout** (alias `--emit-stdout`). |
| `--emit-stderr` | `TEMPLATE` | Write the expanded template to **stderr**.           |
| `--show-output` | —          | Also pass the child's stdout/stderr through verbatim. |
| `--quiet`       | —          | Suppress the `== question ==` banner.                |

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
* All `cttest` matchers are searched **unanchored** (anywhere in the stream).

Examples: `ERROR:` → literal; `WARN*` → glob; `^FATAL`, `ok|done`, `\d+ errors`
→ regex.

## Exit status

Tied to the verdict, so the experiment itself composes (`cttest … && echo confirmed`):

| Code | Meaning                                                    |
| ---- | ---------------------------------------------------------- |
| `0`  | `{RESULT}` is `SUCCESS`                                    |
| `1`  | `{RESULT}` is `ERROR`                                      |
| `2`  | usage or runtime error (e.g. the command could not launch) |

## Examples

```sh
# Pass/fail driven by output, not exit code: parser prints ERROR: but exits 0.
cttest --question "Does the parser accept full-precision notation?" \
  --cmd ./parse --stdin '2234323.234:f64' \
  --err-match 'ERROR:' \
  --emit 'result: {RESULT}'

# Require a positive signal in stdout; show what the command produced.
cttest --question "Does the migration report success?" \
  --shell --cmd 'run-migration --dry-run | tee /tmp/out' \
  --ok-match-stdout 'migration complete' \
  --show-output \
  --emit '{QUESTION} -> {RESULT} (exit {CODE})'

# Hand an agent the machine-readable tool definition.
cttest --explain json
```
