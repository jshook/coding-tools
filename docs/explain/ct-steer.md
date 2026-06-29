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
| `sed -i`, `perl -i` | `ct edit` (preview + `--expect` gate) |
| `head`/`tail`/`sed -n 'A,Bp'` on a file | `ct view --range` |
| `ls -R`, `tree` | `ct tree` |
| `wc -l` over files | `ct tree --summary` |
| `for … do … done`, `while read` | `ct each` |
| `A && B`, `A \|\| B` (every segment ct-serviceable) | `ct and` / `ct or` |

A chain (`&&` / `||`) is only steered when *every* segment is itself
ct-serviceable, so `grep -r x && make` (no `ct` analogue for `make`) is left
alone while `grep -r x && sed -i …` becomes `ct and search … ::: edit …`.

## Subcommands

### `hook`

The runtime hook. Reads a Claude Code `PreToolUse` tool-call envelope as JSON on
**stdin** (`{ "tool_name": "Bash", "tool_input": { "command": "…" }, … }`) and,
on a match, prints a decision object on **stdout** and exits `0`:

- `--mode deny` *(default)* — `permissionDecision: "deny"` with the `ct`
  suggestion as the reason; the call is blocked and the agent re-issues.
- `--mode ask` — `permissionDecision: "ask"`; a confirmation prompt naming the
  suggestion.
- `--mode warn` — no decision, just `additionalContext` carrying the suggestion;
  the command still runs.

On a miss (or a non-`Bash` tool, or malformed input) it prints nothing and exits
`0`. This is the command wired into settings; you rarely run it by hand.

### `install` / `uninstall`

Merge or remove the Bash `PreToolUse` hook in a Claude Code settings file.

- `--scope project` *(default)* → `.claude/settings.json`
- `--scope local` → `.claude/settings.local.json`
- `--scope user` → `~/.claude/settings.json`
- `--mode deny|ask|warn` — baked into the installed hook command (`install`).
- `--dry-run` — show the resulting settings file without writing it.
- `--print` — emit just the hook snippet (for manual paste) and exit.

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
ct steer install            # add the deny-mode hook to .claude/settings.json
ct steer install --mode ask # softer: ask instead of deny
ct steer install --print    # see the snippet without writing
ct steer uninstall          # remove it
```

## Exit status

`0` success (or `check`: command allowed); `1` `check` would steer the command;
`2` usage or runtime error.
