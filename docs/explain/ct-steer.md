# ct-steer

Steer ad-hoc shell to the `ct` tool that serves it — and install the hook that
does it automatically. Reachable as `ct steer …` or `ct-steer …`.

Agents reach for raw shell (`find | xargs grep`, `sed -i`, `cat | head`, `for`
loops) even when a suite tool would do the job bounded, deterministic, and
self-verifying. Run as a Claude Code **PreToolUse hook**, `ct-steer` inspects
each proposed shell command and, when a `ct` tool clearly serves it, steers the
agent to the equivalent `ct` command instead of letting the raw one run.

The matcher is deliberately **conservative**: it fires only on a fixed set of
high-confidence idioms, never re-steers a command that already invokes `ct`, and
is **fail-open** — anything it doesn't recognise, or any malformed input, is
allowed silently. It runs ahead of *every* shell call, so a miss costs nothing.

## What it recognises

| Shell idiom | Steered to |
| --- | --- |
| `find … \| xargs grep`, `find … -exec grep` | `ct search` |
| `grep -r`, `rg`, `ag` | `ct search` |
| `find … -name` (no grep) | `ct search` |
| `grep -c PATTERN file` (count matches) | `ct search --summary` (`--expect` to assert) |
| `sed -i`, `perl -i` | `ct edit` (preview + `--expect` gate) |
| `head`/`tail`/`sed -n 'A,Bp'` on a file | `ct view --range` |
| `python -c`/`node -e`/`perl -e`/`ruby -e`/`jq` reading a file | `ct view` / `ct search` |
| `ls -R`, `tree` | `ct tree` |
| `wc -l`/`wc` over files (incl. `cat FILES \| wc`) | `ct tree --summary` |
| `for … do … done`, `while read` (per-item map) | `ct each` |
| `for`/`while`/`until … sleep …` (poll/wait loop) | `ct await` |
| `A && B`, `A \|\| B` (every segment ct-serviceable) | `ct and` / `ct or` |

A `for`/`while`/`until` loop whose body `sleep`s and re-probes is a bounded
**wait**, steered to `ct await` (not `ct each`). An interpreter one-liner is
steered only when it **reads** a file with no write signal — pure-compute
one-liners (and ones that open a file for writing) are left alone.

A chain (`&&` / `||`) is only steered when *every* segment is itself
ct-serviceable, so `grep -r x && make` (no `ct` analogue for `make`) is left
alone while `grep -r x && sed -i …` becomes `ct and search … ::: edit …`.

### Multi-line scriptlets

A **multi-line** command (a here-doc-style scriptlet an agent runs in one `Bash`
call) is classified line by line. Scaffolding — comments, `cd`, variable
assignments, `echo` — is ignored; each remaining line is scored as *already
`ct`*, *ct-advisable* (a raw idiom with a `ct` form), or *opaque* (no `ct`
analogue). The feedback is tiered:

- **Fold into one `ct and` chain** — when *every* meaningful line is already `ct`
  or ct-advisable **and at least one is not yet `ct`**: the whole scriptlet is one
  compound operation, so it is steered to a single shell-less
  `ct and A ::: B ::: C` call (the `ct edit`s kept, the `grep -c` verifications
  becoming `ct search … --summary`). One atomic, verdict-gated call replaces a
  hand-sequenced script.
- **Advise the lines individually** — when some lines are ct-advisable but others
  are opaque: it can't fold whole, so the `ct` equivalents are listed for the
  steerable steps.
- A scriptlet that is *already* all `ct`, or has nothing serviceable, is left
  alone. (Backslash line-continuations are joined first, so a single wrapped
  command is not mistaken for a scriptlet.)

### Harness tools (opt-in via `--tools`)

Raw shell is not the only way around `ct` — the harness's own search/read tools
are another. When installed for them, the hook also steers:

| Harness tool | Steered to |
| --- | --- |
| `Grep` (pattern / path / glob) | `ct search --grep …` |
| `Glob` (`**/*.rs` → base + name) | `ct search --name … --type f` |
| `Read` (file_path / offset / limit → range) | `ct view [--range A:B]` |

`Read` of an image, PDF, or notebook (`.png/.jpg/.gif/.pdf/.ipynb/…`) is **left
alone** — `ct view` is a line reader and can't render those, so `Read` remains
the right tool. These matchers are off by default; enable them with
`install --tools Bash,Grep,Glob,Read`.

