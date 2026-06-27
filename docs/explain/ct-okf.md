# ct-okf

Author and query **Open Knowledge Format** (OKF v0.1) bundles. An OKF *bundle* is
a directory tree of Markdown *concept* files; each concept opens with a YAML
**frontmatter** block (fenced by `---`) carrying a required `type` plus optional
`title`, `description`, `resource`, `tags`, and `timestamp`. Reserved `index.md`
(a directory listing) and `log.md` (a change history) have defined roles and are
never concepts. Cross-links are ordinary Markdown links, either bundle-relative
(`/tables/customers.md`) or document-relative.

Reachable as `ct okf …` or `ct-okf …`. Pick **exactly one verb** (the default is
`--validate`). The check verbs are *framed verdicts*; the authoring verbs **write
files** — which is why `ct-okf`, like `ct-rules`, is not on the read-only
allowlist. For read-only OKF composition inside `ct-test`/`ct-each`/`ct-rules`,
use the OKF-aware `ct-search`/`ct-tree`/`ct-view`/`ct-outline` or the `okf`
built-in check.

## Selection

All verbs that scan a bundle target it with the suite's shared walker vocabulary:
`--base DIR` (default `.`), `--name PATTERN` (`|`-separated, substring→glob→regex
promoted, anchored), `--hidden`, `--follow`, and `--no-ignore`. Only `.md` files
are considered.

## Read-only verbs

- `--validate` — judge the bundle's conformance. Every non-reserved `.md` must
  have parseable frontmatter with a non-empty `type`; reserved files need only
  have parseable frontmatter if any is present. Counts violations and frames a
  verdict (default `--expect none`). With `--strict`, a broken bundle-relative
  link also counts as a violation.
- `--list` — list concepts with their metadata. Filter by `--type TYPE` (exact)
  and/or `--tag` (concepts carrying *all* given tags). Text by default; richer as
  `--json`.
- `--show PATH` — print one concept's frontmatter (text, or `--json`).
- `--links` — report broken bundle cross-links as a verdict (count of broken
  links; default `--expect none`).

## Authoring verbs (these write)

- `--new PATH` — scaffold a concept. Requires `--type`; takes `--title`,
  `--description`, and `--tag`. Stamps a `timestamp` and refuses to overwrite an
  existing file.
- `--init` — scaffold a bundle-root `index.md` declaring `okf_version: "0.1"` if
  one is absent.
- `--index` — (re)generate `index.md` for `--base` from the immediate concepts'
  `title`/`description` frontmatter.
- `--log MESSAGE` — prepend a dated entry to the bundle's `log.md`, labelled by
  `--log-kind` (default `Update`); same-day entries merge under one heading.
- `--set FIELD=VALUE` — set or update a scalar frontmatter field on the `--file`
  concept, preserving the rest of the file byte-for-byte.

## Atomic batches (`--script`)

`--script PATH` runs a `.ctb` block document of `new`/`set`/`log`/`index`/`init`
items as one **atomic** batch under the suite's prepare/confirm/write standard:
the whole script is simulated in memory — *cascading*, so a later `index` sees a
concept an earlier `new` created and a later `set` edits one — and **nothing is
written unless every op succeeds**. One failing op (a clobbering `new`, a `set`
on a missing concept, a bad attribute) aborts the batch with zero writes, exit
`2`. `--dry-run` prints the plan and writes nothing; `--fence STR` changes the
directive prefix (default `#%`) when a payload would otherwise look like a fence.

Each item opens with a `#% <verb>` line carrying `key=value` attributes; verbatim
text (a description, tag list, log message, or body) goes in named payload
sections. The verbs and their vocabulary:

| Verb    | Attributes              | Sections                  |
| ------- | ----------------------- | ------------------------- |
| `new`   | `file=` (req), `type=` (req), `title=` | `description`, `tags` (one per line), `body` |
| `set`   | `file=` (req), `field=` (req), `value=` (req) | — |
| `log`   | `kind=`, `base=`        | `message` (req)           |
| `index` | `base=`                 | —                         |
| `init`  | `base=`                 | —                         |

`file=`/`base=` are resolved relative to `--base`. Example:

```text
#% new file=tables/customers.md type="BigQuery Table" title=Customers
#% description
The customers dimension.
#% tags
core
pii
#% index base=tables
#% set file=tables/customers.md field=resource value=bq://proj.ds.customers
#% log kind=Creation
#% message
scaffolded the customers table
```

```sh
ct okf --base bundle --script batch.ctb --dry-run   # preview
ct okf --base bundle --script batch.ctb             # apply atomically
```

## Framed verdict (check verbs)

`--validate` and `--links` pose their work as a pass/fail test: `--question`
prints a `== … ==` banner, `--expect` classifies the **violation count**
(`any`/`none`/`N`/`=N`/`+N`/`-N`, default `none`), and `--emit` / `--emit-stderr`
expand a template after the check. Tokens: `{RESULT}` `{QUESTION}` `{COUNT}`
(violations) `{TOTAL}` (concepts) `{BASE}` `{MATCHES}`. Exit status follows the
verdict: `0` SUCCESS, `1` ERROR, `2` usage/runtime error.

## Output and bounds

`--quiet` suppresses informational output (the exit status, and `--emit`, still
report). `--json` emits a structured result and `--json-pretty` indents it; the
object's shape depends on the verb (always carrying `tool`, `verb`, and — for
checks — `verdict`). Every run honours `--timeout SECS` and the standard
heartbeat options `--heartbeat SECS` / `--heartbeat-emit` / `--heartbeat-to`.
`--explain [md|json]` prints this document or the MCP tool-use definition.

## Examples

```sh
# Conformance as a test (exit 0 when every concept conforms).
ct okf --base bundle --validate --question "OKF-conformant?"

# Query metadata: all BigQuery tables tagged 'pii', as JSON.
ct okf --base bundle --list --type "BigQuery Table" --tag pii --json

# Inspect one concept's frontmatter.
ct okf --show bundle/tables/customers.md --json

# Audit cross-links; pass only when none are broken.
ct okf --base bundle --links

# Author: scaffold a concept, refresh the index, record the change.
ct okf --new bundle/tables/orders.md --type "BigQuery Table" --title Orders --tag core
ct okf --base bundle/tables --index
ct okf --base bundle --log "Added orders table" --log-kind Creation

# Update a single frontmatter field in place.
ct okf --set timestamp=2026-06-27 --file bundle/tables/orders.md
```
