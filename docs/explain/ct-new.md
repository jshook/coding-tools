# ct-new — Create a File, Without Clobbering

> Give it a path and a body; it writes the file atomically, refuses to overwrite
> what is already there unless you say so, and reports what it did as a framed
> verdict.

`ct-new` is the suite's *first* move on a file that does not exist yet. The ad-hoc
equivalent — `cat > f <<EOF`, `echo … > f`, `touch f` — has three silent failure
modes: it truncates an existing file without a word, it happily writes to a
mistyped path, and it tells you nothing about what landed. `ct-new` makes each of
those explicit: an existing target is refused by default (`--on-exists`), a
missing parent directory is an error until you pass `--parents`, and every run
ends in a `SUCCESS`/`ERROR` verdict you can gate on. Reachable directly or as
`ct new`.

This document is the canonical reference for `ct-new`. It is also what the tool
prints for `ct-new --explain` (`--explain md`); `ct-new --explain json` prints the
equivalent MCP / tool-use definition.

## When to use it

- Add a new source file, test, config, or doc to a project.
- Scaffold from a template (`--content file:templates/mod.rs`).
- Write a body that contains quotes, `$`, or backticks, with no escaping at all
  (`--content stdin:` with a heredoc).
- Make sure a file exists without caring whether it already did
  (`--on-exists skip`) — an idempotent step inside a larger script.
- Create a whole set of files in one atomic batch (`--script`).

## The two forms

```sh
ct new <PATH> --content <PAYLOAD> [options]     # one file
ct new --script <FILE.ctb> [options]            # a batch, all-or-nothing
```

## Content

| Option               | Argument  | Meaning                                                    |
| -------------------- | --------- | ---------------------------------------------------------- |
| `--content`          | `PAYLOAD` | The file's body. Payload-typed (see below).                |
| `--empty`            | —         | Create a zero-byte file. Required to be explicit: a missing `--content` is a usage error, never a silently empty file. |
| `--no-final-newline` | —         | Write the bytes exactly as given, without the terminating newline. |

`--content` is **payload-typed**, the same scheme every payload option in the
suite uses:

| Value           | Meaning                                                          |
| --------------- | ---------------------------------------------------------------- |
| `TEXT`          | The literal text.                                                |
| `file:PATH`     | The file's contents, verbatim — how a template is instantiated.  |
| `stdin:`        | All of standard input, verbatim — the shell-safe way to pass a body containing quotes, `$`, or backticks. |
| `text:VALUE`    | Escape hatch for a body that genuinely begins with `file:` / `stdin:` / `text:`. |

A non-empty body is written with a final newline if it does not already end in
one, so a file created from an argv string is not missing its terminator. Pass
`--no-final-newline` when the exact bytes matter more.

## Where it goes

| Option      | Argument | Meaning                                                             |
| ----------- | -------- | ------------------------------------------------------------------- |
| `PATH`      | —        | Positional: the file to create. Required unless `--script` is given. |
| `--parents` | —        | Create missing parent directories.                                   |

A missing parent directory is an **error** (exit `2`) naming the directory unless
`--parents` is given — `ct new src/hokes/mod.rs` should fail, not quietly invent
`src/hokes/`. A path that is already a directory is likewise an error.

## Collision policy

`--on-exists` decides what an existing target means. There is no other overwrite
switch, and the default never destroys anything.

| Value             | Effect                                                                        |
| ----------------- | ----------------------------------------------------------------------------- |
| `fail` *(default)*| Nothing is written; the file is reported `blocked` and the verdict is `ERROR` (exit `1`). |
| `skip`            | The existing file is accepted as-is and counted as ensured — an idempotent create. |
| `overwrite`       | The file's contents are replaced.                                             |

Each file is published **atomically** (written beside the target and renamed into
place), so a reader never sees a half-written create, and an interrupted run
never leaves a truncated file.

## Batches (`--script`)

`--script` reads a `.ctb` [block document](../specs/blocks.md) of `new` items and
creates them under the suite's prepare/confirm/write standard: every target is
decided in memory first, and if any one of them is blocked, **nothing** is
written. There is no flag that permits a partial batch.

```text
#% new file=src/limits.rs
#% content
pub const MAX_DEPTH: usize = 64;
#% end
#% new file=tests/limits.rs on-exists=skip
#% content
#[test]
fn depth_is_bounded() {
    assert_eq!(coding_tools::limits::MAX_DEPTH, 64);
}
#% end
```

| Item part      | Meaning                                                                    |
| -------------- | -------------------------------------------------------------------------- |
| `file=PATH`    | Required. The target path.                                                 |
| `on-exists=…`  | Optional per-item `fail`/`skip`/`overwrite`, overriding the invocation's.   |
| `content`      | Payload section, taken **verbatim** — code pastes in with zero escaping. Omit it for an empty file. |

`--fence` changes the directive marker (default `#%`) for a body that itself
starts lines with it. Two items naming the same target is a usage error: which
body would win is exactly the order-dependence the batch form exists to remove.

## Output

