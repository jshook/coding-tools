# coding-tools

Declarative, agent-friendly command-line tools for working in a codebase, behind
one short `ct` command. Each tool replaces an ad-hoc shell pattern with a single,
self-describing command, and every tool is framed the same way — so what you learn
from one transfers to the next, and an agent can discover and drive them uniformly.

```sh
cargo install coding-tools     # installs ct, ct-search, ct-view, ct-edit, ct-test
```

## Tools

`ct <command>` dispatches to `ct-<command>` (git-style), or call each tool by its
full name.

| Command     | Tool        | Purpose                                                                       |
| ----------- | ----------- | ---------------------------------------------------------------------------- |
| `ct search` | `ct-search` | Recursively find files by name, type, size, and content (`find \| xargs grep`). |
| `ct view`   | `ct-view`   | Show a file's lines by range, or the regions around a pattern with context.   |
| `ct tree`   | `ct-tree`   | Report a file tree with per-file line/word/char counts; filter, sort, summarise. |
| `ct edit`   | `ct-edit`   | Find/replace across files, gated by an `--expect` verdict and `--dry-run`.    |
| `ct patch`  | `ct-patch`  | Set/delete nodes by path in JSON/JSONC/JSONL, preserving comments and layout. |
| `ct test`   | `ct-test`   | Run a command as a framed experiment; classify the result from its output.    |

## Why

A coding agent's loop is *locate → read → change → verify*. These tools make each
step **bounded, deterministic, and self-verifying**, which is what lets it run with
less supervision:

- **Framed verdicts.** A search or a command run can pose a `--question`, classify
  into a `SUCCESS`/`ERROR` verdict, and `--emit` a templated line; exit status
  follows the verdict. `ct-search --expect none` passes when nothing is found (a
  negative assertion); `ct-edit --expect =1` writes only if exactly one site
  matched, so a wrong-sized change fails loudly instead of applying silently.
- **Preview before write.** `ct-edit --dry-run` shows the diff and verdict without
  touching disk; edits preserve every untouched byte (indentation, terminators).
- **Read-only by default where it matters.** `ct-test` runs only a fixed, immutable
  allowlist of read-only commands.
- **Machine-readable.** Every tool takes `--json` for structured results, and
  `--explain [md|json]` prints its own documentation / tool-use definition. The
  umbrella's `ct --explain json` is a one-call manifest of the whole suite.

## Shared conventions

- **Pattern promotion.** Any *pattern* argument is promoted with one rule: no
  metacharacters → literal substring; glob metacharacters (`*` `?` `[ ]`) that are
  not a valid regex → glob; otherwise → regex.
- **Exit status.** `0` = success / verdict `SUCCESS`; `1` = clean negative /
  verdict `ERROR`; `2` = usage or runtime error. The `0`/`1` split composes in
  `&&`/`||` pipelines.
- **`--explain [md|json]`.** Every tool is self-describing for humans and agents.

## Examples

```sh
# Find Rust files mentioning TODO — just yes/no.
ct search --base src --name '*.rs' --grep TODO --quiet

# Assert there are no leftover debug prints (passes when nothing matches).
ct search --base src --name '*.rs' --grep 'dbg!\(' --expect none

# Read a span, or the neighbourhood of a symbol.
ct view src/lib.rs --range 40:80
ct view src/lib.rs --match Verdict --context 3 --json

# Preview a one-site rename, then apply it.
ct edit --base src --name '*.rs' --find 'old_api(' --replace 'new_api(' --expect =1 --dry-run

# Frame a read-only check as a test.
ct test --question "Is the config free of deprecated keys?" \
  --cmd cat -- config.toml --err-match 'old_key' --emit 'result: {RESULT}'
```

The canonical reference for each tool is its `--explain md` output, mirrored under
[`docs/explain/`](docs/explain/); [`docs/specs/commands.md`](docs/specs/commands.md)
is the suite index.

## License

Apache-2.0. See [LICENSE](LICENSE).
