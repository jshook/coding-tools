# ct-rules — Say What the Rules Are

> The specification and storage interface of the invariant surface: record a
> verified rule, park an aspiration, promote it when it holds, name shared
> vocabulary, and wire `ct check` into the build. The only tool that writes
> the store.

While working, truths about a codebase get established the hard way — a
search here, an outline there, a dependency question answered once and
forgotten. `ct-rules` records them: each becomes a **rule** in
`.ct/rules.jsonc` with the `question` it answers, the **probe** that answers
it, and the `why` that justifies it — verified at the moment it is written,
reviewed in git like any other artifact, and re-verified forever after by
`ct-check`.

This document is the canonical reference for `ct-rules`. It is also what the
tool prints for `ct-rules --explain` (`--explain md`); `ct-rules --explain
json` prints the equivalent MCP / tool-use definition. The store schema,
probe gate, and bridge are specified in `docs/specs/rules.md`; a worked,
copy-pasteable rule for each invariant category is in
`docs/specs/rules-examples.md`; the verifying side is documented in `ct-check
--explain`.

## The store

`.ct/rules.jsonc` under the project root (the `.ct` directory is the suite's
home for project-local state), discovered by walking upward to the nearest
`.ct`; `--file` overrides. `--init` (or the first `--add`) scaffolds a
commented store. All mutations go through the suite's own comment-preserving
patch machinery, so hand-written commentary survives every edit — and
anything beyond add/promote/remove/def is an ordinary `ct-patch` edit.

Two sections:

- **`defs`** — named vocabulary. A def is a string (expands in place) or a
  list (expands to multiple argv elements); rules reference them as
  `{def:NAME}` instead of re-listing, so shared sets live in exactly one
  place.
- **`rules`** — the invariants, in run order. Fields: `id`, `question`,
  `probe`, `why`, `prompt` (retained request prose, see below), `tags`,
  `added`, `timeout`, `pending`, `severity` (`fail`/`warn`), `expect`
  (outcome adapter), `network`.

The store is written for humans: a standing header comment explains what the
file is (re-established on every write if it has gone missing), each rule is
recorded one field per line in a stable order with the probe argv inline,
and rules are separated by a blank line — the file reads as a ledger of what
the project knows about itself.

## Recording is verification

```sh
ct rules --add no-debug-prints \
  --question "Are all debug prints removed from src?" \
  --why "dbg! output leaked into the 0.1 release notes" \
  --tag hygiene \
  -- ct-search --base src --name '*.rs' --grep 'dbg!\(' --expect none --detail
```

`--add` gate-validates the probe and **runs it immediately**:

- It **holds** → recorded (with `added` date) and confirmed.
- It is **violated** → refused (exit `1`) with the probe's own violation
  output — an enforced rule records an established truth, so fix the code
  first or park it: `--pending` records an aspiration that `ct-check`
  reports separately and never enforces.
- The probe is **broken** (bad pattern, missing binary, exit `2`) → refused
  (exit `2`).

Duplicate ids are refused — ids are history, not slots to overwrite.

```sh
ct rules --promote no-unwrap-in-lib   # re-runs the probe; holds => enforced
ct rules --remove old-rule            # delete by exact id
ct rules --def 'core-types=["Parser","Lexer","Emitter"]'
ct rules --def 'domain-layer=src/domain'
ct rules --list
ct rules --flatten                    # strip retained prompts (see below)
```

## Prototyping before recording

A bare probe with **no verb** runs it and reports — *without saving* — so you can
iterate on a check before committing it:

```sh
ct rules -- deps --acyclic --members         # runs the built-in deps check now
# prototype: HOLDS (deps: all assertions hold); not saved — add --add ID … to record it
ct rules -- mods --layers 'infra*,domain*'   # try a module-layering check
```

Exit status follows the outcome (`0` holds / `1` violated / `2` broken), so a
prototype composes in `&&`/`||`. When it's right, the same probe with `--add ID
--question …` records it. (`deps`/`mods` are the built-in checks — see
`ct-check --explain`; any gated probe can be prototyped this way.)

## Retaining the request behind a rule

When a human asks an agent to record a rule, the *prose of that request* is
worth keeping: months later it is the difference between editing the rule
confidently and guessing at intent. `--prompt TEXT` retains the verbatim
request in the rule's `prompt` field:

```sh
ct rules --add no-debug-prints --question "..." \
  --prompt "please make sure we never ship debug prints again" \
  -- ct-search --base src --grep 'dbg!\(' --expect none --quiet
