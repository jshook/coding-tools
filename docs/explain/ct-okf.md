# ct-okf

Author, query, and **index** **Open Knowledge Format** (OKF v0.1) bundles. An
OKF *bundle* is a directory tree of Markdown *concept* files; each concept opens
with a YAML **frontmatter** block (fenced by `---`) carrying a required `type`
plus optional `title`, `description`, `resource`, `tags`, and `timestamp`.
Reserved `index.md` (a directory listing) and `log.md` (a change history) have
defined roles and are never concepts. Cross-links are ordinary Markdown links,
either bundle-relative (`/tables/customers.md`) or document-relative.

Reachable as `ct okf <verb> …` or `ct-okf <verb> …`. Unlike the other suite
tools, `ct-okf` is **subcommand-shaped** because its surface spans querying,
configuration, checking, and authoring. The authoring verbs **write files** —
which is why `ct-okf`, like `ct-rules`, is not on the read-only allowlist. For
read-only OKF composition inside `ct-test`/`ct-each`/`ct-rules`, use the
OKF-aware `ct-search`/`ct-tree`/`ct-view`/`ct-outline` or the `okf` built-in
check.

## Content roots and the index

`ct-okf` operates over a project's **content roots** — the directories it treats
as OKF bundles. A directory is a root if **any** of these holds (they all
converge on the project config, so adopt whichever is convenient):

1. it has a `.okf` **marker file** (our convention; the file may be empty);
2. it has a bundle-root `index.md` declaring `okf_version` (the OKF-standard
   root signal);
3. it is listed in the project config `.ct/okf.jsonc` (managed by `roots` /
   `init`).

The project root is the nearest ancestor of `--base` containing `.ct` (the same
discovery `ct-rules`/`ct-check` use). A **lazily-maintained full-text index**
lives under `.ct/okf/` as immutable `fst` segments: each update layers a new
segment (existing ones are never rewritten), changed/removed docs are
tombstoned, and `index condense` merges segments and drops tombstones.

Only files accepted by the compiled-in `okf-markdown` provider enter this
index. `.ct/index.jsonc` may narrow or override its include and exclude scopes;
unknown formats, databases, binary state, reserved bundle files, oversized
files, and the index's own storage are not fed to a generic tokenizer. A native
filesystem watcher opportunistically maintains the index between queries.
`search` uses a bounded freshness barrier when the watcher is healthy and falls
back to complete synchronous reconciliation otherwise, so the daemon is never a
correctness dependency. The detached daemon exits after its configured idle
period (one hour by default), handles POSIX termination and Windows console
control events through a status-marking shutdown path, and logs only start/stop
lifecycle records to a 32 KiB, two-generation rotating log under
`.ct/okf/runtime/`. A per-project OS lock enforces one daemon even across crashes.
Status reports daemon RSS and its effective memory ceiling; absent an explicit
`max_daemon_memory_bytes`, that ceiling is the smaller of 2 GiB and five percent
of physical RAM. See `docs/specs/indexing.md`.

## Global options

These apply across verbs (they may appear before or after the subcommand):

- `--base DIR` (default `.`) — the bundle for bundle-scoped verbs
  (`validate`/`links`/`find`/`show`), and the directory project discovery starts
  from for `search`/`index`/`roots`/`init`.
- `--name PATTERN` (`|`-separated, substring→glob→regex promoted, anchored),
  `--hidden`, `--follow`, `--no-ignore` — the suite's shared walker vocabulary.
- `--json` / `--json-pretty` — emit a structured result (pretty indents it).
- `--quiet` — suppress informational output (exit status and `--emit` still
  report).
- `--timeout SECS` and the heartbeat options `--heartbeat SECS` /
  `--heartbeat-emit` / `--heartbeat-to`.
- `--explain [md|json]` prints this document or the MCP tool-use definition.

## Query verbs

- `search QUERY…` — full-text search across the content roots, **auto-updating
  the index first**. Query grammar (per whitespace token): `term` (exact),
  `term*` (prefix), `term~` / `term~N` (Levenshtein fuzzy, N≤2), and `/regex/`
  (a regex over the term dictionary, e.g. `/.*schema.*/` for substring). Cap
  with `--limit N` (default 20) and filter with `--type TYPE` / `--tag T,…`.
  Ranked by tf-idf; prints `score  path  [type]  title`.
- `find` — list the `--base` bundle's concepts by metadata. Filter by `--type`
  (exact) and/or `--tag` (concepts carrying *all* given tags). Text or `--json`.

## Configuration verbs

