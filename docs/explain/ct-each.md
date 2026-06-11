# ct-each — Per-Item Dispatch

> Run one command template once per item — no shell, no loop syntax — classify
> each run by exit status, and judge the whole sweep with an aggregate
> `--expect` verdict.

Sometimes a user or agent wants to perform the same action over a set of
distinct names — files, symbols, modules — and the traditional answer is a
`for` loop in `bash`, with all its quoting hazards and invisible failure
handling. `ct-each` turns that ritual into one declarative command: items go
in, `{ITEM}` expands **inside argv elements** that are passed directly to the
program (never re-parsed by a shell), and every run's outcome is collected
into a single framed verdict.

This document is the canonical reference for `ct-each`. It is also what the
tool prints for `ct-each --explain` (`--explain md`); `ct-each --explain json`
prints the equivalent MCP / tool-use definition.

## Replaces patterns like

```sh
# quoting hazards, silent failures, no aggregate verdict
for f in Parser Lexer Emitter; do
  ct-search --base src --grep "${f}::new" --quiet || echo "missing: $f"
done
```

with:

```sh
ct-each --items Parser Lexer Emitter -- \
  ct-search --base src --grep '{ITEM}::new' --quiet
```

(Each item becomes one direct `ct-search` invocation; the default
`--expect all` makes the sweep fail loudly if any item fails.)

## When to use it

- Apply one read-only check, or one gated edit, across a list of names.
- Get a per-item `SUCCESS`/`ERROR` line and one aggregate verdict instead of
  scrolling loop output.
- Avoid `bash` for-loops entirely: items are substituted into argv, so spaces
  and metacharacters in an item can never re-shape the command.

`ct-each` shares the suite's verdict-and-emit model: it poses a `--question`,
classifies a probe (here: a sweep of runs) into a `SUCCESS`/`ERROR` verdict,
and emits templated lines; exit status follows the verdict.

## Items and substitution

Items come from three combinable sources, in this order:

1. `--items A B C` — explicit items, repeatable;
2. the **walker**: `--base PATH` (with the suite's shared `--name`/`--ext`/
   `--hidden`/`--follow` selectors; `--name`/`--ext` alone imply `--base .`)
   turns every matched file path into an item, in walk order — the natural
   source for per-file sweeps ("every `src/**/*.rs` must …");
3. `--stdin` — one item per line, blank lines skipped.

Order is preserved and one run happens per item.

Everything after `--` is the command template. In **every** element, two
tokens expand:

| Token     | Expands to                      |
| --------- | ------------------------------- |
| `{ITEM}`  | the current item, verbatim      |
| `{INDEX}` | the item's 1-based position     |

Substitution happens per argv element and the result is launched directly —
there is **no shell anywhere**, so no word-splitting, globbing, or quoting
rules apply to item values.

`--dry-run` prints each fully-expanded command (`would run: …`) and runs
nothing — preview the sweep before committing to it.

## Command gate

Dispatch targets are gated by **program name** (the file-name component of the
expanded command), against a fixed, compiled-in set:

- **Default:** the read-only allowlist (`cat ct-check ct-deps ct-outline ct-search ct-tree ct-view echo
  false file grep head ls pwd stat tail true wc`) plus `ct-test` — which is
  itself gated to read-only commands, so dispatching it stays read-only.
- **With `--mutating`:** additionally `ct-edit` and `ct-patch` — the suite's
  own mutating tools, each of which still enforces its own `--expect` /
  `--dry-run` gates per invocation.

Nothing else is ever runnable: the set is **static and immutable**, there is
no flag or file to extend it, and there is no shell mode. The whole sweep is
gated **up front**: every item's expanded command is checked before the first
one runs, so a refusal (exit `2`, nothing run) can never strike mid-sweep.
Like the rest of the suite, this is a guard against unintended side effects,
not a sandbox.

`ct-each` resolves a bare `ct-*` command to a sibling of its own executable
before falling back to `PATH`, exactly as the `ct` umbrella does.

## Classifying the sweep

Each item's run is classified by the suite's exit contract: exit `0` ⇒ that
item is `SUCCESS`; anything else (including a `--timeout` kill) ⇒ `ERROR`.
The aggregate verdict judges the **count of per-item successes** against
`--expect`:

| Spec   | Passes when the SUCCESS count is | Meaning                         |
| ------ | -------------------------------- | ------------------------------- |
| `all`  | `== total`                       | every item passed *(default)*   |
| `any`  | `>= 1`                           | at least one passed             |
| `none` | `== 0`                           | a negative assertion            |
| `N`    | `>= N`                           | at least `N`                    |
| `=N`   | `== N`                           | exactly `N`                     |
| `+N`   | `> N`                            | more than `N`                   |
| `-N`   | `< N`                            | fewer than `N`                  |

`--fail-fast` stops the sweep after the first per-item `ERROR`; unrun items
are reported as *skipped* and still count against `--expect all` (a skipped
item is not a success).

On a per-item `ERROR`, a one-line reason (`ct-each: [2/5] 'Lexer' -> ERROR
(exit=1)`) goes to stderr, so a red item is never unexplained; re-run with
`--show-output` to see the children's streams verbatim. On an aggregate
`ERROR`, the summary reason does the same for the sweep.

## Run bounds and liveness

