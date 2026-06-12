# Block payloads and scripts — draft spec

> ct tools today assume every input is a short, single-line argv string. That
> assumption is why agents still fall back to python/bash heredocs for
> block-shaped work: multi-line verbatim payloads, batched structural edits,
> and code-as-pattern anchors have no first-class entry point. This spec adds
> three layered capacities — explicit match mode, `file:`-sourced values, and
> a suite-wide block-document script format — so that class of fallback
> disappears without any shell anywhere.

Status: AS BUILT (implemented 2026-06-11/12; all walkthrough decisions §7
settled and shipped). The per-tool canonical references are the
`docs/explain/` pages of the affected tools — `ct-edit` carries the script
and block-edit reference; `ct-search`/`ct-view` the block-matching
reference. Shared machinery: `src/payload.rs` (schemes), `src/block.rs`
(block matcher + nearest miss), `src/blockdoc.rs` (`.ctb` parser),
`src/editscript.rs` (batch engine), `pattern::Mode` (`--mode`).
Originating evidence: a field report from agent work on the
nb-rs `datatypes` branch (2026-06-11), where batches of 4–12 structural edits
per file (enum variants plus their match arms across a 2400-line `ast.rs`)
could not be expressed with `ct-edit` and were implemented twice per session
as python heredocs — independently reinventing exactly the
assert-before-write batch contract specified here.

---

## 1. The three deficits

1. **Per-line matching.** Every pattern option matches within single lines.
   Structural code is block-shaped — a `match` arm group, an enum variant
   with its doc comment, a function body — and none of it is addressable.

2. **One edit per invocation, no transactional envelope.** A variant
   addition fans out to N match sites that must change *together*. Applying
   7 of 12 edits and then failing the 8th `--expect` leaves the tree
   uncompilable with the unwind manual. Sequential invocations cannot
   provide the batch-level all-or-nothing the work requires.

3. **Promotion surprise on code payloads.** substring→glob→regex promotion
   is right for search terms and wrong for verbatim code anchors. A literal
   Rust line like `WireSource::Port(_) => todo!("…"),` contains `(` `)` `!`
   `?`, silently promotes to regex, fails to match its own text, and the
   edit reports 0 sites. Diagnosing this costs more than the edit.

These are separable, and each layer below is useful alone: match mode fixes
promotion surprise even in plain argv form; `file:` covers a *single* block
edit with no script machinery; scripts add only what is genuinely new
(multi-edit atomicity, ordering, per-edit verdicts). An agent escalates
exactly as far as the task requires.

## 2. Layer 1 — explicit match mode (suite-wide)

Every pattern-taking tool gains `--mode literal|glob|regex`:

- Absent → today's promotion, unchanged. Promotion remains the ergonomic
  default; nothing existing changes behaviour.
- Present → promotion is **off** for every pattern argument in the
  invocation; the stated mode is used as-is.

Applies to `ct-search --grep`/`--name`, `ct-view --match`, `ct-edit --find`,
`ct-outline --match`, `ct-tree --name`, and the `--ok-match`/`--err-match`
matchers of `ct-test`/`ct-each`/`ct-await`. In block scripts the same choice
is the per-edit `mode=` attribute (§4), where the default flips to `literal`.

## 3. Layer 2 — `file:`-sourced values

Payload-typed options accept a **scheme prefix**:

| Prefix  | Meaning |
| ------- | ------- |
| `file:PATH` | The option's value is the file's contents, read verbatim (exact bytes, UTF-8). |
| `text:VALUE` | The remainder is the literal value — the escape hatch for a payload that genuinely begins with `file:` or `text:`. |

Only these two exact prefixes are recognised; everything else is literal
as-is (`--grep 'http://…'` and `--grep 'std::fmt'` are unaffected — there is
no general `scheme:` reservation). The namespace deliberately leaves room
for future sources without new syntax.

Rules:

- **Verbatim.** A `file:` payload is never promoted: its match mode defaults
  to `literal` (overridable by `--mode`). Replacement and value payloads are
  inserted byte-for-byte. In line-anchored matching contexts, a final
  terminating newline ends the last line; it does not add an empty trailing
  line.
