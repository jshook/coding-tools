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
`--mode literal|glob|regex|auto` states how both matchers are interpreted:
the first three switch promotion **off**, and `auto` is the promoting default
requested out loud. State `literal` when a matcher is verbatim output text.

`--timeout SECS` is **required** — a wait is bounded by design. (The two
flags that run no wait at all, `--list-allowed` and `--explain`, need no
bound.) The bound covers everything: a single probe run can never outlive it
(process-group kill), and expiry is `ERROR` with a reason naming the bound
and the number of probe runs. `--heartbeat SECS` pulses while waiting (tokens: `{ELAPSED}`
`{TOOL}` `{QUESTION}` `{CMD}` `{TICKS}`); `--heartbeat-emit` sets the pulse
template and `--heartbeat-to` picks the stream (`stderr`, the default, or
`stdout`).

### Bounding a single run: `--probe-timeout`

By default there is **no separate per-run limit** — a probe run may use
whatever is left of `--timeout`. That is the honest default, but it has one
sharp edge: a probe slower than the wait (a `ct-search` over a huge tree,
say) can spend the entire budget inside a single run, and the wait then
expires without the probe ever having answered.

`--probe-timeout SECS` bounds each run separately. A run that exceeds it is
cut short and counts as *not yet*, so the wait keeps polling instead of
stalling inside one call:

```sh
# each scan gets 2s; a slow one is abandoned and retried next tick
ct-await --timeout 120 --every 5 --probe-timeout 2 \
  -- ct-search --base target --name build.log --grep 'BUILD SUCCESS' --quiet
```

The same rule covers a probe that *hangs* rather than merely being slow — a
filtered port whose connect never answers, for instance. Without a per-run
bound, one such attempt consumes the rest of `--timeout` and the wait reports
a single probe run; `--probe-timeout` turns it back into a poll.

The two ways a run can stop are reported differently, because they call for
different fixes:

- **cut short by `--probe-timeout`** → not fatal; the wait continues. If it
  eventually expires, the reason says how many runs were cut — *"every run
  was cut short by --probe-timeout 2s without finishing"* means the probe is
  too slow to answer at all, so **narrow the probe** (a smaller `--base`, a
  more specific `--name`) rather than raising `--timeout`.
- **still running when `--timeout` arrived** → the wait ends `ERROR`, and the
  reason reports **how long that run had been going**
  (`probe run 3 was still running at the deadline (it had run 13s)`), so a
  slow probe diagnoses itself instead of reading like a false negative.

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

`--list-allowed` prints that whole gate — and the built-in probes below —
then exits `0` without running anything and without needing a `--timeout`.
Plan a probe that will be accepted, instead of discovering the gate by being
refused by it.

### Built-in probes: `--tcp-port` and `--http`

The most common wait an agent has — *"the local service I just started is
accepting connections now"* — cannot be expressed with the command
allowlist, because the honest way to ask is to open a socket and no
allow-listed program does that. Two built-in probes cover it, and they need
no `--` argv at all (giving both a built-in probe and an argv probe is a
usage error):

```sh
# a port is accepting connections
ct-await --timeout 30 --every 1 --tcp-port 8080

# a health endpoint is answering
ct-await --timeout 60 --every 2 --http http://127.0.0.1:8080/health --ok-status 200
```

Both stay inside the read-only philosophy two ways. They are **observations**:
`--tcp-port` opens a connection and drops it, sending nothing and reading
nothing; `--http` issues one `GET`, follows no redirect, and reads no further
than the status line. And they are **loopback-only** — the host must be
`127.0.0.0/8`, `::1`, or `localhost`, and anything else is refused with the
reason rather than resolved, so a bounded wait can never become a
general-purpose network client. `--http` takes `http://` only: a loopback
health check has nothing to protect in transit, and a self-signed certificate
on `127.0.0.1` would turn *"is the service up?"* into an unrelated TLS error.
`--ok-status` (default `200`) is the status that establishes the condition;
any other status, a refused connection, or a malformed reply is an ordinary
*not yet*. `--ok-match`/`--err-match` still apply, matched against the
probe's one-line detail (`connected to 127.0.0.1:8080`, `HTTP 503 from ...`).

## Reporting

An expiry also names **what the last run saw** — `; last probe: HTTP 200 from
127.0.0.1:8080/health (want 503)`, or `; last probe: no listener on
127.0.0.1:8080: connection refused`. A wrong expectation and an absent
service produce the same bare timeout without it.

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
- **Wait for a just-started local service to accept connections — the built-in loopback probe, instead of a hand-rolled poll loop.**
  ```sh
  ct await --timeout 30 --every 1 --tcp-port 8080 --question 'Is the dev server listening?'
  ```
- **Wait until the local health endpoint answers 200, bounded at 60s.**
  ```sh
  ct await --timeout 60 --every 2 --http http://127.0.0.1:8080/health --ok-status 200
  ```
- **Bound each probe run at 2s so a slow scan is retried rather than swallowing the whole 120s wait.**
  ```sh
  ct await --timeout 120 --every 5 --probe-timeout 2 -- ct-search --base target --name build.log --grep 'BUILD SUCCESS' --quiet
  ```

### Documentation

| Option                 | Effect |
| ---------------------- | ------ |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help. |
| `-V`, `--version`      | Version. |