```

`--prompt` and `--why` accept payload schemes — `file:PATH` reads the prose
verbatim from a file (multi-line, zero quoting), `text:VALUE` escapes the
prefix. The confirmation **says so** ("the originating request is retained
in the rule's `prompt` field"), so the human can edit or drop it immediately
if they prefer. `prompt` is provenance only — verification never reads it. When the
prose has served its purpose, `ct rules --flatten` strips every retained
prompt in one pass (naming the rules it touched), leaving only the
mechanical definitions.

A def's VALUE is parsed as JSON when it is valid JSON (lists!), else taken as
a string — the same promotion `ct-patch` uses.

## What a probe may run

Probes observe; they never change anything. The gate is **compiled in and
immutable** — a store entry selects from it and can never extend it:

- the suite's read-only tools (`ct-search`, `ct-outline`, `ct-tree`,
  `ct-view`, plus `ct-test`, plus `ct-each` *without* `--mutating`);
- the **built-in checks** `deps` (crate graph over `cargo metadata`) and `mods`
  (module graph from `use` edges), run in-process from the rule layer;
- the **bridge**: `cargo metadata`, `cargo tree` (hermetic flags enforced),
  `cargo deny check` (offline unless the rule opts in with `--network` —
  the project's own `deny.toml` remains the policy file), and
  `rust-analyzer search`/`symbols` (search-only) for semantic, AST-aware
  queries.

Never: the mutating suite tools, `ct-check` (no self-recursion), `ct-rules`
itself, shells, or any unlisted external command.

## Outcome adapters

Suite observers speak the exit contract (`0` holds / `1` violated / `2`
broken) — no adapter needed. Bridge tools don't, so a rule may carry one:

| Flag at `--add`        | Stored as | Meaning |
| ---------------------- | --------- | ------- |
| *(none)*               | —         | exit contract (default). |
| `--expect empty`       | `"expect": "empty"` | holds iff the probe exits `0` printing nothing (the `cargo tree -d` shape). |
| `--expect-ok PATTERN`  | `{"ok-match": …}`  | holds when the pattern appears in the probe's output (e.g. `cargo tree -i X` saying the crate is absent). |
| `--expect-err PATTERN` | `{"err-match": …}` | violated when the pattern appears. |

Matcher semantics are exactly `ct-test`'s: substring→glob→regex promotion,
unanchored, err decisive over ok, a required ok that is missing is a
violation.

## Other `--add` fields

| Option       | Meaning |
| ------------ | ------- |
| `--question` | Required: what the rule answers. |
| `--why`      | Encouraged: the rationale, printed whenever the rule fails. |
| `--prompt`   | The verbatim human request behind the rule, retained as provenance (strippable with `--flatten`). |
| `--tag LIST` | Labels for `ct-check --tag` selection. |
| `--severity warn` | Violations report (`WARN` lane) but never redden the exit — the probation ramp before tightening to `fail`. |
| `--network`  | Permit network where the bridge entry deems it meaningful (currently `cargo deny check`, e.g. fresh advisories). A reviewed, per-rule decision visible in the store diff. |
| `--timeout SECS` | Per-rule probe bound, recorded in the store. |
| `--pending`  | Record an aspiration that does not yet hold. |

## The cargo hook

```sh
ct rules --hook cargo
# writes tests/ct_invariants.rs at the project root
```

The shim is plain, reviewed-in-git test code (no `build.rs`, nothing at
compile time, nothing on dependency builds): under `cargo test` it runs
`ct check --quiet` from `CARGO_MANIFEST_DIR` and fails the test on a
non-zero exit; a missing `ct` binary fails **loudly** with instructions
rather than passing silently. `ct-rules` refuses to overwrite a
`tests/ct_invariants.rs` it did not generate.

## Exit status

| Code | Meaning |
| ---- | ------- |
| `0`  | the add/promote/remove/def/list/init/hook succeeded |
| `1`  | refused on the merits — a strict `--add` or a `--promote` whose probe is currently violated |
| `2`  | usage or store error — duplicate/unknown id, non-gated probe, broken probe, malformed store |

`ct-rules` writes only the store (and the hook shim) and is on **no**
allow-gate: nothing in the suite can be driven into running it.

### Documentation

| Option                 | Effect |
| ---------------------- | ------ |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help. |
| `-V`, `--version`      | Version. |