- `roots list` — show the configured/detected roots and how each was found.
- `roots add DIR [--marker]` — register `DIR` in `.ct/okf.jsonc` (and drop a
  `.okf` marker with `--marker`).
- `roots rm DIR` — unregister a root.
- `roots scan [--write]` — discover candidate roots by scanning for OKF
  concepts; `--write` records them and drops markers.
- `index status` — report docs, segments, tombstones, and pending changes.
- `index scopes [--effective]` — show provider-whitelisted include/exclude
  scopes and whether each came from detection or `.ct/index.jsonc`.
- `index why PATH` — explain the effective include/exclude/provider decision
  for one file.
- `index init [--dry-run|--write]` — preview the conservative derived policy,
  or materialize it as `.ct/index.jsonc`.
- `index watch status|start|stop` — inspect or control the opportunistic
  per-project watcher. Indexed queries start it automatically when enabled.
- `index update` — reconcile the index against the roots now.
- `index condense` — merge segments and drop tombstones.
- `index rebuild` — discard and rebuild from scratch.
- `init [--marker]` — onboarding: discover roots, record them in the config
  (optionally writing markers), and build the initial index in one step.

## Check verbs (framed verdict)

- `validate` — judge the `--base` bundle's conformance: every non-reserved `.md`
  must have parseable frontmatter with a non-empty `type`. With `--strict`, a
  broken bundle-relative link also counts as a violation.
- `links` — report broken bundle cross-links.

Both pose their work as a pass/fail test: `--question` prints a `== … ==`
banner, `--expect` classifies the **violation count** (`any`/`none`/`N`/`=N`/
`+N`/`-N`, default `none`), and `--emit` / `--emit-stderr` expand a template
after the check. Tokens: `{RESULT}` `{QUESTION}` `{COUNT}` `{TOTAL}` `{BASE}`
`{MATCHES}`. Exit status follows the verdict: `0` SUCCESS, `1` ERROR, `2`
usage/runtime error.

## Authoring verbs (these write)

- `show PATH` — print one concept's frontmatter (text, or `--json`).
- `add PATH --type TYPE [--title T] [--description D] [--tag T,…]` — scaffold a
  concept (stamps a `timestamp`, refuses to overwrite). Alias: `new`.
- `mv SRC DST` — move/rename a concept, rewriting every bundle cross-link
  (absolute and document-relative) that pointed at it. Alias: `rename`.
- `set FIELD=VALUE --file PATH` — set or update a scalar frontmatter field,
  preserving the rest of the file byte-for-byte.
- `log MESSAGE [--kind LABEL]` — prepend a dated entry to the bundle's `log.md`
  (default label `Update`; same-day entries merge under one heading).
- `gen-index [--scaffold]` — (re)generate `--base`'s `index.md` from the
  immediate concepts' `title`/`description`; `--scaffold` instead writes an
  absent `index.md` declaring `okf_version: "0.1"`.
- `script PATH [--dry-run] [--fence STR]` — run a `.ctb` block document of
  `new`/`set`/`log`/`index`/`init` items as one **atomic** batch: the whole
  script is simulated in memory (*cascading*, so a later op sees an earlier op's
  writes) and **nothing is written unless every op succeeds**. `--dry-run`
  prints the plan and writes nothing; `--fence STR` changes the directive prefix
  (default `#%`).

## Examples

```sh
# Onboard a project, then search its knowledge (index auto-updates).
ct okf init
ct okf search "customer dimension" --type "BigQuery Table"
ct okf search "schmea~"            # fuzzy: tolerates the typo
ct okf search "/.*orders.*/"       # regex/substring over the term dictionary

# Manage roots and the index explicitly.
ct okf roots add docs/kb --marker
ct okf index status
ct okf index scopes --effective
ct okf index why docs/kb/state.sqlite
ct okf index init --dry-run
ct okf index watch status
ct okf index condense

# Conformance as a test (exit 0 when every concept conforms).
ct okf --base bundle validate --question "OKF-conformant?"

# Author: scaffold a concept, move it (fixing links), record the change.
ct okf add bundle/tables/orders.md --type "BigQuery Table" --title Orders --tag core
ct okf mv bundle/tables/orders.md bundle/facts/orders.md
ct okf --base bundle log "Moved orders to facts/" --kind Update

# Atomic batch.
ct okf --base bundle script batch.ctb --dry-run   # preview
ct okf --base bundle script batch.ctb             # apply atomically
```

- **Judge a bundle's OKF conformance with a framed pass/fail verdict.**
  ```sh
  ct okf validate --base docs/concepts
  ```
- **Report broken cross-links within an OKF bundle.**
  ```sh
  ct okf links --base docs/concepts
  ```
