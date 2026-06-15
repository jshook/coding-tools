# The ct rule surface — draft spec v2

> `.ct/` holds what a project knows about itself. `ct-rules` is how that
> knowledge is *said*; `ct-check` is how it is *verified*. Rules scan for
> known violations using the suite's read-only tools — and, where Rust
> semantics demand it, by leveraging established Rust tooling through a
> fixed, compiled-in bridge.

Status: AS BUILT (implemented 2026-06-10/11; all walkthrough decisions §9–§10
settled and shipped). The per-tool canonical references are
`docs/explain/ct-rules.md` and `docs/explain/ct-check.md`; this document
remains the cross-tool design record. Prior-art research:
`docs/specs/rules-prior-art.md`.

This spec takes strong inspiration from jQAssistant, ArchUnit-class tools,
rust-analyzer, cargo-modules, and cargo-deny — without patterning directly on
them or importing their vocabularies. Where a free Rust tool already does a
job well, a rule *leverages* it rather than ct rebuilding it.

---

## 1. Vocabulary (ours)

| Term         | Meaning |
| ------------ | ------- |
| **rule**     | One recorded, framed observation: an `id`, the `question` it answers, the probe that answers it, and the `why` behind it. |
| **probe**    | The rule's command — an argv vector (never a shell) that runs read-only and reports violations. |
| **violation**| One offending finding a probe reports. A rule holds when its probe reports zero violations and exits `0`. |
| **def**      | A named definition in the store — a set of names, paths, or a pattern — that rules reference as `{def:NAME}` instead of hardcoding lists. |
| **observer** | A read-only suite tool a probe can use (`ct-search`, `ct-outline`, `ct-tree`, `ct-view`, `ct-each`, `ct-test`). |
| **built-in check** | A crate-/module-graph assertion the rule layer runs in-process, named by the probe head `deps`/`mods`. |
| **bridge**   | The compiled-in table of permitted external Rust-tool invocations (§5). |
| **lane**     | A rule's reporting state: `holds`, `violated`, `pending`, `broken`. |

Deliberately not used: "concept", "constraint", "ban list", "contract" — the
ideas are absorbed; the terms are not.

## 2. The scan-for-violations model

A rule does not assert an opinion; it **scans for known violations**. The
probe's job is to *enumerate offenders* — file paths, symbols, crates — so a
failing rule arrives carrying its own evidence. The suite's framing makes
this natural:

```sh
# probe: list debug prints (each match is a violation); none expected
ct-search --base src --name '*.rs' --grep 'dbg!\(' --expect none --detail
```

Conventions for probe authorship (enforced by `ct-rules --add` lint, not
hard-failed):

- Prefer probes whose *output is the violation list* (`--detail`, `--flat`,
  per-item lines) over silent `--quiet` probes — `ct-check` relays a failing
  probe's output as the violation report.
- The probe exit contract is the suite's: `0` = zero violations (rule holds),
  `1` = violations found, `2` = the probe itself is broken.

## 3. The store: `.ct/rules.jsonc`

Renamed from v1's `checks.jsonc` to match the surface's name. Same `.ct/`
upward discovery (nearest `.ct`, git-style; `--file` overrides). Two
sections: `defs` and `rules`.

```jsonc
{
  "defs": {
    // Named vocabulary — rules say {def:core-types} instead of re-listing.
    // Untyped: a string expands in place; a list expands to multiple argv
    // elements (legal only where the receiving option accepts repeats).
    "core-types":   ["Parser", "Lexer", "Emitter"],
    "domain-layer": "src/domain",
    "infra-layer":  "src/infra"
  },
  "rules": [
    {
      "id": "no-debug-prints",
      "question": "Are all debug prints removed from src?",
      "probe": ["ct-search", "--base", "src", "--name", "*.rs",
                "--grep", "dbg!\\(", "--expect", "none", "--detail"],
      "why": "dbg! output leaked into the 0.1 release notes",
      "tags": ["hygiene"],
      "added": "2026-06-10"
    },
    {
      "id": "core-types-referenced",
      "question": "Is every core type still referenced?",
      "probe": ["ct-each", "--items", "{def:core-types}", "--quiet", "--",
                "ct-search", "--base", "src", "--grep", "{ITEM}", "--quiet"],
      "why": "dead core types signal an unfinished refactor",
      "tags": ["structure"]
    },
    {
      "id": "no-openssl",
      // Built-in check (§5): the crate graph, asserted in-process — no adapter.
      "question": "Is the dependency tree free of openssl?",
      "probe": ["deps", "--deny", "openssl"],
      "why": "musl cross-builds require pure-Rust TLS",
      "tags": ["deps", "portability"],
      "severity": "fail"
    }
  ]
}
```

