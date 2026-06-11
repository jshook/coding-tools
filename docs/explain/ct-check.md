# ct-check — Verify the Project's Invariants

> Run the rule store's probes — the project's recorded structural truths —
> and report each rule in one of five lanes, with one exit status for the
> whole surface. Purely read-only; rules are *said* with `ct-rules`.

A project accumulates truths about itself: *no `dbg!` lines ship*, *`Verdict`
is defined exactly once*, *the dependency tree is free of openssl*. The rule
store (`.ct/rules.jsonc`) gives those observations a durable, reviewed home;
`ct-check` is the bounded command that re-verifies all of them — at the start
of a session, after a refactor, or from `cargo test` via the generated hook.

This document is the canonical reference for `ct-check`. It is also what the
tool prints for `ct-check --explain` (`--explain md`); `ct-check --explain
json` prints the equivalent MCP / tool-use definition. The full surface
specification (store schema, gate, bridge) is `docs/specs/rules.md`; the
authoring side is documented in `ct-rules --explain`.

## The model

- A **rule** is one recorded observation: an `id`, the `question` it answers,
  the **probe** (an argv vector, never a shell) that answers it by scanning
  for known violations, and the `why` behind it.
- A probe reports **violations**; a rule *holds* when its probe reports none.
  Probes run the suite's read-only tools (`ct-search`, `ct-outline`,
  `ct-tree`, `ct-view`, `ct-deps`, `ct-each` without `--mutating`, `ct-test`) or a
  compiled-in **bridge** invocation of established Rust tooling
  (`cargo metadata`, `cargo tree`, `cargo deny check`, `rust-analyzer
  search|symbols` — hermetic flags enforced). The gate is immutable: a store
  entry selects from it and can never extend it.
- **Runs are pure.** `ct-check` writes nothing — not the store, not state.
  All writing lives in `ct-rules`.

## Store discovery

The store is `.ct/rules.jsonc`, found by walking parent directories to the
nearest `.ct` (git-style), so `ct check` works from any subdirectory.
`--file` overrides; no `.ct` found is exit `2` with the searched origin named.

The store is validated before anything runs: malformed entries, duplicate
ids, unknown defs, and non-gated probes are a usage error (exit `2`) naming
the offending rule — a refusal can never strike mid-run.

## Lanes

| Lane      | Meaning | Effect on exit |
| --------- | ------- | -------------- |
| `SUCCESS` | the probe reported zero violations | — |
| `ERROR`   | violations found (severity `fail`) | exit `1` |
| `WARN`    | violations found (severity `warn`) — visibility without blockage | none |
| `PENDING` | an aspiration recorded with `--pending`: its current state is reported (`not yet held` / `now holds — promote?`) | none |
| `BROKEN`  | the probe itself is defective — exited `2`, died, timed out, or its binary is missing | exit `2` |

Any `BROKEN` rule makes the whole run exit `2`: a defective rule store is a
maintenance signal and must not masquerade as a clean verdict in either
direction. A red lane is never unexplained — the reason, the rule's `why`,
and the head of the probe's own violation output go to stderr.

## Invocation

| Option        | Argument | Meaning |
| ------------- | -------- | ------- |
| `--file`      | `PATH`   | The store. Default: nearest `.ct/rules.jsonc` upward. |
| `--id`        | `PATTERN`| Select rules by id (substring→glob→regex promoted, anchored). |
| `--tag`       | `LIST`   | Select rules carrying any of these tags (comma-separated). |
| `--fail-fast` | —        | Stop after the first enforced violation; the rest report as `SKIPPED`. |
| `--list`      | —        | Print the selected rules (id, flags, question, tags); run nothing. |
| `--quiet`     | —        | Suppress per-rule lines and the default summary (stderr diagnostics remain). |
| `--json`      | —        | One structured result; overrides text output and emit templates. |
| `--timeout`   | `SECS`   | Default per-rule bound (fractional allowed); a rule's own `timeout` field overrides. A timed-out probe is `BROKEN`. |

Rules run in store order, sequentially, each independent. `--heartbeat SECS`
(with `--heartbeat-emit`, `--heartbeat-to`) pulses as everywhere in the
suite, with `{ID}` `{DONE}` `{TOTAL}` available as live tokens.

## Reporting

Default per-rule line: `LANE  id  question`. `--emit-each TEMPLATE` replaces
it — tokens `{RESULT}` `{ID}` `{QUESTION}` `{CODE}` `{WHY}` `{CMD}`. Default
summary: `N/M invariant(s) hold[, n warned, n pending, n broken, n skipped]
-> RESULT`. `--emit` / `--emit-stderr` templates take `{RESULT}` `{OK}`
`{ERRORS}` `{WARNED}` `{PENDING}` `{BROKEN}` `{SKIPPED}` `{TOTAL}` `{REASON}`.

The `--json` result carries `tool`, `verdict`, `store`, the per-lane counts,
and a `rules` array of `{id, question, lane, code, reason, why}`.

## Exit status

| Code | Meaning |
| ---- | ------- |
| `0`  | every selected enforced rule holds (`WARN`/`PENDING` never affect status) |
| `1`  | at least one enforced rule is violated |
| `2`  | usage or store error, no `.ct` found, or any rule is `BROKEN` |

## Composing

`ct-check` is itself on the suite's read-only allowlist, so `ct-test` can
frame a whole invariant run and `ct-each` can dispatch it. A *rule's probe*
may not run `ct-check` (no self-recursion through the store).

```sh
ct check                          # everything the project knows about itself
ct check --tag hygiene            # one vocabulary slice
ct check --id 'no-*' --fail-fast  # a fast negative gate
ct test --question "Do all invariants hold?" --cmd ct-check -- --quiet
```

### Documentation

| Option                 | Effect |
| ---------------------- | ------ |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help. |
| `-V`, `--version`      | Version. |
