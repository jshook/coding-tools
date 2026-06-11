# Prior art for ct-rules / ct-check (Rust targeting)

Research notes, 2026-06-10. Capability pictures of the tools ct-rules is a
"lean combination" of, and what each implies for the design. Conceptual models
are stable; currency claims verified against live sources where marked.

## The three-layer decomposition

Every tool below splits into the same layers, which is the frame for deciding
what "lean" keeps:

1. **Fact layer** — what can be observed (text, syntax, module graph, crate
   graph, advisory/license data, semantic name resolution).
2. **Rule language** — how rules are said (full query language ↔ closed
   config schema ↔ argv probes).
3. **Enforcement layer** — severity, lanes, baselines, build hooks, report
   formats, exit codes.

---

## jQAssistant (v2.9.0, Jan 2026 — active, BUSCHMAIS-backed)

Architecture: scan → labeled property graph in embedded Neo4j → Cypher
queries → report. Scanner plugins cover bytecode, Maven poms, XML/YAML/JSON,
Git history, framework specifics. Maven-plugin/CLI enforcement; fails builds
at a severity threshold.

**The rules model (its essential contribution):**
- **Concepts** enrich the graph (label classes matching `..service..` as
  `:Service`; aggregate type-level deps into package-level `DEPENDS_ON`).
- **Constraints** are queries that must return zero rows; each returned row
  *is* a violation record (columns = the report).
- Rules declare `requiresConcepts`; the engine topologically orders the DAG,
  so constraints are written one abstraction up from raw syntax. 2.9 added
  rule overriding and abstract concepts.
- Severity ladder (`info`→`blocker`) with `warnOnSeverity`/`failOnSeverity`
  thresholds; **baseline files** so only *new* violations fail (legacy
  adoption); named groups with per-include severity override.
