---
type: Specification
title: ct rules by example
timestamp: 2026-07-19
---

# ct rules by example — a worked rule for each invariant category

> A practical companion to `ct-rules --explain` and `docs/specs/rules.md`:
> one copy-pasteable rule for every category in the catalog (`rules.md` §6),
> plus the rule *shapes* — adapters, lanes, severity, defs — that cut across
> them. Each entry says what to type and **why it earns its place in the
> store**.

A rule is recorded with `ct rules --add ID --question … -- PROBE`. `--add`
gate-validates the probe and **runs it immediately**: it is recorded only if
it holds right now, so a green store is an established truth, not a wish. The
probe is an argv vector (never a shell); it scans for known violations and a
failing probe arrives carrying its own evidence. `ct check` re-verifies the
whole store forever after.

Every probe below uses only the suite's read-only tools or the compiled-in
bridge, so all of them pass the gate. Where the *native* observer for a
category is not built yet, the example uses the supported approximation today
and names the native form on the roadmap — the same honesty `rules.md` §6
keeps.

## How to read each entry

- The **probe** is everything after `--`.
- **Why it's valuable** is the one-line case for making it a *project* rule —
  a recorded invariant the whole team and every agent inherit.
- **Today** says whether the form is native, an approximation, or bridged, so
  you never adopt a probe that can't actually enforce.

---

## The catalog, by example

### 1. Layer ordering

Dependencies between named layers flow one way — the single most-cited
architecture rule.

```sh
# Workspace-member layering: the api crate must not reach the db crate.
ct rules --add api-not-on-db \
  --question "Does the api crate stay off the db crate?" \
  --why "the api layer reaches storage only through the service layer" \
  --tag layering \
  -- deps --forbid 'my-api=>my-db'
```

```sh
# The whole ordered stack in one rule: api may use service may use db, never up.
ct rules --add layer-order \
  --question "Do the crates respect the layer order?" \
  --why "api -> service -> db is one-way; an upward edge inverts the architecture" \
  --tag layering \
  -- deps --layers my-api,my-service,my-db --layers-closed
```

```sh
# Module-level layering inside one crate: nothing in domain may import infra.
ct rules --add domain-no-infra \
  --question "Is the domain layer free of infrastructure imports?" \
  --why "domain code must not depend on infrastructure" \
  --tag layering \
  -- mods --layers 'infra*,domain*'
```

**Why it's valuable:** one-way dependencies are what keep a codebase
changeable — a violation means a lower layer has reached up and pinned an
upper one in place. The native form reports the offending dependency path as
evidence.

**Today:** workspace-member layering is native — a single boundary via
`deps --forbid`, or the whole ordered stack via `deps --layers` (with
`--layers-closed` for exhaustiveness, naming every offending member);
module-level layering inside one crate is native too — `mods --layers` over
the heuristic `use`-edge graph (ct-outline honesty class).

### 2. Cycle freedom

No dependency cycles among crates or modules — "cycles are the death of
modularization."

```sh
# One probe sweeps the whole workspace-member graph for cycles.
ct rules --add no-crate-cycles \
  --question "Is the workspace free of crate dependency cycles?" \
  --why "a cycle defeats incremental builds and local reasoning" \
  --tag cycles \
  -- deps --acyclic --members
```

```sh
# Or pin one specific boundary by forbidding its back-edge.
ct rules --add no-db-to-api-cycle \
  --question "Is the api/db boundary acyclic?" \
  --why "a db -> api edge would close a dependency cycle" \
  --tag cycles \
  -- deps --forbid 'my-db=>my-api'
```

**Why it's valuable:** a cycle defeats incremental builds and local
reasoning — you can no longer understand or rebuild one side without the
other. Each violation carries a concrete cycle path as evidence.

**Today:** native — `deps --acyclic --members` sweeps the workspace-member
graph in one probe (`--members` is what keeps it actionable: crate cycles among
third-party deps are unfixable dev-dependency noise). Module-level cycles inside
a crate are native too — `mods --acyclic` over the heuristic `use`-edge graph.

### 3. Banned symbols & APIs

Never call or use X — Rust's popular form of clippy's `disallowed-methods` /
`disallowed-types`.

```sh
# No .unwrap() anywhere under src.
ct rules --add no-unwrap-in-src \
  --question "Is src free of .unwrap() calls?" \
  --why "unwrap() panics in production; surface the error instead" \
  --tag hygiene \
  -- ct-search --base src --ext rs \
       --grep '.unwrap(' --mode literal --expect none --detail
```

**Why it's valuable:** bans a footgun API across the whole project in one
line, and the failing probe lists every offending site so the fix path is
obvious.