> **Read gating and the harness `Edit` tool.** Steering `Read` to `ct view` in `deny`/`ask` mode blocks the harness read that the `Edit` tool's must-read-first precondition depends on — so after a steered `Read` you must mutate via `ct edit` (the intended pairing). To observe `Read` for logging without disrupting `Edit`, gate it in `--mode warn`, which only injects a suggestion and lets the read proceed.

## Subcommands

### `hook`

The runtime hook. Reads a Claude Code `PreToolUse` tool-call envelope as JSON on
**stdin** (`{ "tool_name": "Bash", "tool_input": { "command": "…" }, … }`, or a
`Grep`/`Glob`/`Read` envelope with its own fields) and, on a match, prints a
decision object on **stdout** and exits `0`:

- `--mode deny` *(default)* — `permissionDecision: "deny"` with the `ct`
  suggestion as the reason; the call is blocked and the agent re-issues.
- `--mode ask` — `permissionDecision: "ask"`; a confirmation prompt naming the
  suggestion.
- `--mode warn` — no decision, just `additionalContext` carrying the suggestion;
  the command still runs.

On a miss (or a non-`Bash` tool, or malformed input) it prints nothing and exits
`0`. This is the command wired into settings; you rarely run it by hand.

**Tool-call logging (on by default).** The hook appends one JSON object per line
to a daily log — a record of **every** call it sees, the silent *allows*
included. That allow stream is the point: it is the raw material for spotting
shell idioms that *should* have been steered to `ct` but no rule yet covers. Each
record carries `tool`, `command` (for `Bash`), `cwd`, `session_id`, the
`decision` (`deny`/`ask`/`warn`/`allow`), the `rule_id` and `ct_tool` when
steered, and a `ts_ms` timestamp.

- **Location.** Logs default to `.ct/tclog/` under the nearest `.ct` directory
  (git-style upward discovery; created if absent). Files rotate daily as
  `<yyyy-mm-dd>.jsonl` (UTC). The log directory is kept out of version control
  automatically: the hook ensures `.ct/.gitignore` carries a `*log` rule, which
  matches the `tclog` directory.
- **Controls.** `--log-dir DIR` (or the `CT_STEER_LOG` environment variable)
  redirects the logs to another directory — one you manage yourself, so its
  gitignore is left untouched. `--no-log` disables logging entirely.
- **Coverage.** The hook only sees the tools it is installed for, so pair logging
  with `install --all-tools` (a single `*` matcher) to capture the full tool
  stream; recognised idioms are still steered, everything else passes through and
  is merely recorded.

Logging is **best-effort and fail-open** — a write error never disturbs the tool
call. Analyse a day's log with the suite itself, e.g.
`ct search --base .ct/tclog --grep '"decision":"allow"'`.

**Background-Bash limitation.** The hook gates the `Bash` tool, but a host may
not run `PreToolUse` for **backgrounded** tool calls — so a `for … sleep … done`
watcher launched in the background can slip past ungated. The durable fix is
behavioural: launch a bounded wait as `ct await` (itself backgrounded), never a
hand-rolled `sleep` loop. The `wait-loop` rule above steers the foreground form
toward exactly that.

**Pipeline nudge (`--nudge-pipelines`).** With this flag, a `Bash` command that
contains a shell pipe **but that no specific rule steered** gets a **warn-only**
nudge (`additionalContext`, never a deny) prompting the agent to try harder to
express the task with `ct` — a single `ct` call, or a `ct and A ::: B` chain —
before falling back to a pipe. It fires only when the specific idiom matcher
found nothing, so it never double-fires with the concrete rewrites above.

### `post`

The PostToolUse **recorder**. Reads a `PostToolUse` envelope on stdin and appends
a record of the call *as it actually executed* to the same daily `.ct/tclog` log,
tagged `event: "post"` with a `ct` boolean (did the executed command use `ct`?),
`tool`, `command`, `cwd`, `session_id`, and `ts_ms`. It only observes — it prints
nothing and always exits `0`. Paired with the `pre` records the hook writes, this
lets you **measure whether steer guidance was followed**: after a `pre` record
shows a `deny`/`warn` on a raw command, did the next `post` record in the same
`session_id` show a `ct` call? Wire it with `install --measure`; it shares
`--log-dir`/`--no-log` with the hook.

```sh
# the executed calls that actually went to ct (the follow-through signal)
ct search --base .ct/tclog --grep '"ct":true'
```

### `install` / `uninstall`

Merge or remove the Bash `PreToolUse` hook in a Claude Code settings file.

