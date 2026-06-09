# Synopsis

This project is a set of command-line tools that streamline project
comprehension, study, analysis, and refactoring. They intentionally replace
traditional patterns of cobbled-together shell usage with single, declarative
commands.

Every tool follows a common form for documentation, option naming, feature
discovery, pattern matching, and exit status — so that what you learn from one
tool transfers to the next, and so that agents can discover and drive each tool
the same way.

## Tools

The umbrella `ct` is the front door: `ct <command>` runs the matching
`ct-<command>` tool — `ct search` → `ct-search`, `ct test` → `ct-test` — the same
git-style convention `git`/`cargo` use. Every tool is also a standalone binary you
can call by its full name.

| Tool | Purpose | Documentation |
| ---- | ------- | ------------- |
| `ct` | Umbrella launcher: dispatch to `ct-<command>`; `ct --explain json` is a one-call manifest of the whole suite. | [explain/ct.md](../explain/ct.md) |
| `ct-search` | Recursively find files by name, type, size, and content from a chosen root — replaces `find … \| xargs grep …`. | [explain/ct-search.md](../explain/ct-search.md) |
| `ct-view`   | Show one file's lines by range, or the regions around a pattern with context — a bounded read. | [explain/ct-view.md](../explain/ct-view.md) |
| `ct-tree`   | Report a file tree with per-file line/word/char counts; filter, sort, and summarise. | [explain/ct-tree.md](../explain/ct-tree.md) |
| `ct-edit`   | Find/replace across selected files, gated by an `--expect` verdict and previewable with `--dry-run`. | [explain/ct-edit.md](../explain/ct-edit.md) |
| `ct-patch`  | Set/delete nodes by path in JSON/JSONC/JSONL, preserving comments and formatting. | [explain/ct-patch.md](../explain/ct-patch.md) |
| `ct-test`   | Run a command as a framed experiment — pose a question, classify the result from output, emit a templated verdict. | [explain/ct-test.md](../explain/ct-test.md) |

Each tool's page is the **canonical, self-contained reference** for that tool —
the same text the tool emits from `<tool> --explain` — so it never drifts from
the binary. There is no separate spec; the per-tool docs are the spec.

## Shared conventions

These hold for every tool; each tool's page documents them in full.

* **Pattern promotion (substring → glob → regex).** Any *pattern* argument is
  promoted with one rule: text with no metacharacters is a literal substring;
  glob metacharacters (`*` `?` `[ ]`) that are not a valid regex are treated as
  a glob; otherwise the pattern is used as a regex.
* **Exit status.** `0` = success / a match was found; `1` = clean negative (no
  match / the experiment failed); `2` = usage or runtime error. The `0`/`1`
  split makes the tools composable in `&&`/`||` pipelines.
* **Framed verdict.** Any tool can pose its work as a pass/fail test: a
  `--question` (printed as a `== … ==` banner), a classification into a
  `SUCCESS`/`ERROR` *verdict*, and an `--emit TEMPLATE` line whose `{TOKEN}`s
  expand to the result. Exit status follows the verdict (`0`/`1`/`2` as above).
  `ct-test` classifies a command's output; `ct-search` classifies its match count
  via `--expect` (`any`/`none`/`N`/`=N`/`+N`/`-N`) — so "find nothing" can be the
  passing condition. This is the unification point: a search and a
  command-experiment are framed and consumed the same way.
* **`--explain [md|json]`.** Every tool describes itself for agents: `md`
  (default) emits an [llms.txt](https://llmstxt.org/)-style Markdown guide;
  `json` emits a [Model Context Protocol](https://modelcontextprotocol.io) /
  tool-use definition (`{name, description, input_schema}`) a harness can ingest
  directly. The umbrella's `ct --explain json` carries a `tools` array bundling
  every leaf tool's definition, so an agent can hoist the whole suite in one
  call; `ct help [<command>]` and `ct <command> --help` cover the standard
  discovery idioms.

## Documentation layout

| Path | Audience | Role |
| ---- | -------- | ---- |
| `docs/specs/commands.md` | human | This synopsis and index. |
| `docs/explain/<tool>.md` | human + agent | Canonical per-tool reference; emitted by `<tool> --explain md`. |
| `docs/explain/<tool>.json` | agent | MCP / tool-use definition; emitted by `<tool> --explain json`. |

The `docs/explain/` files are compiled into the binaries (`include_str!`), so the
`--explain` output and the on-disk docs are always the same bytes. `ct.json` is
the one composite: its `tools` array embeds each leaf tool's definition, and a
test asserts those embedded copies stay identical to the standalone leaf files.