- Living documentation: rules carry ids/descriptions/rationale; reports and
  diagrams are generated from the same queries ("architecture docs that break
  the build when they lie").

What people express: layering/boundary rules, dependency cycles, naming
conventions, forbidden APIs, metrics thresholds, process rules (commit
conventions, test-per-aggregate).

**Steal:** concept/constraint split (→ named selectors/defs); zero-rows=pass
with rows-as-violation-records; severity thresholds + baseline; rules carry
their rationale.
**Don't replicate:** persistent graph DB, runtime plugin ecosystem, full
general-purpose query language.

---

## rust-analyzer (active; `ra_ap_*` crates auto-published ~weekly)

Semantic layer for Rust: full name resolution (modules, re-exports, macros
incl. proc-macros), HIR type inference, trait/method resolution. Exact where
grep cannot be: find-all-references across renames/aliases/`pub use`,
trait-impl enumeration, type-of queries. Diverges from rustc in corners
(const generics, cfg'd-out code); its diagnostics are not a rustc substitute.

**Programmatic surfaces:**
- CLI subcommands (`symbols`, `diagnostics`, `ssr`/`search`, `analysis-stats`,
  `scip`/`lsif` index emission, `unresolved-references`) — maintained for RA
  developers, output formats not stable contracts. SCIP emission is the most
  legitimate batch surface (documented interchange format).
- SSR: `pattern ==>> template` with `$placeholders`; matching is
  semantically resolved (a path matches regardless of import alias);
  search-only mode usable as a rule predicate.
- `ra_ap_*` library crates: the whole workspace published in lockstep at
  `0.0.NNN` — every release semver-breaking, no stability promise. Consumers
  (e.g. cargo-modules, pinned `=0.0.328`) pin exact versions and bump in
  batches. Heavy: hundreds of crates, large binaries, cold full-workspace
  load per run (seconds→minutes, GBs on big workspaces), no persistent
  cross-run cache.
- Ecosystem signal: third-party static tooling more often builds on
  **rustdoc JSON** (cargo-semver-checks) or `cargo check --message-format=json`
  than on ra_ap, precisely because of the churn.

**Implication for ct:** do not link ra_ap into the suite (violates the
pure-Rust/lean/musl posture and the maintenance budget). If semantic probes
are wanted, shell out to an installed `rust-analyzer` binary (`symbols`,
`search`/SSR search-only, `scip`) as an *optional* observer that degrades
loudly when absent.

---

## cargo-modules (v0.26.0 — active)

Subcommands: `structure` (text tree of modules + items with visibility),
`dependencies` (DOT digraph of internal "owns"/"uses" edges; `--focus-on`,
`--max-depth`, edge-kind toggles), `orphans` (source files not linked by any
`mod`). **`--acyclic` turns the graph view into a CI gate**: nonzero exit on
module-dependency cycles.

Backend: entirely `ra_ap_*` (verified in Cargo.toml) — it boots a headless
rust-analyzer to get real name resolution. Consequence: accurate but heavy,
and chained to ra_ap churn. Output is text/DOT only (no JSON).

**Steal:** the `--acyclic` pattern (every view should have a pass/fail
exit-code form); orphan detection as an invariant; focus/max-depth scoping.
**Gap it exposes:** module-level dependency facts ("module A must not use
module B") currently require the heavy backend; a `use`-statement heuristic
observer is the lean, ct-outline-honesty-class alternative.

---

## cargo-deps → cargo tree / cargo-depgraph / cargo metadata

- `cargo-deps` is abandoned (~2019); ignore.
- `cargo tree` (built into cargo): inverted deps (`-i`), duplicate-version
  detection (`-d`), edge-kind filtering (`-e normal|build|dev`), `--format`,
  `--depth`. No invariant semantics in exit codes.
- `cargo-depgraph`: maintained DOT renderer over cargo metadata.
- **The substrate is `cargo metadata --format-version 1`**: the full resolved
  package graph (packages, resolve nodes, dep kinds, activated features,
  licenses, sources) as stable JSON, zero extra installs. Everything the
  graph tools show derives from it.

Crate-level invariants expressible from metadata alone: no-dep-on-X (per
edge-kind), workspace-member layering (A must not depend on B), duplicate
version bans, max depth/count, feature invariants ("no default features on
Y"), license/source fields. Not expressible: module-level facts, "is the dep
actually used".

**Implication for ct:** one new read-only observer over `cargo metadata`
unlocks the entire crate-graph invariant class cheaply and exactly — the
highest value-per-line addition to the fact layer.

---

## cargo-deny (v0.19.8, May 2026 — active, de-facto standard)

Four checks over the cargo-metadata graph (via Embark's `krates`):
- `advisories` (RustSec DB: vulns/unmaintained/yanked, `ignore` w/ reasons),
- `licenses` (allow-list-first SPDX expression evaluation,
  `confidence-threshold` for license-text classification, per-crate
  `exceptions`, `[[licenses.clarify]]`),
- `bans` (deny/allow crates, duplicate versions, feature constraints; scoped
  escapes: `skip`, `skip-tree`, `wrappers` = banned crate allowed only under
  named parents),
- `sources` (registry/git allow-lists).

Model: **policy as one reviewed `deny.toml` in the repo**; tri-state
deny/warn/allow on most knobs; diagnostics point at the exact config line
that governs them; human + line-delimited-JSON output from one diagnostic
model; exit-code-is-the-API; fast (one metadata pass, seconds).

Known pains: config-key churn across versions breaks upgrades; duplicate
check breeds rotting `skip` lists; advisory updates redden CI without code
changes; ignores lack expiry.

**Steal:** scoped exceptions with structure and `reason` fields (never global
mutes); diagnostics that name the governing rule/config line; `init`
scaffolding a commented default store; one diagnostic model rendering both
human and JSON.
**Don't replicate:** bundled advisory/license-classification machinery —
that's cargo-deny's job; a ct rule can simply *wrap* `cargo deny check` if a
project wants it (subject to gate policy).

---

## Composite: what "lean combination" means for ct-rules

| Layer | Heavy original | Lean ct equivalent |
| ----- | -------------- | ------------------ |
| Facts | Neo4j graph / headless rust-analyzer | Read-only gated observers, added incrementally: text (`ct-search`), structure (`ct-outline`), crate graph (**new: cargo-metadata observer**), module-use edges (**candidate: heuristic observer**), semantics (**optional shell-out to rust-analyzer binary**) |
| Rule language | Cypher / closed TOML schema | Argv probes over the observers (rule language = tool language); **named defs** as the lean concept mechanism |
| Enforcement | Maven plugin, severity ladders, baselines | Exit-code contract, SUCCESS/ERROR/PENDING/BROKEN lanes, cargo test-shim hook, `why`-carrying diagnostics |

Decisions this research supports:
1. Argv-probe rules stay (vs. a query language) — jQAssistant's weight lives
   in its query engine; its *value* (concepts, zero-rows, rationale-carrying
   rules) ports without it.
2. Named selectors/defs (`{def:...}` expansion in rule argvs) = the lean
   concept/constraint split. Worth adding to the spec.
3. A cargo-metadata observer is the next fact-layer tool; module-use edges
   the one after (heuristic class); ra_ap never compiled in, optionally
   shelled out.
4. Severity/lane refinements worth weighing: per-rule severity or
   warn-vs-fail threshold; baseline ("only new violations") as a possible
   later lane; exception entries with mandatory `reason`.
5. Wrap-don't-rebuild for supply-chain: cargo-deny exists and is excellent;
   the only question is gate policy for invoking it from a rule.

Sources: github.com/regexident/cargo-modules (+ Cargo.toml),
github.com/EmbarkStudios/cargo-deny, github.com/jQAssistant/jqassistant
releases via search, sdkman.io/sdks/jqassistant, rust-analyzer
docs/contributing CLI docs (agent-reported, partially unverified),
mvnrepository.com/artifact/com.buschmais.jqassistant.