Rule fields are v1's (`id`, `question`, `probe` [was `cmd`], `why`, `tags`,
`added`, `timeout`, `pending`) plus:

| New field    | Meaning |
| ------------ | ------- |
| `prompt`     | The verbatim human request that led to the rule, retained as provenance so intent can be revisited (the `--add` confirmation announces the retention). Never read by verification; `ct-rules --flatten` strips every prompt in one pass. |
| `severity`   | `fail` (default) or `warn`. A violated `warn` rule is reported (`WARN` lane) but never reddens the exit status — enforced vocabulary with soft consequences; the probation ramp before tightening to `fail`. |
| `expect`     | For bridge probes only (§5): how to read the external tool's outcome — `"exit"` (default, the suite contract), `"empty"` (holds iff stdout reports nothing), or a `ct-test`-style matcher object (`{"err-match": …}` / `{"ok-match": …}`, identical promotion and fail-closed precedence). Observers and the built-in checks (`deps`/`mods`) reject it — they already speak the exit contract; the store load fails if one carries an adapter. |
| `network`    | `true` permits the probe to touch the network, honored only for bridge prefixes where it means something (currently `cargo deny check`); everything else — and the default — runs hermetic (`--offline` enforced). A reviewed, per-rule decision visible in the store diff. |

`defs` expansion: `{def:NAME}` expands in probe argv elements before gate
validation and before `{ITEM}`/`{INDEX}` expansion. Defs are **untyped**: a
string expands in place; a list expands to multiple argv elements (legal only
where the receiving option accepts repeats, e.g. `ct-each --items`) — the
receiving tool gives the value meaning, and the verify-on-add run catches
misuse. Unknown def → the rule is `broken`.

## 4. The two tools

**`ct-rules` — say what the rules are.** `--add` (verify-then-record; strict
unless `--pending`; `--prompt` retains the originating request prose, and the
confirmation announces the retention), `--promote` (re-verify, clear
pending), `--remove`, `--list`, `--def NAME=...` (manage defs), `--flatten`
(strip every retained prompt, leaving the mechanical definitions),
`--hook cargo` (§7), `--init` (scaffold a commented starter store — the
cargo-deny idea). Writes the store; on no gate, ever. Store writes keep the
file human-friendly: a standing header comment (re-established if lost), one
field per line per rule, blank lines between rules.

A **bare probe with no verb** (`ct rules -- <probe>`) runs it and reports the
outcome *without saving* — prototype a check, then re-run it with `--add ID
--question …` once it holds. Exit status follows the outcome (`0`/`1`/`2`), so
a prototype composes in `&&`/`||`. Any gated probe prototypes this way,
built-in checks included.

**`ct-check` — verify them.** Pure read-only runner: store order, sequential,
independent; selection by `--id`/`--tag`; lanes `holds`/`violated`/`pending`/
`broken`; any `broken` rule ⇒ exit `2`; violated `fail` rule ⇒ exit `1`;
otherwise `0`. `--json` and emit templates as in v1. On the read-only
allowlist (self-recursion via a rule rejected). A violated rule's report =
its `question`, its `why`, and the probe's own violation output — diagnostics
always name the governing rule, so the suppression/fix path is self-teaching.

## 5. The bridge: leveraging Rust tooling

Some invariant classes need facts the suite's observers don't carry. Two come
from **built-in checks** — `deps` (the resolved crate graph) and `mods` (the
intra-crate module graph) — reserved probe heads the rule layer runs
**in-process**: `deps` shells one hermetic `cargo metadata` internally, `mods`
parses `use` edges. They classify their own outcome, take no `expect` adapter,
and are prototyped and recorded exactly like any other probe (§8). The rest —
semantic symbol references, supply-chain policy, raw dependency paths — come
from external tools through the **bridge**: a **compiled-in, immutable table of
argv prefixes** naming known read-only invocations of specific external tools.

The initial bridge (settled; each entry is an exact prefix + enforced flags):

