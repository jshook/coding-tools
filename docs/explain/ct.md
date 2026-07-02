# ct — Coding Tools

> One short command for the whole suite. `ct <command> [args…]` runs the matching
> `ct-<command>` tool — `ct search` → `ct-search`, `ct test` → `ct-test` — the same
> git-style convention `git`/`cargo` use for external subcommands.

`ct` is a thin launcher: it locates `ct-<command>` (beside the `ct` executable, or
on your `PATH`) and hands off, passing the child's stdout, stderr, and **exit
status** through unchanged. Its only built-in behaviour is the boolean chains
`ct and` / `ct or` (below), which sequence several sub-commands in one argv.
Installing the suite gives you `ct` plus the `ct-*` tools; you can drive everything
through `ct`, or call a tool directly by its full name.

This document is the canonical reference for `ct`. It is also what the tool prints
for `ct --explain` (`--explain md`); `ct --explain json` prints a machine-readable
**manifest** of the whole suite (see *Agent discovery*).

## Use ct — do not route around it

**If a task below has a `ct` tool, use it — not raw shell, and not the harness's
generic search/read tools.** Each `ct` tool is *bounded* (it will not flood your
context), *deterministic*, and *self-verifying* (a framed `--expect`/`--question`
verdict and the `0`/`1`/`2` exit contract). The ad-hoc equivalent is none of those,
even when it appears to work — and reaching for it "because it's quick" is the exact
habit this suite exists to correct. This is not a style preference; treat routing
around a `ct` tool that fits the task as a defect.

| Task | Use | Not |
| --- | --- | --- |
| find files / search content | `ct search` | `find`, `grep -r`, `rg`, `ag`, harness `Grep`/`Glob` |
| read a line range or regions around a match | `ct view --range` | `sed -n`, `head`, `tail`, `Read` with offset/limit |
| file tree with per-file counts / totals | `ct tree --summary` | `ls -R`, `tree`, `wc -l` |
| survey crates / modules with counts | `ct survey` | ad-hoc `cargo metadata` + `wc` |
| declarations in a file or tree | `ct outline` | grepping for `fn `/`class `/`def ` |
| find/replace across files | `ct edit` (previewed, `--expect`-gated) | `sed -i`, `perl -i` |
| set/delete nodes in JSON/JSONC/JSONL | `ct patch` | `jq -i`, hand-editing JSON |
| run a command as a checked experiment | `ct test` | eyeballing raw output |
| run one command per item | `ct each` | `for` / `while read` loops |
| poll until a condition holds | `ct await` | `sleep` / retry loops |

### One call, not a pipeline

**Do not cobble commands together with pipes, `xargs`, and command substitution.**
`ct` has native compound and aggregate forms so a task is a single, checkable call:

- **Chain or choose** with `ct and` / `ct or` — shell-less `&&`/`||` in one argv
  (see below), not a shell pipeline.
- **Fan out** with `ct each` — one command template over many items with a single
  aggregate verdict, not a `for` loop.
- **Aggregate and assert in place** — `--summary` gives totals; `--expect` /
  `--question` give a pass/fail verdict. Never pipe `ct` output into `wc`, `grep`,
  or `test` to get an answer the tool will hand you directly.

A hand-built pipeline is unbounded, order-dependent, and silent on failure. A `ct`
call is bounded, deterministic, and tells you whether it succeeded — prefer it
every time, even for a one-off.

## Usage

```
ct <command> [args...]         run the matching ct-<command> tool
ct and <cmd...> ::: <cmd...>    run each in turn, stop at the first failure (shell-less &&)
ct or  <cmd...> ::: <cmd...>    run each in turn, stop at the first success (shell-less ||)
ct help [<command>]            show this help, or a command's own --help
ct <command> --explain         print one tool's definition (md or json)
ct --explain [md|json]         describe the whole suite (json = a manifest of every tool)
ct completions [shell]         print the shell completion script (bash/zsh/fish; auto-detects if omitted)
ct --version
```

## Boolean chains (`ct and` / `ct or`)

When you have a shell, compose tools with its own short-circuit operators —
`ct search … --quiet && ct edit …` runs the edit only if the search found
something. `ct and` / `ct or` give you the **same** short-circuit semantics in a
**single argv**, for callers with no shell to interpret `&&`/`||` (an agent or MCP
client invoking one command, an `exec` without `/bin/sh`).

Write the sub-commands one after another, separated by the literal token `:::`
(distinctive so it won't collide with a flag value; `--` is avoided because some
tools consume it for their own trailing argv):

```
ct and search --grep 'Foo::new' --quiet ::: edit --from x --to y --mutating
```

- `ct and` runs each segment left to right and **stops at the first failure**
  (non-zero exit), returning that exit code; if every segment succeeds it returns
  `0`. This is `&&`.
- `ct or` runs left to right and **stops at the first success** (exit `0`),
  returning `0`; if every segment fails it returns the **last** segment's exit
  code. This is `||`.

Because the chain reads the suite-wide `0`/`1`/`2` contract, a clean negative
(`1`, e.g. a search that found nothing) short-circuits an `and` without looking
like a crash, while a `2` (usage/abort) halts it the same way the shell would.
A blank segment (a leading, trailing, or doubled `:::`, or no commands at all) is
a usage error (exit `2`). Chains do not nest; each segment is a single
`ct-<command>` invocation.

## Shell completion

`ct completions` prints a registration script (run `eval "$(ct completions)"`,
or install it for your shell). Completion is **dynamic**: beyond subcommands,
flags, and value_enum sets (`--mode`, `--type`, …), it offers live values read
from `.ct/rules.jsonc` at completion time — rule ids for `ct check --id` /
`ct rules --promote`/`--remove`, tags for `--tag`, and def names for `--def`.