- **Payload-typed options only.** The schemes apply where a value is content
  (patterns, replacements, structured values, stdin text, prose), never to
  selection/config options (`--base`, `--name`, `--emit`, numerics). Each
  tool's `--explain` enumerates its scheme-aware options.
- `text:` only strips the prefix; it does not change match mode. Sourcing
  and matching stay orthogonal.

Initial scheme-aware options:

| Tool | Options | What it unlocks |
| ---- | ------- | ---------------- |
| `ct-edit` | `--find`, `--replace` | a single block edit with zero quoting |
| `ct-search` | `--grep` | "does this exact block exist, where?" (§5) |
| `ct-view` | `--match` | show a block's neighbourhood before editing (§5) |
| `ct-patch` | the VALUE in `--set` / `--append` | multi-line string values into JSON/YAML |
| `ct-test` | `--stdin` | multi-line child input (the `supervise` plumbing already takes the text) |
| `ct-each` | `--items` | item lists from a file without a pipe |
| `ct-rules` | `--prompt`, `--why` | multi-line prose retention |

The intended loop: the agent writes `find.block` / `replace.block` with its
native file tool (no heredoc, no quoting), then runs one plain invocation:

```sh
ct edit --base src --name '*.rs' \
  --find file:target/find.block \
  --replace file:target/replace.block \
  --expect =1 --dry-run
```

(Argv strings can technically carry newlines, so direct tool-call agents can
pass blocks inline today — but shell-mediated agents cannot without quoting
gymnastics, and a payload file is more auditable either way.)

## 4. Layer 3 — the ct block document (`.ctb`)

One suite-wide script format, defined once in the lib (`blockdoc` module):
fence parsing, directive attributes, named payload sections. `ct-edit
--script` is the first consumer; `ct-patch` batches adopt the same envelope
later with their own section vocabulary. One parser, one format for agents
to learn. A tool rejects directives it does not understand (exit `2`).

### 4.1 Format

Delimited text, not JSON — payloads are code, and code must paste in
verbatim with zero escaping (newlines, quotes, `$`, backslashes):

```
#% edit expect="=1" file=src/ast.rs
#% find
            Value::U64(v) => v.to_string(),
#% replace
            Value::U64(v) => v.to_string(),
            Value::I64(v) => v.to_string(),
#% end
```

- A **fence line** starts with the fence string (`#%` by default;
  `--fence STR` for payloads that contain `#%` at line start). Everything
  else inside an open payload section is verbatim content.
- A **directive** is `#% NAME [key=value]…`. Attribute parsing splits each
  token at the *first* `=`; the remainder is the value verbatim — so
  `expect==1` is key `expect`, value `=1`. Double-quoting (`expect="=1"`)
  is the preferred spelling for values that would read ambiguously, and
  required for values containing spaces.
- **Payload sections** (`#% find`, `#% replace`, `#% value`) run until the
  next fence line. `#% end` closes the item.
- Outside items, blank lines and `#`-comment lines are ignored.

### 4.2 ct-edit vocabulary

`#% edit` opens one edit, with attributes:

- `expect=` — today's SPEC vocabulary (`any`, `none`, `N`, `=N`, `+N`,
  `-N`). **Default in scripts: `=1`** — scripted structural edits are
  anchored, and inside an atomic batch the stricter default is the safer
  one (an anchor that unexpectedly matches twice must stop the batch, not
  apply twice). The argv form keeps its existing default (`any`); this
  deliberate inconsistency is documented in both `--explain` pages.
- `mode=` — `literal` (default in scripts), `glob`, `regex`. Promotion is
  off in scripts; the author states intent.
- `file=` — optional per-edit narrowing **within** the invocation's
  `--base`/`--name` selection. No new targeting vocabulary.

`#% find` and `#% replace` carry the blocks verbatim, including leading
whitespace.

### 4.3 Block matching

A multi-line pattern matches as a **line-anchored literal block**: a find
block of K lines matches K consecutive source lines exactly, byte-for-byte
in literal mode, leading/trailing whitespace significant. Single-line finds
behave exactly as today. Multi-line patterns combined with `mode=regex` or
`mode=glob` are reserved — a usage error (exit `2`) for now, so the door
stays open without guessing semantics.

### 4.4 The prepare/confirm/write standard