| Prefix                        | Facts unlocked | Enforced |
| ----------------------------- | -------------- | -------- |
| `cargo metadata`              | resolved crate graph, features, licenses, sources | `--locked --offline` appended; `--format-version 1` |
| `cargo tree`                  | dependency paths, inverted deps, duplicates | `--locked` appended; `--offline` unless the rule sets `network: true` (not currently meaningful here) |
| `cargo deny check`            | advisories / licenses / bans / sources policy (the project's own `deny.toml` remains the policy file — ct does not rebuild this) | `--offline` unless the rule sets `network: true` |
| `rust-analyzer search` (search-only SSR) and `rust-analyzer symbols` | semantic, AST-aware queries: resolved references, structural patterns | search-only forms; replace mode never |

Deliberately excluded for now: `cargo clippy` / `cargo check` (compile the
workspace) and `cargo modules` (would bless a headless rust-analyzer boot
before ct's own lean module observer is weighed).

Properties that keep this inside the suite's safety posture:

- The table is **compiled in** — like every gate, nothing a caller or a store
  entry does at run time can extend it. A store entry *selects from* the
  bridge; it cannot add to it. (This is what distinguishes the bridge from
  the rejected trust-tier file.)
- Prefix-matched, not name-matched: `cargo tree` is permitted; `cargo
  publish` is not a gate miss but a different, unlisted prefix — refused.
- Absent tools degrade **loudly**: a bridge probe whose binary is missing is
  `broken` (exit `2` path), never silently skipped.
- Bridge probes don't speak the suite's exit contract, so the rule's
  `expect` adapter interprets them (e.g. `cargo tree -i openssl` errors "did
  not match any packages" when openssl is absent → `--expect-ok 'did not
  match any packages'`, stored `{"ok-match": …}`, maps that to
  holds/violated). Adapters are small, named, and compiled per prefix. For
  the openssl question itself the built-in `deps --deny openssl` is the
  simpler native form — no bridge, no adapter.

The bridge is for *leverage*, not identity: ct never wraps a tool just to
rename it. cargo-deny stays configured by `deny.toml` and merely gets a seat
in the rule report; rust-analyzer is used for what only it can do (resolved
references), at the price (cold workspace load) the rule author accepts
knowingly via per-rule `timeout`.

Never compiled in: `ra_ap_*` crates (weekly `0.0.x` breaking releases;
cargo-modules pins `=0.0.328` as the cautionary exhibit). The pure-Rust,
lean, cross-compile posture stands.

## 6. The rule catalog: top 10 architectural & symbolic categories

Cross-referenced from public discussion of ArchUnit, import-linter, deptrac,
jQAssistant, clippy configuration, and cargo-deny usage (sources in
`rules-prior-art.md` and the research notes). Ordered roughly by how often
practitioners cite them. Each entry: what it asserts → how ct expresses it
(today / with the `deps`/`mods` built-in checks / via bridge).

1. **Layer ordering** — dependencies between named layers flow one way
   (domain ← application ← infrastructure; controller → service →
   repository). The single most-cited architecture rule (ArchUnit's layered
   architecture, import-linter's `layers` contract, deptrac's core model).
   *ct:* workspace-member layering via `deps` over cargo metadata;
   module-level layering via `mods --layers` (the heuristic use-edge graph);
   directory-level approximations also possible with `ct-search --base {def:layer} --grep`.

2. **Cycle freedom** — no dependency cycles among modules/crates ("cycles
   are the death of modularization"). ArchUnit slices, cargo-modules
   `--acyclic`, jQAssistant package cycles.
   *ct:* crate-level via `deps --acyclic`; module-level via `mods
   --acyclic` (the heuristic use-edge graph), no rust-analyzer required.

3. **Banned symbols & APIs** — never call/use X (`java.util.Date`,
   `System.out`; in Rust: `unwrap()` in lib code, `std::fs` over `fs_err`,
   `println!` in libraries). Clippy's `disallowed-methods`/`disallowed-types`
   is the popular Rust form.
   *ct:* today, textually (`ct-search --expect none`); semantically via
   bridge (`rust-analyzer search` resolves aliased paths text can't).

4. **Banned & allowed dependencies** — the crate graph must not contain X
   (or: only allowlisted crates). cargo-deny `bans`; the rust-lang repo's
   tidy keeps an explicit dependency allowlist.
   *ct:* the built-in `deps --deny X` (the native form); or bridge — `cargo
   tree -i X` with `--expect-ok 'did not match any packages'`, or `cargo deny
   check bans`.

5. **Sibling independence** — modules/features in a set must not depend on
   each other (plugins, bounded contexts). import-linter's `independence`
   contract; ArchUnit slices.
   *ct:* `ct-each --items-def {def:plugins}` fanning a cross-reference probe;
   exactly via `deps` for workspace members.

6. **Role–name–location coherence** — things of a role are named and placed
   accordingly (`*Test` naming, entities in the domain layer, `ct-*` binaries
   in `src/bin`). ArchUnit naming/annotation conventions; jQAssistant's most
   quoted examples.
   *ct:* today — `ct-outline --kind --match` + `ct-search --name` compose
   well for this; defs keep role vocabularies in one place.

7. **Boundary leak prevention** — internal types don't surface in the public
   API; only designated modules are exported; visibility stays tight
   (`pub(crate)` hygiene). ArchUnit's "no JPA entities out of controllers";
   cargo-modules' visibility view; cargo-check-external-types.
   *ct:* heuristically today (`ct-outline` visibility once the Rust pack
   reports it — small planned extension); semantically via bridge.

8. **Dependency hygiene: duplicates & versions** — no duplicate crate
   versions; workspace-unified dependency versions. cargo-deny
   `multiple-versions` (noisy but universally enabled), `cargo tree -d`.
   *ct:* bridge (`cargo tree -d` + `expect: empty`); natively in `deps`.

9. **Supply-chain policy** — license allowlist, trusted sources, no known
   advisories. cargo-deny's whole domain; the clearest "leverage, don't
   rebuild" case.
   *ct:* bridge (`cargo deny check`), policy stays in `deny.toml`; the rule
   contributes the `question`/`why` and a seat in the unified report.

10. **Presence contracts** — every X has its Y: every public item documented,
    every module a test module, every error type a `std::error::Error` impl,
    every source file a license header. jQAssistant "every aggregate has a
    test"; ArchUnit annotation checks; Rust's `missing_docs`.
    *ct:* the `ct-each` walker-item-source pattern is purpose-built for this
    (per-file probes); `ct-outline --kind` covers per-symbol presence.

Runners-up (worth recording, not top-10 by citation): **orphan/dead
structure** (unlinked files — cargo-modules `orphans`; unused deps —
cargo-udeps/machete territory), **size & complexity thresholds** (god
modules, fan-out — `ct-tree --max-lines` expresses these natively today),
**test placement** conventions.

The catalog is descriptive, not a schema: every category lands as ordinary
rules + defs, not as special-cased machinery. That is the lean bet — the
categories live in the store as recorded vocabulary, not in the binary.

A worked, copy-pasteable `ct rules --add` for each category here — with the
native/approximation/bridge status spelled out and the case for adopting each
as a project rule — is in `docs/specs/rules-examples.md`.

## 7. Cargo hook (carried from v1, unchanged in substance)

`ct rules --hook cargo` writes a `tests/ct_invariants.rs` shim that runs
`ct check --quiet` from `CARGO_MANIFEST_DIR` and fails the test on a
non-zero exit; degrades loudly when `ct` is absent from `PATH`. Possible
companion: a `cargo-ct` external-subcommand shim (`cargo ct check`).

## 8. Fact-layer roadmap implied by the catalog

`deps`/`mods` ship as **built-in checks** — probe heads the rule layer runs
in-process (`ct rules … -- deps …` / `-- mods …`; verified by `ct check`), not
top-level tools; the remaining entries extend existing observers.

| Fact source | Facts | Status |
| ----------- | ----- | ------ |
| `deps` (built-in check) | crate graph from `cargo metadata --locked --offline`: `--deny NAME`, `--forbid 'A=>B'` (workspace layering), `--layers` (ordered stack, `--layers-closed` for exhaustiveness), `--acyclic` (`--members` for the actionable scope), `--duplicates`, `--edges` kind filtering — every violation with an evidence path | **shipped** |
| `ct-outline` visibility | report `pub`/`pub(crate)`/private on Rust entries (small extension to the existing pack) | small extension |
| `mods` (built-in check) | heuristic `use`-statement module graph (ct-outline honesty class): `--forbid 'A=>B'`, `--acyclic`, `--layers` at module granularity, without rust-analyzer | **shipped** |

## 9. Carried decisions (settled in the v1 walkthrough)

1. Upward discovery to the nearest `.ct`; `--file` overrides.
2. Strict `--add`; `--pending` lane for aspirations; explicit `--promote`;
   runs are pure (never write).
3. `broken` is its own lane; any broken rule ⇒ run exits `2`.
4. `ct-each` admissible in probes; `--mutating` rejected at add and load.
5. `ct-each` gains the walker item source (`--base`/`--name`/`--ext`).
6. Store order, sequential, independent; no parallelism in v1.
7. `ct-check` joins the read-only allowlist; `ct-rules` on no gate;
   no self-recursion through the store.

## 10. Settled in the v2 walkthrough (2026-06-10)

1. **Bridge contents**: the four prefixes in §5 (cargo metadata, cargo tree,
   cargo deny check, rust-analyzer search/symbols search-only); clippy/check/
   cargo-modules excluded for now.
2. **`severity: warn`**: adopted in v1 — `WARN` is a fifth lane; violated
   warn rules report but never redden the exit.
3. **Network posture**: hermetic by default (`--offline` enforced on bridge
   probes); per-rule `network: true` opt-in, honored only where meaningful
   (cargo deny), visible in the store diff.
4. **`expect` adapters**: `exit` (default) + `empty` + the `ct-test` matcher
   pair with identical promotion and fail-closed precedence.
5. **defs**: untyped — string (in-place) or list (multi-element where repeats
   are legal); the verify-on-add run is the validator.
6. **Baseline**: deferred — `pending` + `warn` are the adoption ramp; revisit
   only if they prove insufficient on a legacy codebase. (If revisited, heed
   prior art's rotting-skip-list failure mode.)

No open questions remain in this draft.
