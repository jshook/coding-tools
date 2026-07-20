---
type: Specification
title: Event-assisted indexing
timestamp: 2026-07-19
---

# Event-assisted indexing — design specification v1

Status: AS BUILT (2026-07-19). The initial provider is `okf-markdown`; the
provider registry and scope model intentionally admit future indexers without
implicitly indexing unknown formats.

This specification defines how `ct` maintains persistent search indexes without
turning filesystem notifications, filename guesses, or a background process into
correctness dependencies. The initial consumer is the existing OKF full-text
index. The policy and lifecycle are intentionally generic so more index providers
can be registered later.

---

## 1. Goals and non-goals

The design has four goals:

1. Avoid rescanning unchanged content roots before every indexed query.
2. Index only files for which `ct` has an explicit tokenizer/index provider.
3. Make the effective include/exclude policy and every per-path decision visible.
4. Report enough timing and storage data to determine whether indexing helps.

Filesystem notifications are an opportunistic acceleration layer. They are not a
source of truth. A missing daemon, lost event, watcher overflow, unsupported
filesystem, stale heartbeat, or malformed cache must fall back to synchronous
reconciliation. A query may become slower in those cases, but never silently
stale because the optimization was unavailable.

`ct-search` remains a direct filesystem search in v1. The first indexed provider
is `okf-markdown`, backed by the existing `.ct/okf/` immutable-segment index.
General source-code or arbitrary-file content indexing is explicitly deferred.

## 2. Vocabulary

| Term | Meaning |
| --- | --- |
| **provider** | A compiled-in implementation that owns eligibility checks, parsing, tokenization, stemming/versioning, and document metadata for one known format. |
| **scope** | A root, provider, and include-pattern set from which eligible documents may enter an index. |
| **exclusion** | A global or scope-local pattern that prevents indexing even when an include matches. |
| **hard exclusion** | A non-overridable safety boundary such as the index's own storage directory or a non-regular file. |
| **effective plan** | The normalized scopes, exclusions, limits, and provider versions after combining detected defaults with `.ct/index.jsonc`. |
| **dirty path** | A path reported by the watcher that may require an upsert or removal. It is a hint until checked against the effective plan and filesystem. |
| **generation** | A monotonically increasing identifier published with an atomic index manifest update. |
| **reconciliation** | A complete walk/stat comparison of an effective scope against the index manifest. The correctness backstop. |

## 3. Provider boundary

No generic “looks textual” provider exists. A file is indexable only when an
enabled compiled-in provider claims it. Each provider defines:

- stable provider id and schema/tokenizer version;
- default filenames/extensions;
- content validation or magic sniffing;
- maximum supported size;
- parser, tokenizer, normalization/stemming, and metadata extraction;
- whether symlinks or other exceptional file kinds are supported.

The initial registry contains only:

| Provider | Eligible input | Index |
| --- | --- | --- |
| `okf-markdown` v1 | regular UTF-8 `.md` concept documents under detected/configured OKF roots (frontmatter is optional); reserved `index.md` and `log.md` are excluded | `.ct/okf/` |

Databases, binary state, archives, images, executables, object files, index
segments, and every unknown extension are therefore ineligible by default.
Adding an include pattern does not bypass the provider boundary: a scope must
name a registered provider, and that provider must accept the file.

The manifest records the provider id and version. A provider version change
requires rebuilding that provider's index before it can be queried.

## 4. Configuration and precedence

The optional project store is `.ct/index.jsonc`. With no store, `ct` derives a
conservative plan from registered providers and existing project configuration.
For v1, every detected OKF root becomes an `okf-markdown` scope with
`include = ["**/*.md"]`.

Example:

