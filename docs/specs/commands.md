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
| `ct-each`   | Run a command template once per item — no shell — with per-item verdicts and an aggregate `--expect`. | [explain/ct-each.md](../explain/ct-each.md) |
| `ct-outline`| Report the declarations in a file or tree — kind, name, `start:end` span — for bounded reads. | [explain/ct-outline.md](../explain/ct-outline.md) |
| `ct-rules`  | Record, promote, remove, and list the project's invariant rules (`.ct/rules.jsonc`); writes the store, on no gate. | [explain/ct-rules.md](../explain/ct-rules.md) |
| `ct-check`  | Verify the recorded invariants — five lanes (`SUCCESS`/`ERROR`/`WARN`/`PENDING`/`BROKEN`), one exit status; purely read-only. | [explain/ct-check.md](../explain/ct-check.md) |
| `ct-deps`   | Assert crate-graph invariants over hermetic `cargo metadata` — deny crates, forbid `A=>B` paths, duplicates — with evidence. | [explain/ct-deps.md](../explain/ct-deps.md) |
| `ct-await`  | Poll a gated read-only probe until success, an abort pattern, or the required bound — observe work you don't execute. | [explain/ct-await.md](../explain/ct-await.md) |

Each tool's page is the **canonical, self-contained reference** for that tool —
the same text the tool emits from `<tool> --explain` — so it never drifts from
the binary. There is no separate spec; the per-tool docs are the spec.

## Shared conventions

These hold for every tool; each tool's page documents them in full.

* **Pattern promotion (substring → glob → regex).** Any *pattern* argument is
  promoted with one rule: text with no metacharacters is a literal substring;
  glob metacharacters (`*` `?` `[ ]`) that are not a valid regex are treated as
  a glob; otherwise the pattern is used as a regex. `--mode literal|glob|regex`
  pins the interpretation (promotion **off**) — state `literal` when the
  pattern is verbatim code.
* **Payload schemes (`file:` / `text:`).** Payload-typed values (patterns,
  replacements, structured values, stdin text, prose) accept `file:PATH` —
  the value is the file's contents, verbatim, never promoted — and
  `text:VALUE`, the escape for a value that genuinely begins with a scheme
  prefix. Only those two exact prefixes are reserved (`http://…` is
  unaffected). A **multi-line** pattern payload matches as a line-anchored
  literal **block** (K lines match K consecutive source lines,
  byte-for-byte) in `ct-search`/`ct-view`/`ct-edit`, with a *nearest-miss*
  diagnostic when a block matches nothing.
* **Prepare/confirm/write.** Every block operation spanning multiple edit
  sites or files (`ct-edit --script`) is validated *in full, in memory* —
  matching, expectations, and write pre-flight — before any file changes;
  nothing is written unless the whole operation is confirmed. There is no
  flag that permits a partial write. See `docs/specs/blocks.md`.
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
* **Run bounds and liveness.** Every leaf tool takes `--timeout SECS`
  (fractional allowed) and `--heartbeat SECS` with `--heartbeat-emit` /
  `--heartbeat-to`. For the self-contained tools a timeout aborts the run with
  exit `2` (the mutating tools never interrupt a write phase once it begins);
  for the dispatching tools (`ct-test`, `ct-each`) it kills the child's process
  group and folds into the verdict (`ERROR`, `{CODE}` = `timeout`). The
  heartbeat is minimal by default (`[{ELAPSED}s]`, to stderr) and
  token-customisable. There is **no shell mode anywhere**: every dispatch is a
  direct argv launch, gated by a fixed, immutable command list.
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