**Today:** native textually (`ct-search --expect none`). For references text
can't see — aliased paths, re-exports — the bridge admits `rust-analyzer
search` (search-only SSR), at the cost of a cold workspace load the rule
accepts knowingly via `--timeout`.

### 4. Banned & allowed dependencies

The crate graph must not contain X (cargo-deny `bans`; the rust-lang tidy
allowlist).

```sh
# Native: no openssl in the shipped artifact (pure-Rust TLS only).
ct rules --add no-openssl \
  --question "Is the dependency tree free of openssl?" \
  --why "musl cross-builds require pure-Rust TLS" \
  --tag deps,portability \
  -- deps --deny openssl --edges normal
```

```sh
# Bridge alternative — the matcher adapter reads cargo tree's own words.
# cargo tree -i prints "did not match any packages" when the crate is absent.
ct rules --add no-openssl-bridge \
  --question "Is openssl absent from the tree?" \
  --expect-ok 'did not match any packages' \
  -- cargo tree -i openssl
```

**Why it's valuable:** keeps a crate with the wrong license, portability cost,
or security history out of the build for good; the native form reports the
shortest dependency path that proves the violation.

**Today:** native (`deps --deny`); equivalents are the bridged `cargo tree
-i` with an `--expect-ok` matcher, or `cargo deny check bans` deferring to
your `deny.toml`.

### 5. Sibling independence

Modules or features in a set must not depend on each other — plugins, bounded
contexts (import-linter's `independence`).

```sh
# Two plugin crates must each stay off the other (--forbid is repeatable).
ct rules --add plugins-independent \
  --question "Do the plugin crates stay independent of each other?" \
  --why "plugins must compose without a sibling dependency" \
  --tag boundaries \
  -- deps --forbid 'plugin-a=>plugin-b' --forbid 'plugin-b=>plugin-a'
```

**Why it's valuable:** independence is what makes a set of features swappable
and individually testable; one sibling edge quietly turns a plugin set into a
tangle.

**Today:** native per pair via repeated `--forbid`; for a larger set, fan
`deps --forbid` across the pairs with `ct-each`.

### 6. Role–name–location coherence

Things of a role are named and placed accordingly (`*Test` naming, entities
in the domain layer, binaries in `src/bin`).

```sh
# Types belong in modules — the crate root is wiring only.
ct rules --add no-types-in-crate-root \
  --question "Is the crate root free of type declarations?" \
  --why "lib.rs wires modules together; types live inside them" \
  --tag structure \
  -- ct-outline --base src/lib.rs --kind struct,enum --expect none
```

**Why it's valuable:** when role implies location, a reader (or an agent)
can navigate by convention instead of grep — and a misplaced type is caught
the day it lands, not at the next big cleanup.

**Today:** native — `ct-outline --kind`/`--match` composes with `ct-search
--name`; defs keep the role vocabulary in one place.

### 7. Boundary leak prevention

Internal types don't surface in the public API; visibility stays tight.

```sh
# The public surface must not re-export the internal module.
ct rules --add no-internal-reexport \
  --question "Does the public API avoid re-exporting internals?" \
  --why "internal types must not leak through the crate's public surface" \
  --tag boundaries \
  -- ct-search --base src/lib.rs \
       --grep 'pub use crate::internal' --mode literal --expect none --detail
```

**Why it's valuable:** a small, deliberate public surface is what lets you
refactor internals freely; every accidental `pub use` is a future breaking
change waiting to happen.

**Today:** a text approximation (the re-export scan). Semantic `pub(crate)`
visibility reporting is a planned `ct-outline` extension, after which this
becomes exact rather than heuristic.

### 8. Dependency hygiene: duplicates & versions

No duplicate crate versions; workspace-unified versions (cargo-deny
`multiple-versions`, `cargo tree -d`).

```sh
# Native: the version list is the evidence when an assertion fails.
ct rules --add no-duplicate-deps \
  --question "Is the tree free of duplicate crate versions?" \
  --why "duplicate versions bloat builds and split trait impls" \
  --tag deps \
  -- deps --duplicates
```

```sh
# Bridge alternative — the `empty` adapter: holds iff the probe prints nothing.
ct rules --add no-duplicate-deps-bridge \
  --question "Is the tree free of duplicate crate versions?" \
  --expect empty \
  -- cargo tree -d
```

**Why it's valuable:** an accidental second version of a crate silently bloats
the binary and fractures trait coherence (two `serde::Serialize`s that aren't
the same trait) — exactly the kind of drift no one notices until it bites.

**Today:** native (`deps --duplicates`) or bridged (`cargo tree -d` with
`--expect empty`).

### 9. Supply-chain policy

License allowlist, trusted sources, no known advisories — cargo-deny's whole
domain, the clearest "leverage, don't rebuild" case.

```sh
# Defer to the project's own deny.toml; the rule gives it a question and a why.
ct rules --add supply-chain-clean \
  --question "Does the dependency set pass our deny.toml policy?" \
  --why "license allowlist, trusted sources, and bans are release gates" \
  --tag supply-chain \
  -- cargo deny check
```

```sh
# Fresh advisories need the network — a reviewed, per-rule opt-in.
ct rules --add advisories-clean \
  --question "Is the tree free of known advisories?" \
  --why "block shipping against a freshly disclosed advisory" \
  --network --timeout 60 \
  --tag supply-chain \
  -- cargo deny check advisories