| Option          | Effect                                                                |
| --------------- | --------------------------------------------------------------------- |
| `--dry-run`     | Decide and report everything, including a bounded preview of each body, but write nothing. |
| `--quiet`       | Suppress the per-file lines; print only the summary.                  |
| `--json`        | Emit a structured result (see below).                                 |
| `--json-pretty` | Like `--json`, but pretty-printed (indented).                         |

Text mode prints one line per file, then a summary:

```text
src/limits.rs: created (1 line(s), 33 byte(s))
1 file(s) ensured (1 created, 0 overwritten, 0 skipped, 0 blocked) -> SUCCESS (applied)
```

Under `--dry-run` each body is previewed as `path:N:+ line` rows, capped at 12
lines per file with a `+ N more line(s)` marker — a create is bounded like every
other read in the suite. `--json` emits:

```json
{
  "tool": "ct-new",
  "verdict": "SUCCESS",
  "dry_run": false,
  "applied": true,
  "ensured": 1,
  "created": 1,
  "overwritten": 0,
  "skipped": 0,
  "blocked": 0,
  "files": [ { "path": "src/limits.rs", "action": "created", "lines": 1, "bytes": 33 } ]
}
```

`action` is `created`, `overwritten`, `skipped`, or `blocked`; a `--script` run
adds the script's path under `script`.

### Documentation

| Option                 | Effect                                                            |
| ---------------------- | ---------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                      |
| `-V`, `--version`      | Version.                                                         |

## Verdict & expectations

The verdict is `--expect` applied to the number of files **ensured** — created,
overwritten, or already present under `--on-exists skip`: `any` (≥1, the
default), `none` (==0), `N` (≥N), `=N`, `+N`, `-N`. Files are written only when
the verdict is `SUCCESS` and `--dry-run` is not set.

A `blocked` target forces `ERROR` whatever `--expect` says: refusing to clobber
must never read as success. So the everyday create,

```sh
ct new src/limits.rs --content file:limits.rs.tpl --expect =1
```

fails loudly if the file was already there, and `--on-exists skip` is how you say
that outcome is fine. `ct-new` runs no external programs, so it is not subject to
the `ct-test` allowlist; safety comes from the default policy, `--dry-run`,
`--expect`, and your VCS. `ct-each --mutating` may dispatch it, so one command
template can create a file per item.

## Run bounds and liveness

Every suite tool is bounded and observable the same way:

| Option             | Argument   | Effect                                                            |
| ------------------ | ---------- | ----------------------------------------------------------------- |
| `--timeout`        | `SECS`     | Abort planning (exit `2`, with a one-line message) if it exceeds SECS seconds (fractional allowed). |
| `--heartbeat`      | `SECS`     | Print a liveness pulse every SECS seconds while the run is in progress. |
| `--heartbeat-emit` | `TEMPLATE` | Pulse template. Tokens: `{ELAPSED}` (whole seconds so far) `{TOOL}`. Default: `[{ELAPSED}s]`. |
| `--heartbeat-to`   | `stderr\|stdout` | Stream for pulses. Default: `stderr`.                       |

The timeout bound covers the planning phase only: once a `SUCCESS` verdict begins
writing, every write completes — a timeout can never leave a file half-written.

## Exit status

| Code | Meaning                                                                  |
| ---- | ------------------------------------------------------------------------ |
| `0`  | verdict `SUCCESS` (written unless `--dry-run`)                           |
| `1`  | verdict `ERROR` — a target was blocked, or the count missed `--expect`; nothing written |
| `2`  | usage or runtime error (missing parent directory, bad script, write error) |

## Examples

```sh
# Preview the exact bytes before anything is written.
ct new src/limits.rs --content 'pub const MAX_DEPTH: usize = 64;' --dry-run

# A body with quotes, $ and backticks — no escaping, because the shell never sees it.
ct new scripts/release.sh --content stdin: --parents <<'EOF'
echo "releasing $(git describe --tags)"
EOF

# Instantiate a template, asserting exactly one file appeared.
ct new src/hooks/mod.rs --content file:templates/mod.rs --parents --expect =1

# Idempotent: fine whether or not the file was already there.
ct new .ct/notes.md --content 'scratch notes' --parents --on-exists skip

# A deliberate empty file, with the exact bytes given (no added terminator).
ct new .keep --empty --no-final-newline

# A whole scaffold in one atomic batch; one blocked target means zero writes.
ct new --script scaffold.ctb --json --quiet
```

- **Create a new source file with its content in one gated call, instead of `cat > file <<EOF` which silently truncates an existing file.**
  ```sh
  ct new src/limits.rs --content 'pub const MAX_DEPTH: usize = 64;' --expect =1
  ```
- **Scaffold from a template into a directory that does not exist yet, previewing the bytes first.**
  ```sh
  ct new src/hooks/mod.rs --content file:templates/mod.rs --parents --dry-run
  ```
- **Make sure a file is there without caring whether it already was, instead of `touch` plus a redirect.**
  ```sh
  ct new .ct/notes.md --content 'scratch notes' --parents --on-exists skip
  ```