- `--scope project` *(default)* → `.claude/settings.json`
- `--scope local` → `.claude/settings.local.json`
- `--scope user` → `~/.claude/settings.json`
- `--mode deny|ask|warn` — baked into the installed hook command (`install`).
- `--tools Bash,Grep,Glob,Read` *(default `Bash`)* — which tools to gate; one
  `PreToolUse` matcher entry is written per tool. `Grep`/`Glob` steer to
  `ct search`, `Read` to `ct view`.
- `--all-tools` — gate every tool under a single `*` matcher (supersedes
  `--tools`), so the default logging records the full tool stream, not just the
  steerable tools.
- `--nudge-pipelines` — bake the warn-only pipeline nudge (above) into the
  installed hook command.
- `--measure` — also install a `PostToolUse` `*` matcher running `ct steer post`,
  so executed calls are recorded for effectiveness analysis. `uninstall` removes
  both the steer hook and the recorder in one pass.
- `--log-dir DIR` — bake a `--log-dir` override into the installed hook command
  (logging is on by default to `.ct/tclog`, so this is only for redirecting it).
- `--no-log` — bake `--no-log` into the installed hook command, disabling the
  default tool-call logging.
- `--pin` — bake the **absolute path of this `ct-steer` binary** into the hook
  instead of resolving `ct` on `PATH`, so a version-skewed or missing `ct` can't
  break it.
- `--force` — skip the install preflight (below) and install anyway.
- `--dry-run` — show the resulting settings file without writing it.
- `--print` — emit just the hook snippet (for manual paste) and exit.

**Preflight (and why it matters).** A hook runs `ct steer …` on *every* tool call,
resolving `ct` from `PATH` at fire time. If that `ct` is an older build that does
not understand the subcommand or a baked flag (`post`, `--nudge-pipelines`,
`--log-dir`, …), the call fails at argument parsing — *before* the fail-open logic
runs — and a hook error is **blocking**, so a skewed `ct` can block every tool call
in the session. To prevent arming that, a real `install` first runs the resolving
`ct steer <sub> --help` and checks the subcommand and baked flags parse; if not, it
**refuses** (exit non-zero) with a recovery hint. Use `--pin` to eliminate the skew
(absolute path), or `--force` to install regardless. Recovery is always possible:
`ct steer uninstall` uses only long-stable syntax, so a working `ct` can always
clear a bad hook.

The merge is **idempotent** (re-installing is a no-op; a `--mode` change rewrites
in place) and preserves the rest of the file — **including comments and layout**.
It edits through `ct-patch`'s byte-range splices rather than reserialising, so a
hand-commented `settings.json` survives untouched except for the hook entry.

### `check`

Classify a command string and print what the hook would decide:

```sh
ct steer check 'grep -r TODO src'   # → DENY [grep-recursive] — ct search …  (exit 1)
ct steer check 'git status'         # → ALLOW                                (exit 0)
```

Exit `0` means the command is allowed; `1` means it would be steered. `--mode`
sets the printed label; `--json` prints the decision JSON. Useful for testing a
rule or scripting a gate.

## Global flags

`--json` structures the `install`/`uninstall` outcome and the `check` decision;
`--quiet` suppresses informational lines (exit status still reports); `--timeout`
SECS bounds the run (exit 2 on overrun). `--heartbeat` SECS prints a liveness
pulse while running, with `--heartbeat-emit` setting its template and
`--heartbeat-to` its stream (`stderr`/`stdout`). `--explain [md|json]` prints
this document or the MCP tool-use definition.

## Setup

```sh
ct steer install                          # deny-mode Bash hook → .claude/settings.json
ct steer install --mode ask               # softer: ask instead of deny
ct steer install --tools Bash,Grep,Glob,Read  # also gate the harness search/read tools
ct steer install --all-tools --mode warn      # log every call, steer non-intrusively
ct steer install --no-log                     # opt out of the default tool-call logging
ct steer install --print                      # see the snippet without writing
ct steer uninstall                            # remove every steer matcher
```

Tool-call logging is on by default (`.ct/tclog/<yyyy-mm-dd>.jsonl`). Analyse a
day's log to find un-steered patterns worth a new rule:

```sh
ct search --base .ct/tclog --grep '"decision":"allow"'  # the misses to mine
```

## Examples

- **Log every tool call to .ct/tclog and non-intrusively steer recognized idioms.**
  ```sh
  ct steer install --all-tools --mode warn
  ```
- **Ask what the hook would do with a command (exit 1 means it would steer to ct).**
  ```sh
  ct steer check 'grep -r TODO src'
  ```

## Exit status

`0` success (or `check`: command allowed); `1` `check` would steer the command;
`2` usage or runtime error.
