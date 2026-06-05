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

| Tool | Purpose | Documentation |
| ---- | ------- | ------------- |
| `ctsearch` | Recursively find files by name, type, size, and content from a chosen root — replaces `find … \| xargs grep …`. | [explain/ctsearch.md](../explain/ctsearch.md) |
| `cttest`   | Run a command as a framed experiment — pose a question, classify the result from output, emit a templated verdict. | [explain/cttest.md](../explain/cttest.md) |

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
* **`--explain [md|json]`.** Every tool describes itself for agents: `md`
  (default) emits an [llms.txt](https://llmstxt.org/)-style Markdown guide;
  `json` emits a [Model Context Protocol](https://modelcontextprotocol.io) /
  tool-use definition (`{name, description, input_schema}`) a harness can ingest
  directly.

## Documentation layout

| Path | Audience | Role |
| ---- | -------- | ---- |
| `docs/specs/commands.md` | human | This synopsis and index. |
| `docs/explain/<tool>.md` | human + agent | Canonical per-tool reference; emitted by `<tool> --explain md`. |
| `docs/explain/<tool>.json` | agent | MCP / tool-use definition; emitted by `<tool> --explain json`. |

The `docs/explain/` files are compiled into the binaries (`include_str!`), so the
`--explain` output and the on-disk docs are always the same bytes.
