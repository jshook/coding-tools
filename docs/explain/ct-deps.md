# ct-deps — Crate-Graph Invariants

> Assert properties of the resolved dependency graph — banned crates,
> forbidden paths between packages, duplicate versions — with every violation
> carrying its evidence path.

Crate-level architecture questions ("are we free of openssl?", "does the api
crate stay off the database crate?", "did a second version of serde sneak
in?") are answerable exactly and cheaply from cargo's own resolved graph.
`ct-deps` asks them as assertions: the graph comes from `cargo metadata
--format-version 1 --locked --offline` — hermetic by construction, no
network, no lockfile writes — and each violated assertion reports the
dependency path (or version list) that proves it.

This document is the canonical reference for `ct-deps`. It is also what the
tool prints for `ct-deps --explain` (`--explain md`); `ct-deps --explain
json` prints the equivalent MCP / tool-use definition.

## Replaces patterns like

```sh
# eyeballing cargo tree output, or scripting against its text format
cargo tree -i openssl && echo "uh oh"
cargo tree -d | grep -c .
```

with:

```sh
ct-deps --deny openssl --duplicates \
  --question "Is the dependency tree policy-clean?" \
  --emit '{QUESTION} -> {RESULT} ({COUNT} violation(s))'
```

## Assertions

| Option         | Argument | A violation when…                                          |
| -------------- | -------- | ----------------------------------------------------------- |
| `--deny`       | `NAME`   | a crate named `NAME` is reachable from any workspace member (repeatable). Evidence: the shortest dependency path. |
| `--forbid`     | `'A=>B'` | any package named `A` reaches a package named `B` (repeatable). `A` absent from the graph is an error (`exit 2`) — a defective assertion, not a clean pass. Evidence: the path. |
| `--duplicates` | —        | any crate resolves at more than one version. Evidence: the version list. |

At least one assertion is required. `--edges normal,build,dev` restricts
which dependency-edge kinds are traversed (default: all three) — e.g.
`--edges normal` asks about the shipped artifact only, ignoring dev/build
tooling.

Workspace-member layering is the `--forbid` form: `--forbid 'my-api=>my-db'`
holds the layering between two members of the same workspace.

## Source of truth

One `cargo metadata` invocation per run, with `--format-version 1 --locked
--offline` always enforced: the lockfile is read, never written; the network
is never touched; a workspace that cannot resolve offline is an error (`exit
2`), never a silent pass. `--timeout SECS` bounds the cargo child
(process-group kill). `ct-deps` is **read-only** and is on the `ct-test`
allowlist — and therefore admissible in `ct-each` dispatch and `ct-rules`
probes, where it is the native form of crate-graph rules.

## Reporting

Default output: one line per violation (`check: subject: evidence`) and a
summary (`N violation(s) -> RESULT`); `--quiet` for exit-status-only.
`--emit`/`--emit-stderr` templates take `{RESULT}` `{COUNT}` `{VIOLATIONS}`
(newline-joined violation lines) `{QUESTION}`. `--json` emits
`{tool, verdict, count, violations:[{check, subject, evidence}]}`.
`--heartbeat` (with `--heartbeat-emit`/`--heartbeat-to`) pulses as everywhere
in the suite.

## Exit status

| Code | Meaning                                                    |
| ---- | ---------------------------------------------------------- |
| `0`  | every assertion holds                                      |
| `1`  | at least one violation (each printed with its evidence)    |
| `2`  | usage or runtime error — no assertions, bad `--forbid` spec or unknown source package, cargo metadata failure or timeout |

## Examples

```sh
# Portability gate: pure-Rust TLS only, counting only shipped dependencies.
ct-deps --deny openssl --deny native-tls --edges normal

# Workspace layering: the API crate must not reach the storage crate.
ct-deps --forbid 'my-api=>my-storage'

# Version hygiene, framed for the rule store (exit contract, no adapter needed):
ct rules --add no-duplicate-deps \
  --question "Is the tree free of duplicate crate versions?" \
  -- ct-deps --duplicates --quiet

# Machine-readable.
ct-deps --duplicates --json
```

### Documentation

| Option                 | Effect |
| ---------------------- | ------ |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help. |
| `-V`, `--version`      | Version. |