This is the suite standard for **every** block operation that spans
multiple edit sites or multiple files (`ct-edit --script`, a multi-file
block `--find`, future `ct-patch` batches): the entire operation is
prepared and confirmed first, and **no file is changed unless ct has
confirmed the whole operation will complete successfully once started.**

Confirmation means phase 1 completes *in full, across all files*, before
phase 2 touches anything:

- every target file is read and parsed;
- every edit's matching and replacement is simulated in memory and its
  `expect` judged;
- every final buffer is fully computed;
- every target is pre-flighted for writability (permissions, existence of
  the parent), so the write phase has no foreseeable failure mode left.

Any failure at any point in phase 1 — a missed anchor, a wrong-sized match
set, an unreadable or unwritable target, a malformed script — fails the
batch with zero writes. Expectation failures are the clean negative (batch
verdict `ERROR`, exit `1`); structural and environment failures (malformed
script, `file=` matching nothing, overlap under `--no-cascade`, write
pre-flight) are usage/runtime errors (exit `2`). This generalises the contract `ct-edit` already
keeps for a single edit ("compute first, write only on SUCCESS") to the
batch and to the file set. Concurrent external modification of a target
between the two phases is out of scope, as it is for `ct-edit` today.

Phase 1 simulates the whole script in memory, **in script order**: each
edit matches against the buffer as already transformed by earlier edits in
the same file, and its `expect` is judged there. Phase 2 writes each file's
final buffer only if *every* edit passed.

```
# phase 1 (in memory, in script order)
buffer = read(file)
for edit in script:
    sites = match(edit.find, buffer)   # sees earlier edits' output
    judge(edit.expect, sites)          # any miss -> batch ERROR
    buffer = apply(edit, buffer)

# phase 2 (only if all passed)
write(file, buffer)                    # one write per file
```

Verification is exactly faithful to what gets written — this is the python
`s.replace` chain the field fallback used, with the verdicts attached. A
`--no-cascade` flag switches to pristine matching (every edit judged
against original content; any two edits with overlapping sites is a usage
error, exit `2`) for order-independent sweeps.

### 4.5 Atomic only

Any edit failing its `expect` → batch verdict `ERROR`, **zero writes**,
exit `1`. There is no flag that makes a partial write possible: partial
application is exactly the "7 of 12 applied, tree uncompilable" failure
this feature exists to prevent, and an opt-out would weaken the guarantee
agents are meant to rely on. Independent edits that genuinely don't need
atomicity run as separate invocations or separate scripts. (A
`--best-effort` mode was considered and rejected — §7.)

`--dry-run`, `--timeout`, heartbeats, and the write-completion guarantee
are unchanged: phase 2 is the write phase; once it starts, the watchdog is
disarmed and every write completes.

### 4.6 Diagnostics: `nearest_miss`

When a literal block fails to match, the result reports the best partial
alignment — the candidate site with the longest matching prefix of the
find block, its path and line, the index of the first diverging line, and
the diverging source/pattern line pair. The author sees *why* the anchor
missed (whitespace drift, a comment edit, an already-applied change)
without bisecting by hand. This is the block analogue of the suite's
evidence-on-negatives posture (`ct-deps` evidence paths).

Text mode prefixes per-site diff lines with the edit ordinal
(`[3/12] path:line:- …`), then a per-edit verdict table and the batch
summary. `--json` extends the existing shape:

```json
{
  "tool": "ct-edit",
  "verdict": "ERROR",
  "applied": false,
  "edits": [
    { "ordinal": 1, "expect": "=1", "mode": "literal",
      "replacements": 1, "verdict": "SUCCESS", "sites": [ … ] },
    { "ordinal": 2, "expect": "=1", "mode": "literal",
      "replacements": 0, "verdict": "ERROR",
      "nearest_miss": { "path": "…", "line": 571,
                        "first_diverging_line": 3,
                        "expected": "…", "found": "…" } }
  ]
}
```

### 4.7 ct-patch vocabulary (later, same envelope)

```
#% set path=tool.description format=json file=cfg.json
#% value
multi-line
string value
#% end

#% delete path=tool.legacy_key file=cfg.json
```

Same prepare/confirm/write standard (§4.4) over structured documents. Not
part of the first implementation pass; recorded here so the envelope is
designed for two consumers from day one.