```

**Why it's valuable:** policy stays exactly where it belongs (`deny.toml`),
and the rule earns it a seat in the unified `ct check` report so a license or
advisory regression fails the same gate as everything else.

**Today:** native via the bridge; hermetic by default, with `--network` the
visible opt-in where freshness matters.

### 10. Presence contracts

Every X has its Y — every file a license header, every module a test, every
public item documented.

```sh
# Every Rust source must carry an SPDX header (per-file probe via the walker).
ct rules --add license-headers \
  --question "Does every Rust source carry an SPDX header?" \
  --why "every file must declare its license" \
  --tag hygiene \
  -- ct-each --base src --name '*.rs' -- \
       ct-search --base '{ITEM}' --grep 'SPDX-License-Identifier' --quiet
```

**Why it's valuable:** "every X has its Y" is the broadest invariant family of
all, and `ct-each`'s walker item source turns it into one rule — the
violation report names each file that is missing its Y, so it doubles as a
to-do list.

**Today:** native — the `ct-each` walker for per-file presence, `ct-outline
--kind` for per-symbol presence.

---

## Rule shapes & lifecycle

The ten above all vary one underlying form. These are the knobs that form
exposes — each is its own kind of rule.

### Three ways to read a probe (outcome adapters)

Suite observers speak the exit contract (`0` holds / `1` violated / `2`
broken), so they need no adapter — every `ct-search`/`deps`/`ct-outline`/
`ct-each` example above relies on it. Bridge tools don't, so a rule supplies
one at `--add`:

- **`--expect empty`** — holds iff the probe exits `0` printing nothing (the
  `cargo tree -d` shape in §8).
- **`--expect-ok PATTERN` / `--expect-err PATTERN`** — `ct-test`-style
  matchers over the probe's output, `err` decisive over `ok` (the `cargo tree
  -i` shape in §4).

### Park an aspiration: `--pending`

Record the goal the day you decide it — before the code is clean.

```sh
ct rules --add no-unwrap-in-src --pending \
  --question "Is src free of .unwrap() calls?" \
  -- ct-search --base src --ext rs --grep '.unwrap(' --mode literal --expect none

ct rules --promote no-unwrap-in-src   # re-runs the probe; enforces once it holds
```

**Why it's valuable:** `ct check` reports a pending rule in its own lane
without failing the build, so an aspiration is visible and tracked instead of
living in someone's head; `--promote` flips it to enforced the moment it
actually holds.

### Soft enforcement: `--severity warn`

The probation ramp before a rule blocks merges.

```sh
ct rules --add no-todo-comments --severity warn \
  --question "Is src free of TODO comments?" \
  -- ct-search --base src --ext rs --grep TODO --expect none
```

**Why it's valuable:** a violated `warn` rule reports in the `WARN` lane but
never reddens the exit, so you can socialize a new rule — and let the existing
violations drain — before tightening it to `fail`.

### Shared vocabulary: defs

Name a set once; reference it everywhere as `{def:NAME}`.

```sh
ct rules --def 'banned-apis=[".unwrap(", ".expect("]'
ct rules --add no-panic-prone-calls \
  --question "Is src free of panic-prone calls?" \
  --tag hygiene \
  -- ct-each --items '{def:banned-apis}' --quiet -- \
       ct-search --base src --ext rs --grep '{ITEM}' --mode literal --expect none
```

**Why it's valuable:** the banned set lives in exactly one place — a list def
splices into `ct-each --items`, so adding a fourth banned call is a one-line
def edit, not a hunt through every rule that referenced the old three.

### Keep the request: `--prompt`

`--prompt TEXT` (or `--prompt file:PATH`) retains the verbatim human ask
behind a rule as provenance — months later it is the difference between
editing the rule confidently and guessing at intent. Verification never reads
it; `ct rules --flatten` strips every retained prompt in one pass once it has
served its purpose.

---

## Runners-up worth recording

Not top-ten by citation, but real project rules today:

- **Orphan / dead structure** — a core type no longer referenced anywhere
  signals an unfinished refactor: `ct-each --items '{def:core-types}' -- ct-search
  --base src --grep '{ITEM}' --quiet` (holds while every name is still used).
- **Test placement** — assert a convention with `ct-outline`/`ct-search`
  framing, e.g. a `tests` module present where one is expected.

Size and complexity thresholds (god modules, fan-out) are visible today with
`ct-tree --min-lines`/`--max-lines` as a *report*, but `ct-tree` carries no
verdict framing, so it is not yet a single-probe rule — treat it as a review
aid until a thresholded observer lands.

## See also

- `ct-rules --explain` — the canonical reference for the writing side
  (`docs/explain/ct-rules.md`).
- `ct-check --explain` — how the store is verified, and the five lanes.
- `docs/specs/rules.md` — the surface spec: store schema, the probe gate, the
  bridge, and the full §6 catalog these examples realize.