```jsonc
{
  "version": 1,
  "watch": true,
  "debounce_ms": 150,
  "audit_seconds": 300,
  "idle_seconds": 3600,
  "max_file_bytes": 2097152,

  // Optional. When absent, the effective per-machine value is the smaller of
  // 2 GiB and five percent of physical RAM.
  "max_daemon_memory_bytes": 536870912,

  "scopes": [
    {
      "root": "knowledge",
      "provider": "okf-markdown",
      "include": ["**/*.md"],
      "exclude": ["archive/**"]
    }
  ],

  "exclude": [
    ".git/**",
    ".ct/okf/**",
    "target/**",
    "node_modules/**",
    "**/*.db",
    "**/*.sqlite*"
  ]
}
```

Rules, in descending precedence:

1. Hard exclusions always win: index storage, non-regular files, and paths that
   cannot be represented beneath their declared root.
2. Global and scope-local exclusions win over includes.
3. A path must match a scope include.
4. The named provider must exist, be enabled, accept the filename/content, and
   accept the size.
5. With no explicit `scopes`, provider-derived scopes are used. Once explicit
   scopes exist they replace, rather than silently augment, derived scopes.

Patterns use project/scope-relative paths with `/` separators and gitignore-like
`*`, `**`, and `?` wildcards. Configuration or ignore-policy changes invalidate
the affected scope.

`ct okf index init --dry-run` prints the derived configuration;
`ct okf index init --write` materializes it. Neither mode indexes unsupported
formats.

## 5. Inspectability

The policy surface is read-only unless `--write` is explicit:

```text
ct okf index scopes [--effective]
ct okf index why PATH
ct okf index status
```

`scopes` reports each root, provider, include/exclude patterns, origin
(`derived`, `project`, or `hard`), provider version, and limits. `why` provides a
decision trace for one path, including the matching scope/pattern or rejection
reason. Both have structured output through the existing global `--json` flags.

Stable decision reasons include `included`, `excluded`, `hard-excluded`,
`outside-scope`, `unsupported-provider`, `unsupported-type`, `too-large`, and
`not-regular`. A file that passes this path-level decision can still report a
provider read/parse error during indexing.

## 6. Event-assisted maintenance

A per-project watcher daemon observes effective scope roots recursively using the
platform's native notification facility when available. Events are coalesced for
`debounce_ms`, normalized, and filtered through the effective plan before they
can affect an index:

- create or modify: stat, validate, then upsert;
- remove: tombstone an existing document;
- rename: remove the old key and validate/upsert the new key;
- ambiguous event, overflow, watcher error, root/config change: mark the scope
  for reconciliation.

The daemon owns a dirty set, not an assumption that each raw event is complete or
ordered. Atomic-save editor sequences may produce several event shapes; the
final filesystem state after the debounce window decides the delta.

Watcher state has four externally visible lanes:

- `clean`: all observed events are reflected through generation N;
- `dirty`: known paths are queued;
- `reconcile`: one or more scopes require a full comparison;
- `unavailable`: no trustworthy daemon state exists.

## 7. Query freshness and fallback

An indexed query attempts a bounded freshness handshake with the daemon. A
successful barrier means all events observed before the request have been applied
and an index generation has been atomically published. The query then opens that
generation.

If no healthy daemon acknowledges the barrier promptly, the caller runs the
existing synchronous reconciliation and may attempt one guarded daemon start.
It never waits indefinitely and never treats an unacknowledged cache as clean.

Native event delivery can be incomplete. Full reconciliation therefore runs:

- at initial daemon startup;
- after overflow/error or effective-plan changes;
- after sleep/resume or an expired heartbeat;
- every `audit_seconds` while active;
- whenever an explicit `index update` or `index rebuild` requests it.

Synchronous reconciliation is the fallback for filesystems that do not support
reliable native events. Disabling watching restores reconcile-on-query behavior.

## 8. Daemon lifecycle

The daemon is automatically started on the first indexed query when `watch` is
enabled. A per-project OS file lock is the authority for the one-process rule;
the OS releases it after both graceful and ungraceful process exits. A short,
atomically-created startup claim only closes the launcher-to-child lock handoff
race and is recovered after ten seconds if startup dies. Before spawning, the
client must prove that its runtime directory and claim can be persisted. A
failed spawn is cached where possible and otherwise simply disables the
optimization for that invocation.

