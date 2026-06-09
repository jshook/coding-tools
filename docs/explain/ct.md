# ct — Coding Tools

> One short command for the whole suite. `ct <command> [args…]` runs the matching
> `ct-<command>` tool — `ct search` → `ct-search`, `ct test` → `ct-test` — the same
> git-style convention `git`/`cargo` use for external subcommands.

`ct` is a thin launcher: it locates `ct-<command>` (beside the `ct` executable, or
on your `PATH`) and hands off, passing the child's stdout, stderr, and **exit
status** through unchanged. It adds no behaviour of its own. Installing the suite
gives you `ct` plus the `ct-*` tools; you can drive everything through `ct`, or
call a tool directly by its full name.

This document is the canonical reference for `ct`. It is also what the tool prints
for `ct --explain` (`--explain md`); `ct --explain json` prints a machine-readable
**manifest** of the whole suite (see *Agent discovery*).

## Usage

```
ct <command> [args...]    run the matching ct-<command> tool
ct help [<command>]       show this help, or a command's own --help
ct <command> --explain    print one tool's definition (md or json)
ct --explain [md|json]    describe the whole suite (json = a manifest of every tool)
ct --version
```

## Commands

| Command     | Runs        | Purpose                                                             |
| ----------- | ----------- | ------------------------------------------------------------------- |
| `ct search` | `ct-search` | Recursively find files by name, type, size, and content.           |
| `ct view`   | `ct-view`   | Show a file's lines by range, or the regions around a pattern.      |
| `ct tree`   | `ct-tree`   | Report a file tree with per-file line/word/char counts and filters. |
| `ct edit`   | `ct-edit`   | Find/replace across files, gated by `--expect` and `--dry-run`.     |
| `ct patch`  | `ct-patch`  | Set/delete nodes by path in JSON/JSONC/JSONL, preserving formatting. |
| `ct test`   | `ct-test`   | Run a command as a framed experiment with a templated verdict.      |

Dispatch is **generic**: `ct <name>` runs `ct-<name>`, so any `ct-*` tool you add
to your `PATH` is reachable through `ct` without changing `ct` itself. An unknown
command (no matching `ct-<name>`) is a usage error (exit `2`).

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

# Run a framed experiment via the umbrella.
ct test --question "Does seq count?" --cmd seq --emit '{RESULT}' -- 1 3

# Per-tool and whole-suite agent definitions.
ct search --explain json     # one tool
ct --explain json            # manifest of the whole suite
```