| Option             | Argument   | Effect                                                            |
| ------------------ | ---------- | ----------------------------------------------------------------- |
| `--timeout`        | `SECS`     | **Per item:** kill that run's process group after SECS seconds (fractional allowed); the item is `ERROR` and its `{CODE}` is `timeout`. |
| `--heartbeat`      | `SECS`     | Print a liveness pulse every SECS seconds while the sweep runs.   |
| `--heartbeat-emit` | `TEMPLATE` | Pulse template. Tokens: `{ELAPSED}` (whole seconds so far) `{TOOL}` `{QUESTION}` `{ITEM}` `{INDEX}` `{DONE}` `{TOTAL}`. Default: `[{ELAPSED}s]`. |
| `--heartbeat-to`   | `stderr\|stdout` | Stream for pulses. Default: `stderr`.                       |

The heartbeat is minimal by default — one `[12s]` line per interval — and
stops before the summary is printed, so a pulse never lands after the verdict.

## Invocation

| Option       | Argument | Meaning                                                              |
| ------------ | -------- | -------------------------------------------------------------------- |
| `--items`    | `ITEM…`  | Items to dispatch over, in order (one run per item).                 |
| `--base`     | `PATH`   | Walker item source: matched file paths become items (a file yields itself; a directory is descended). |
| `--name`     | `PATTERN`| Walker filter: file-name alternatives, promoted and anchored. Implies `--base .` when `--base` is absent. |
| `--ext`      | `LIST`   | Walker filter: extensions (comma-separated, no dots); combined with `--name` as alternatives. |
| `--hidden`   | —        | Walker: include dot-entries.                                          |
| `--follow`   | —        | Walker: follow symlinks.                                              |
| `--stdin`    | —        | Also read items from standard input, one per line.                   |
| `--question` | `TEXT`   | The question this sweep answers; printed as a `== … ==` banner.      |
| `--expect`   | `SPEC`   | Aggregate expectation over the SUCCESS count (default `all`).        |
| `--fail-fast`| —        | Stop after the first per-item `ERROR`.                               |
| `--mutating` | —        | Permit `ct-edit` / `ct-patch` as the dispatch target.                |
| `--dry-run`  | —        | Print each expanded command; run nothing.                            |

## Reporting the result

Per item (stdout): the default line is `{RESULT} {ITEM}` (suppressed by
`--quiet`); `--emit-each TEMPLATE` replaces it. Tokens: `{RESULT}` `{ITEM}`
`{INDEX}` `{CODE}` `{CMD}` `{STDOUT}` `{STDERR}`.

After the sweep (stdout): the default summary is `OK/TOTAL item(s) succeeded
-> VERDICT` (suppressed by `--quiet` or replaced by `--emit`). Summary tokens
for `--emit` / `--emit-stderr`: `{RESULT}` `{OK}` `{ERRORS}` `{SKIPPED}`
`{TOTAL}` `{QUESTION}` `{EXPECT}` `{REASON}`.

| Option          | Argument   | Effect                                              |
| --------------- | ---------- | --------------------------------------------------- |
| `--emit-each`   | `TEMPLATE` | Per-item line written to stdout.                    |
| `--emit`        | `TEMPLATE` | Summary written to stdout (alias `--emit-stdout`).  |
| `--emit-stderr` | `TEMPLATE` | Summary written to stderr.                          |
| `--show-output` | —          | Also pass each child's stdout/stderr through verbatim. |
| `--quiet`       | —          | Suppress the banner, default per-item lines, and default summary. |
| `--json`        | —          | One structured JSON object instead of text (overrides the emit templates). |

The `--json` result carries `tool`, `verdict`, `expect`, `ok`, `errors`,
`skipped`, `total`, and an `items` array of `{index, item, cmd, code, result}`.

### Documentation

| Option                 | Effect                                                           |
| ---------------------- | ---------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                      |
| `-V`, `--version`      | Version.                                                         |

## Exit status

Tied to the aggregate verdict, so a sweep composes (`ct-each … && next-step`):

| Code | Meaning                                                    |
| ---- | ---------------------------------------------------------- |
| `0`  | aggregate verdict `SUCCESS` (or `--dry-run` printed its plan) |
| `1`  | aggregate verdict `ERROR`                                  |
| `2`  | usage or runtime error — bad options, no items, a launch failure, or a refused dispatch target |

## Examples

```sh
# Assert every listed type is still referenced somewhere under src/.
ct-each --question "Are all core types still referenced?" \
  --items Parser Lexer Emitter -- \
  ct-search --base src --grep '{ITEM}' --quiet

# Frame each item as its own experiment by dispatching ct-test.
ct-each --items config.toml settings.toml -- \
  ct-test --quiet --cmd cat -- '{ITEM}' --err-match 'old_key'

# Preview a per-item gated rename, then apply it (--mutating unlocks ct-edit;
# each ct-edit still enforces its own --expect =1 before writing).
ct-each --items old_alpha old_beta --dry-run --mutating -- \
  ct-edit --base src --name '*.rs' --find '{ITEM}(' --replace 'renamed_{ITEM}(' --expect =1
ct-each --items old_alpha old_beta --mutating -- \
  ct-edit --base src --name '*.rs' --find '{ITEM}(' --replace 'renamed_{ITEM}(' --expect =1

# Per-file invariant via the walker source: every Rust file carries the header.
ct-each --base src --name '*.rs' -- \
  ct-search --base '{ITEM}' --grep 'SPDX-License-Identifier' --quiet

# Or pipe a file list from anywhere; bound and observe each run.
ct-search --base src --name '*.rs' | \
  ct-each --stdin --timeout 10 --heartbeat 5 -- ct-view '{ITEM}' --match TODO --context 1

# Hand an agent the machine-readable tool definition.
ct-each --explain json
```