The process uses no shell, inherits no standard streams, writes a heartbeat and
status record, and exits after `idle_seconds` without a query/barrier (one hour
by default). POSIX `SIGINT`, `SIGTERM`, and `SIGHUP`, plus the corresponding
Windows console control events, request the same graceful path: mark status
`unavailable`, record the stop reason, and release the singleton lock. Sudden
termination still releases the OS lock, so stale status cannot prevent lazy
restart. Explicit controls are:

```text
ct okf index watch status
ct okf index watch start
ct okf index watch stop
```

The launcher must never repeat the update-check failure mode where an
unpersistable claim causes every invocation to spawn another nominally detached
process.

The daemon is otherwise quiet. It writes only lifecycle records (start, stop
reason, and exceptional exit detail) to `.ct/okf/runtime/daemon.log`. The active
log rotates at 32 KiB and retains two older generations; query and per-event
activity is deliberately not logged.

## 9. Persistence and concurrency

Index segments remain immutable. A writer completes segment files first, writes
the next manifest to a temporary sibling, and atomically renames it over the old
manifest. Readers open one complete manifest generation. A per-index update lock
serializes daemon updates, explicit maintenance, and synchronous fallback.

The distinct per-project daemon lock serializes process ownership. Status,
heartbeat, and PID fields are observational and never establish ownership.

Daemon runtime files live beneath the index's hard-excluded runtime directory and
are not themselves watched as content. A PID alone is not proof of identity.

## 10. Metrics

`index status` exposes operational facts without retaining repository content or
query text:

- generation and watcher lane/backend;
- indexed document, segment, and tombstone counts;
- logical source bytes and physical index/runtime bytes;
- dirty-path and pending add/change/remove counts;
- last event-batch duration and affected paths;
- last reconciliation duration and entries visited;
- cached daemon-start failure and last heartbeat;
- last event-to-index latency;
- current daemon resident memory, physical system memory, and the effective
  daemon memory ceiling.

The memory ceiling is configurable as `max_daemon_memory_bytes`. When omitted,
it is recomputed on each machine as `min(2 GiB, physical RAM / 20)` and is not
materialized by `index init`. The daemon samples its resident set on each
heartbeat and gracefully exits with reason `memory-limit` after exceeding the
ceiling, allowing a later query to reconcile and lazily start a fresh process.

JSON carries raw counts, bytes, and durations in milliseconds. Text output uses
human-readable units but does not omit the underlying document/segment counts.

## 11. Failure semantics

Malformed project configuration, an explicitly named unknown provider, or an
index write failure is exit `2`: the requested indexing policy cannot be honored.
A watcher failure alone is not a query failure when synchronous reconciliation
succeeds. Status and JSON must distinguish `fallback` from a watcher-clean query.
Crossing the daemon memory ceiling is likewise an optimization shutdown, not an
index correctness failure.

No event may cause a path outside its canonical scope to be opened. Symlink
following is off unless a future provider explicitly supports and contains it.

## 12. Acceptance criteria

1. Repeated indexed queries over an unchanged healthy scope perform no full walk.
2. Create, modify, rename, and remove become searchable/unsearchable after one
   bounded freshness barrier.
3. Killing the daemon, dropping events, or corrupting runtime state still yields
   correct results through reconciliation.
4. Unknown/binary/database files never enter the index without a registered
   provider that accepts them.
5. Every candidate path has an inspectable decision through `index why`.
6. Scope, timing, and storage metrics are available in text and JSON.
7. An unwritable runtime directory produces no repeated daemon spawn attempts
   within one command and never adds seconds to ordinary query startup.
8. At most one daemon holds a project's singleton lock; after any process exit,
   a later indexed query may lazily acquire it and start a replacement.
9. The daemon reports RSS and self-terminates above its effective memory limit.
