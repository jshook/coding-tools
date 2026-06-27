# coding-tools

Declarative, agent-friendly command-line tools for working in a codebase, behind
one short `ct` command. Each tool replaces an ad-hoc shell pattern with a single,
self-describing command, and every tool is framed the same way — so what you learn
from one transfers to the next, and an agent can discover and drive them uniformly.

```sh
cargo install coding-tools     # installs ct and every ct-* tool below
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
| `ct each`   | `ct-each`   | Run a command template once per item (no shell); aggregate `--expect` verdict. |
| `ct outline`| `ct-outline`| Report a file's declarations — kind, name, `start:end` span — for bounded reads. |
| `ct okf`    | `ct-okf`    | Author and query OKF knowledge bundles — validate, list/query metadata, links, scaffold. |
| `ct rules`  | `ct-rules`  | Record the project's invariants in `.ct/rules.jsonc` — verified at the moment they're written. |
| `ct check`  | `ct-check`  | Re-verify every recorded invariant; five lanes, one exit status. Read-only.   |
| `ct await`  | `ct-await`  | Wait, boundedly, for an external outcome via a read-only probe.               |

`ct rules`/`ct check` also host **built-in checks** — `deps` (crate-graph),
`mods` (module-graph), and `okf` (OKF-bundle conformance) invariants — recorded
and verified as rules (`ct rules … -- deps …` / `-- mods …` / `-- okf …`), not as
separate top-level commands.

`ct-search`, `ct-tree`, `ct-view`, and `ct-outline` are also **OKF-aware**: they
auto-detect a Markdown concept's YAML frontmatter and surface it additively
(`ct search --okf-type/--okf-tag`, `ct tree --sort/--group okf-type`, `ct view
--frontmatter`, `ct outline --frontmatter`) without changing their default
output. See the [OKF spec](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md).

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
- **Atomic batches.** `ct-edit --script` runs a whole batch of block edits under
  prepare/confirm/write: every edit is simulated and judged in memory (and every
  target pre-flighted for writability) before anything is written — one failing
  anchor means zero writes, never a half-applied batch. `ct-okf --script` applies
  the same standard to a batch of OKF mutations (cascading over an in-memory
  bundle overlay), so a multi-step authoring change lands all-or-nothing.
- **Read-only by default where it matters.** `ct-test` runs only a fixed, immutable
  allowlist of read-only commands; `ct-each` adds the suite's own gated mutating
  tools only behind an explicit `--mutating` flag. There is **no shell mode
  anywhere** — every dispatch is a direct argv launch.
- **Bounded and observable.** Every tool takes `--timeout` (self-bounding for the
  read-only/mutating tools, a child process-group kill folded into the verdict for
  `ct-test`/`ct-each`) and `--heartbeat`, a minimal templated liveness pulse for
  long runs.
- **Machine-readable.** Every tool takes `--json` for structured results, and
  `--explain [md|json]` prints its own documentation / tool-use definition. The
  umbrella's `ct --explain json` is a one-call manifest of the whole suite.

## Shared conventions

- **Pattern promotion.** Any *pattern* argument is promoted with one rule: no
  metacharacters → literal substring; glob metacharacters (`*` `?` `[ ]`) that are
  not a valid regex → glob; otherwise → regex. `--mode literal|glob|regex` pins
  the interpretation (promotion off) for verbatim code anchors.
- **Payload schemes.** Payload-typed values accept `file:PATH` (the file's
  contents, verbatim — never promoted) and `text:VALUE` (the escape for literal
  values starting with a scheme prefix). A multi-line pattern matches as a
  line-anchored literal **block** in `ct-search`/`ct-view`/`ct-edit`, with a
  nearest-miss diagnostic when it matches nothing.
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

# Dispatch one check over several items — what used to need a bash for-loop.
ct each --items Parser Lexer Emitter -- \
  ct-search --base src --grep '{ITEM}::new' --quiet

# A verbatim block edit with zero quoting — write the payloads as files.
ct edit --base src --name '*.rs' \
  --find file:target/find.block --replace file:target/replace.block \
  --expect =1 --dry-run

# A batch of structural edits, atomic by construction (.ctb script):
# everything is verified in memory; one failing anchor means zero writes.
ct edit --base src --name '*.rs' --script target/edits.ctb
```

The canonical reference for each tool is its `--explain md` output, mirrored under
[`docs/explain/`](docs/explain/); [`docs/specs/commands.md`](docs/specs/commands.md)
is the suite index.

## License

Apache-2.0. See [LICENSE](LICENSE).