## Commands

| Command     | Runs        | Purpose                                                             |
| ----------- | ----------- | ------------------------------------------------------------------- |
| `ct search` | `ct-search` | Recursively find files by name, type, size, and content.           |
| `ct view`   | `ct-view`   | Show a file's lines by range, or the regions around a pattern.      |
| `ct tree`   | `ct-tree`   | Report a file tree with per-file line/word/char counts and filters. |
| `ct edit`   | `ct-edit`   | Find/replace across files, gated by `--expect` and `--dry-run`.     |
| `ct patch`  | `ct-patch`  | Set/delete nodes by path in JSON/JSONC/JSONL, preserving formatting. |
| `ct test`   | `ct-test`   | Run a command as a framed experiment with a templated verdict.      |
| `ct each`   | `ct-each`   | Run a command template once per item (no shell), with an aggregate `--expect` verdict. |
| `ct outline`| `ct-outline`| Report the declarations in a file or tree: kind, name, `start:end` span.   |
| `ct rules`  | `ct-rules`  | Record, promote, remove, and list the project's invariant rules (`.ct/rules.jsonc`). |
| `ct check`  | `ct-check`  | Verify the recorded invariants; five lanes, one exit status. Read-only.    |
| `ct await`  | `ct-await`  | Poll a read-only probe until it succeeds, aborts, or the bound expires.    |
| `ct steer`  | `ct-steer`  | Steer ad-hoc shell to the `ct` tool that serves it; install the PreToolUse hook. |

Dispatch is **generic**: `ct <name>` runs `ct-<name>`, so any `ct-*` tool you add
to your `PATH` is reachable through `ct` without changing `ct` itself. An unknown
command (no matching `ct-<name>`) is a usage error (exit `2`). The words `and`,
`or`, `help`, and `completions` are reserved by `ct` itself, so a `ct-and` tool on
`PATH` is reachable only by its full name, not through `ct and`.

## Discovering each tool

Every tool is self-describing, and `ct` forwards the standard idioms:

- `ct <command> --help` → the tool's human help (`ct search --help`).
- `ct help <command>` → the same, git-style.
- `ct <command> --explain [md|json]` → the tool's canonical reference (`md`) or its
  MCP / tool-use definition (`json`).

## Agent discovery

`ct --explain json` is built for agents that hoist tool definitions. The top level
is `ct`'s own tool-use definition (a `command` + `args` dispatcher), and it carries
a `tools` array with the **full definition of every suite tool** — so a single
call yields every tool an agent can drive. Agents that only read a single
`{name, description, input_schema}` still get a usable `ct` definition; agents that
understand the `tools` array get the whole suite at once.

`ct --explain md` (this page) is the human-oriented overview.

## Update check

The first time you run a `ct` command, the suite tells you it will check
crates.io for a newer release about **once a day**, in the background. The check
is polite and never in your way:

- It polls the crates.io **sparse index** with a conditional request (sending the
  last `ETag` so an unchanged index answers `304 Not Modified`), the same
  CDN-friendly path cargo's registry uses.
- The network poll runs in a **detached background process**, so a `ct` command
  never waits on it. When a newer version is found, the next run prints a one-line
  notice (to stderr, and only on a terminal — scripts and pipes stay clean).

Configure it with the `CT_UPDATE_CHECK` environment variable:

| Value | Effect |
| --- | --- |
| _(unset)_ / `daily` | check once a day (default) |
| `weekly` / `hourly` | check at that cadence |
| `<seconds>` | check no more often than this many seconds |
| `never` / `off` / `0` | disable the check entirely |

State (last check time, the latest seen version, the `ETag`) lives in the user
cache directory — `%LOCALAPPDATA%\coding-tools` on Windows, `~/Library/Caches/coding-tools`
on macOS, `$XDG_CACHE_HOME/coding-tools` (or `~/.cache/coding-tools`) elsewhere —
overridable with `CT_STATE_DIR`. Everything is best-effort: no network, a
malformed index, or an unwritable cache is ignored silently.

## Exit status

`ct` returns the child tool's exit status verbatim, so the suite-wide contract
holds end to end:

| Code | Meaning                            |
| ---- | ---------------------------------- |
| `0`  | success / verdict `SUCCESS`        |
| `1`  | clean negative / verdict `ERROR`   |
| `2`  | usage or runtime error (including an unknown `ct-<command>`) |

## Examples

```sh
# Search via the umbrella (identical to `ct-search ...`).
ct search --base src --type f --name '*.rs' --grep TODO --quiet

# Pose a search as a test: assert there are no leftover debug prints.
ct search --base src --name '*.rs' --grep 'dbg!\(' \
  --question "Are all debug prints removed?" --expect none \
  --emit '{QUESTION} -> {RESULT}'

# Run a framed experiment via the umbrella (cat is on ct-test's allowlist).
ct test --question "Is the config free of deprecated keys?" \
  --cmd cat -- config.toml --err-match 'old_key' --emit '{RESULT}'

# Dispatch one check over several items (no shell loop).
ct each --items Parser Lexer -- ct-search --base src --grep '{ITEM}' --quiet

# Shell-less AND: edit only if the marker is present (one argv, no && needed).
ct and search --base src --grep 'Foo::new' --quiet ::: edit --from Foo::new --to Foo::create --mutating

# Shell-less OR: fall back to a second probe if the first finds nothing.
ct or search --base src --grep 'TODO' --quiet ::: search --base src --grep 'FIXME' --quiet

# Per-tool and whole-suite agent definitions.
ct search --explain json     # one tool
ct --explain json            # manifest of the whole suite
```