## 5. Block matching in the read-only tools

The block matcher ships through `ct-search` and `ct-view` in the same pass
— it has to be built for `ct-edit` anyway, and exposing it keeps the
*locate → read → change → verify* loop symmetric around block edits:

```sh
# does this exact arm group exist, and where?
ct search --base src --name '*.rs' --grep file:arm.block --detail

# show me the block in context before editing
ct view src/ast.rs --match file:arm.block --context 5

# after the edit: assert the new block is present exactly once
ct search --base src --grep file:new.block --expect =1
```

A multi-line `--grep`/`--match` pattern uses the same line-anchored literal
semantics as §4.3; match counts, `--expect`, and verdicts are unchanged. On
a clean negative with `--detail`, the nearest-miss diagnostic is reported.
The output matchers of `ct-test`/`ct-each`/`ct-await` stay single-pattern
and stream-oriented — block semantics against captured output are murkier
and wait for demand.

## 6. Non-goals

- **Not a patch format.** No context-line fuzz, no hunk offsets. Anchors
  are exact (literal) or intentional (regex). Fuzzy application is what
  review-then-`git apply` is for.
- **No new targeting vocabulary.** `--base`/`--name` selection is
  unchanged; `file=` only narrows within it.
- **No shell, anywhere.** Nothing in this spec launches anything; it is
  pure input plumbing for the existing gated tools.
- **The single-edit argv form stays exactly as is.** Scripts and schemes
  are additive.

## 7. Settled decisions (walkthrough, 2026-06-11)

| # | Question | Decision |
| - | -------- | -------- |
| 1 | File-sourcing syntax | **Scheme prefix `file:`** (escape: `text:`), over an `@path` sigil or parallel `--*-file` flags. Only the two exact prefixes are reserved; the scheme namespace is extensible without new syntax. |
| 2 | Same-file edit interaction | **Full-chain simulation**: phase 1 verifies each edit against the buffer as transformed by earlier edits, in script order; write only if all pass. `--no-cascade` = pristine matching + overlap error. Rejected: the originating proposal's pristine-match/cascade-apply split (verification would not be faithful to what gets written). |
| 3 | `--best-effort` partial application | **Rejected — atomic only.** No flag makes a partial write possible. |
| 4 | Block matching reach | **`ct-search` + `ct-view` in the same pass** (verify loop symmetry); output matchers of the dispatch tools wait for demand. |
| 5 | Default `expect` in scripts | **`=1`**, argv form keeps `any`; the deliberate inconsistency is documented in both `--explain` pages. |
| 6 | Format scope | **One suite format, `.ctb`**, parser in the lib (`blockdoc`), `ct-edit` first consumer, `ct-patch` envelope designed in from day one; unknown directives are a usage error. |
| 7 | Multi-site/multi-file write safety | **Prepare/confirm/write standard (§4.4)**, stated as a requirement: validation — matching, replacement simulation, and write pre-flight — completes for the entire operation before any file changes; no write begins without confirmation the whole operation will complete. |

## 8. Implementation notes

- `src/blockdoc.rs`: fence/directive/payload parser, attribute splitting,
  shared by future consumers; unit-tested independently of `ct-edit`.
- `src/` value-resolution helper for the `file:`/`text:` schemes, shared by
  every scheme-aware option; resolves before any matching/promotion logic.
- Block matcher (line-anchored K-line literal match + nearest-miss scan)
  lives beside the existing per-line engine and is shared by `ct-search`,
  `ct-view`, `ct-edit`.
- Explain docs (`docs/explain/*.{md,json}`) updated for every tool that
  gains `--mode`, scheme-aware options, or `--script`; the `ct.json`
  manifest follows via the existing identity test.
- Tests: blockdoc parsing (fences, attribute `=` splitting, custom fence),
  scheme resolution (`file:`, `text:`, non-reservation of other prefixes),
  block match + nearest-miss, script atomicity (failing edit → zero
  writes), write pre-flight (read-only target file → exit `2`, zero writes
  anywhere), cascade vs `--no-cascade`, `=1` script default,
  `ct-search`/`ct-view` block patterns, dry-run parity. All shipped in
  `tests/block_payloads.rs` plus unit tests in the four new lib modules.
