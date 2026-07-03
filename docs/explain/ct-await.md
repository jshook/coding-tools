# ct-await — Wait, Observably, for an External Outcome

> Poll a gated read-only probe until it succeeds, an abort pattern appears,
> or the bound expires — observe work you don't execute.

Some outcomes belong to processes the suite deliberately does not run: a
human's `mvn test` in another terminal, a CI pipeline, a deploy. `ct-await`
separates **observation authority** from **execution authority**: whoever
owns the work runs it; `ct-await` watches its observable effects — a log
marker, a generated file, a passing invariant — through the suite's own
read-only probes, and turns the wait into a bounded, framed verdict instead
of a `while ! grep …; do sleep 5; done` loop.

This document is the canonical reference for `ct-await`. It is also what the
tool prints for `ct-await --explain` (`--explain md`); `ct-await --explain
json` prints the equivalent MCP / tool-use definition.

## Replaces patterns like

```sh
# unbounded, silent, shell-fragile
while ! grep -q 'BUILD SUCCESS' target/build.log 2>/dev/null; do sleep 5; done
```

with:

```sh
# you run:   mvn test > target/build.log 2>&1     (in your terminal)
# the agent runs, and stays honestly bounded while you do:
ct-await --question "Did the integration build pass?" \
  --every 5 --timeout 900 --heartbeat 30 \
  --ok-match 'BUILD SUCCESS' --err-match 'BUILD FAILURE' \
  -- cat target/build.log
```

## The wait

Each tick (every `--every` seconds, default 5), the probe — an argv after
`--`, launched directly, never through a shell — runs once from the current
directory, and the tick is classified with exactly `ct-test`'s matcher
precedence:

- **`--err-match PATTERN`** (substring→glob→regex promoted) found in the
  probe's stdout or stderr → decisively failed: `ERROR`, immediately — a
  failed build reports in seconds, not after the timeout. Decisive over
  everything else.
- **`--ok-match PATTERN`** found → the condition is established: `SUCCESS`,
  done. When supplied, it is the **required proof**: a probe exiting `0`
  without it is still *not yet* (fail-closed).
- **No matchers**: probe **exit `0`** establishes the condition.
- **Anything else** → *not yet*. A probe that errors is the normal waiting
  case (the log file does not exist yet); only a probe that cannot launch at
  all is a hard error (`exit 2`).

Matchers see only what the probe *prints* — so a probe that surfaces content
(`cat build.log`, `ct-view`, `ct-search --detail`) pairs with matchers, while
a self-classifying probe (`ct-search --quiet`, `ct-check --quiet`) needs none.
`--mode literal|glob|regex` pins how both matchers are interpreted
(promotion off) — state `literal` when a matcher is verbatim output text.

`--timeout SECS` is **required** — a wait is bounded by design. The bound
covers everything: a single probe run can never outlive it (process-group
kill), and expiry is `ERROR` with a reason naming the bound and the number
of probe runs. `--heartbeat SECS` pulses while waiting (tokens: `{ELAPSED}`
`{TOOL}` `{QUESTION}` `{CMD}` `{TICKS}`); `--heartbeat-emit` sets the pulse
template and `--heartbeat-to` picks the stream (`stderr`, the default, or
`stdout`).

## What a probe may be

The same fixed, immutable read-only set `ct-each` dispatches by default — a
**platform-aware** allowlist, so probes work on Unix/MSYS2 and on native Windows
alike. It is a cross-platform core (`ct-await ct-check ct-outline ct-search
ct-tree ct-view`) plus the host OS's stock read-only utilities (`cat echo false
file grep head ls pwd stat tail true wc` on Unix/MSYS2, or `findstr hostname more
where whoami` on native Windows), plus `ct-test` and `ct-each` (without
`--mutating`). `ct-check` being included means *"wait until the project's
invariants hold"* is one command. `ct-await` will dispatch the read-only `ct-*`
tools — including `ct-await` itself as a bounded poll — but it never polls
**itself** (no self-nesting), the same guard `ct-each` has.

## Reporting

Default: the `== question ==` banner and one outcome line
(`SUCCESS (probe succeeded after 42s (9 run(s)))`); the reason also goes to
stderr on `ERROR`, so a red wait is never unexplained. `--quiet` suppresses
the banner and outcome line. `--emit`/`--emit-stderr` templates take
`{RESULT}` `{ELAPSED}` `{TICKS}` `{REASON}` `{QUESTION}` `{CMD}`.

## Exit status

| Code | Meaning                                                    |
| ---- | ---------------------------------------------------------- |
| `0`  | the probe succeeded within the bound                       |
| `1`  | `--abort-on` matched, or the `--timeout` expired           |
| `2`  | usage error, a refused probe, or a probe that cannot launch |

## Examples

```sh
# Wait for a file the human's build will create.
ct-await --timeout 600 -- ct-search --base target/release --name 'my-app' --quiet

# Wait until the project's invariants hold again (ct-check is gated read-only).
ct-await --question "Do all invariants hold yet?" --every 10 --timeout 300 \
  -- ct-check --quiet

# Watch a log with both outcomes wired (the probe surfaces the content).
ct-await --every 2 --timeout 1800 \
  --ok-match 'test result: ok' --err-match 'FAILED|error\[' \
  --emit '{QUESTION} -> {RESULT} after {ELAPSED}s' \
  --question "Did the long test suite pass?" \
  -- cat ci.log
```

- **Poll until ct-search finds 'server ready' in logs (bounded at 60s), instead of a hand-rolled sleep/retry loop.**
  ```sh
  ct await --timeout 60 --every 2 -- ct-search --base logs --grep 'server ready' --quiet
  ```
- **Wait until 'ok' appears in health.txt (probe exit 0), bounded at 30s.**
  ```sh
  ct await --timeout 30 -- ct-search --base . --name health.txt --grep ok --quiet
  ```

### Documentation

| Option                 | Effect |
| ---------------------- | ------ |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help. |
| `-V`, `--version`      | Version. |
