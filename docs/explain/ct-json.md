# ct-json — JSON Query

> Ask a JSON document a question and get back exactly the values that answer
> it — one per line, counted, and framed by an `--expect` verdict — instead of
> writing a `node -e` / `python -c` one-liner or shelling out to `jq`.

`ct-json` reads one or more JSON-family documents (JSON, JSONC, JSONL) and runs
a **jq filter** over them, in-process (the [jaq](https://github.com/01mf02/jaq)
dialect of jq: no `jq` binary, no shell, no pipe). `--path` addresses a node in
the same path grammar `ct-patch` uses to `--set` it, so what you read is what
you would write; `--query` is any jq program for the projections, joins and
aggregations a path cannot express. It is read-only, so it carries no
allow-gate — and it is itself on the read-only allowlist, so `ct test`,
`ct each`, `ct await`, and rule probes can dispatch it. Reachable directly or
as `ct json`.

This document is the canonical reference for `ct-json`. It is also what the
tool prints for `ct-json --explain` (`--explain md`); `ct-json --explain json`
prints the equivalent MCP / tool-use definition.

## When to use it

- Read one node of a config or manifest (`--path hooks.PreToolUse[0].matcher`)
  instead of viewing the whole file.
- Project an array to the fields you care about, one record per line.
- Join two arrays of the same document (a lookup table built from one, applied
  to the other) — the case that used to need a scratch `.js` file.
- Aggregate JSONL logs (`--slurp` + `group_by`) without a loop.
- Assert a shape: *"exactly 12 wires"*, *"no node without a type"* — with
  `--expect`, so the answer is a verdict, not something to eyeball.

## Options

| Option          | Argument       | Meaning                                                                                              |
| --------------- | -------------- | ---------------------------------------------------------------------------------------------------- |
| `--file`        | `PATH\|SCHEME` | The document to query (**repeatable**; each one is an input). A bare value is a path; `file:PATH`, `stdin:`, `b64:DATA`, and `text:JSON` carry the document itself (see *Payload schemes*). |
| `--query`       | `FILTER\|SCHEME` | The jq filter to run. Default `.` (or just the `--path` node). Payload-typed too: `b64:` carries a filter full of quotes and `$` through any shell untouched. |
| `--path`        | `PATH`         | Read this node first, in `ct-patch`'s grammar — dotted keys, `[N]`, `[key=value]` — plus `[*]` for every element. `--query` then runs on each node selected. |
| `--arg`         | `NAME=VALUE`   | Bind `$NAME` to the string `VALUE` (**repeatable**; `VALUE` is payload-typed).                     |
| `--argjson`     | `NAME=JSON`    | Bind `$NAME` to `JSON`, parsed (**repeatable**).                                                   |
| `--format`      | `json\|jsonc\|jsonl` | Force the input format instead of detecting it from the extension.                             |
| `--slurp`       | —              | Collect every input (each `--file` document, every JSONL record) into one array and run the filter once over it. |
| `--raw`         | —              | Print string results as their own characters, unquoted (jq's `-r`).                               |
| `--pretty`      | —              | Indent each result over several lines instead of one compact value per line.                      |
| `--limit`       | `N`            | Stop after N results; the filter is not evaluated further, so an open-ended generator stays bounded. |
| `--expect`      | `SPEC`         | Verdict over the number of results: `any` (default), `none`, `N`, `=N`, `+N`, `-N`.               |
| `--quiet`       | —              | Suppress the results; print only the one-line summary.                                             |
| `--json`        | —              | Emit a structured JSON result instead of the bare values (see *JSON result*).                     |
| `--json-pretty` | —              | Like `--json`, but pretty-printed (indented).                                                      |

### Documentation

| Option                 | Effect                                                            |
| ---------------------- | ---------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                      |
| `-V`, `--version`      | Version.                                                         |

## Inputs

Every `--file` is one input. The format comes from `--format`, else from the
extension: `.json` and `.jsonc` are one document each, `.jsonl`/`.ndjson` feed
every non-blank line as its own input (reported as `file:LINE` in messages),
and any other extension — or a scheme-carried document — reads as JSON. A
`.yaml`/`.yml` file is refused rather than mis-read; `ct-json` is JSON-family
only. All three parse through the same lenient tree `ct-patch` edits, so
comments and trailing commas are tolerated everywhere, and **object keys keep
their document order**, as jq keeps them.

The filter runs once per input, in order, and its results are concatenated.
`--slurp` instead gathers every input into one array and runs the filter once
— the shape for "across all these records" questions.

### Payload schemes

`--file`, `--query`, and `--arg` values resolve through the suite's payload
schemes, so none of them ever has to survive shell quoting:

| Prefix       | Meaning                                                                |
| ------------ | ---------------------------------------------------------------------- |
| *(none)*     | `--file`: a path to read. `--query`/`--arg`: the value itself.        |
| `file:PATH`  | Read the value verbatim from a file.                                    |
| `stdin:`     | Read the value verbatim from standard input (`--file stdin:` queries a piped document). |
| `b64:DATA`   | Decode a base64 string (standard or URL-safe alphabet, padding optional). The way to hand over a filter full of `"`, `$`, and backticks from a harness that mangles them. |
| `text:VALUE` | The literal `VALUE`, escaping a value that genuinely begins with a prefix. |

## `--path`: the `ct-patch` grammar, read-only

`--path` is parsed by the same code as `ct-patch`'s `--set`/`--delete` paths,
so the two cannot drift: dot-separated keys, `[N]` array indices, and
`[key=value]` to pick the **first** array element whose `key` equals `value`
(`value` is read as JSON when it is one — `[port=80]`, `[enabled=true]` — else
as a string, the same promotion `ct-patch` gives a `--set` value). A leading
`.` is optional. One addition `ct-patch` does not take: `[*]` selects **every**
element, so `wires[*].to_node` yields one value per wire.

Under the hood a path becomes a filter — `servers[name=web].port` is
`.["servers"] | first(.[] | select(.["name"] == "web")) | .["port"]` — and
`--query`, if given, is piped after it. A path that selects nothing yields
nothing (a clean `ERROR` verdict under the default `--expect any`), rather than
an error.

## `--query`: jq, in-process

`--query` is a jq program in the jaq dialect — the jq core language plus its
standard library (`map`, `select`, `group_by`, `to_entries`, `test`, string
interpolation `"\(...)"`, `reduce`, `limit`, `first`, …). Three jaq-specific
things to know: it is strict where jq is lenient (`.a` on a number is an
error, not `null`-ish silence), number literals print as written (`1.5e3`
stays `1.5e3`), and there is no `input`/`inputs` — the suite feeds inputs
itself (`--file` repeatable, `--slurp` to see them all at once).

A filter error (indexing a number, a `halt`) stops the run with exit `2` and
the message names the input it happened on. Variables come from `--arg`
(always a string) and `--argjson` (parsed); each name may be bound once.

## Output

By default each result is one line of compact JSON, in order. `--raw` prints a
string result as its characters (so `--raw --query '.name'` gives a bare name,
and a multi-line string spans lines); non-strings are unaffected. `--pretty`
indents every result over several lines (two spaces, a space after each colon).
`--limit N` caps the results — and stops evaluating, so `--query 'repeat(1)'
--limit 3` returns.

The verdict is `--expect` applied to the **number of results** (after
`--limit`): `any` (the default, ≥1), `none`, `N` (≥N), `=N`, `+N` (>N), `-N`
(<N). When it is `ERROR` a one-line summary goes to stderr after the values
(`ct-json: 0 value(s) -> ERROR (--expect any)`); `--quiet` replaces the values
with that summary on stdout.

## JSON result

`--json` emits a single object:

```json
{
  "tool": "ct-json",
  "verdict": "SUCCESS",
  "count": 2,
  "files": ["patch.json"],
  "filter": ".[\"wires\"] | .[]",
  "values": [ { "from_node": 1, "to_node": 2 }, { "from_node": 2, "to_node": 3 } ]
}
```

`filter` is the exact program that ran (`--path` translated, then `--query`),
`files` the input labels, `values` the results as JSON. Note that the suite's
JSON writer sorts object keys inside `values`; the text form keeps document
order.

## Run bounds and liveness

Every suite tool is bounded and observable the same way:

| Option             | Argument   | Effect                                                            |
| ------------------ | ---------- | ----------------------------------------------------------------- |
| `--timeout`        | `SECS`     | Abort the run (exit `2`, with a one-line message) if it exceeds SECS seconds (fractional allowed). |
| `--heartbeat`      | `SECS`     | Print a liveness pulse every SECS seconds while the run is in progress. |
| `--heartbeat-emit` | `TEMPLATE` | Pulse template. Tokens: `{ELAPSED}` (whole seconds so far) `{TOOL}`. Default: `[{ELAPSED}s]`. |
| `--heartbeat-to`   | `stderr\|stdout` | Stream for pulses. Default: `stderr`.                       |

## Exit status

| Code | Meaning                                                               |
| ---- | --------------------------------------------------------------------- |
| `0`  | the verdict is `SUCCESS` (the result count met `--expect`)            |
| `1`  | the verdict is `ERROR` (a clean negative: e.g. the node is absent)     |
| `2`  | usage or runtime error (unreadable file, invalid filter, filter error) |

## Examples

- **List every wire, one JSON object per line, and assert there are exactly 12 — instead of a node -e loop over JSON.parse.**
  ```sh
  ct json --file patch.json --path 'wires[*]' --expect =12
  ```
- **Join two arrays of one document: print each wire with node ids resolved to type names through a lookup built from .nodes — the case that used to need a scratch .js file.**
  ```sh
  ct json --file patch.json --query '(.nodes | map({(.id|tostring): .type_id}) | add) as $t | .wires[] | "\($t[.from_node|tostring]).\(.from_port) -> \($t[.to_node|tostring]).\(.to_port)"' --raw
  ```
- **Read one node of a JSONC config by the same path ct patch would set it with, instead of viewing the whole file.**
  ```sh
  ct json --file .claude/settings.json --path 'hooks.PreToolUse[0].matcher' --raw
  ```
- **Count JSONL records per level in one call, instead of grep -c per level.**
  ```sh
  ct json --file events.jsonl --slurp --query 'group_by(.level) | map({level: .[0].level, n: length})'
  ```
- **Query a document arriving on a pipe, and bind a threshold without quoting it into the filter.**
  ```sh
  ct json --file stdin: --query '.items[] | select(.size > $min) | .name' --argjson min=1024 --raw
  ```

The join example is the whole of what used to be:

```js
const p = JSON.parse(fs.readFileSync('patch.json'));
const byId = Object.fromEntries(p.nodes.map(n => [n.id, n.type_id]));
for (const w of p.wires)
  console.log(`${byId[w.from_node]}.${w.from_port} -> ${byId[w.to_node]}.${w.to_port}`);
```

— and when that filter has to cross a harness that rewrites `$` or backticks,
`--query b64:<the filter, base64-encoded>` carries it intact.

## Composing with the suite

- `ct json` is read-only and on the allowlist, so `ct test --cmd ct-json …`
  frames a shape check as an experiment, `ct each … -- ct-json --file {ITEM} …`
  sweeps many documents, and a rule probe can assert a manifest invariant.
- `ct patch` writes what `ct json --path` reads: check a node, then set it, in
  the same grammar.
- `--expect none` turns "no record with this property" into a passing exit:
  `ct json --file manifest.json --query '.tools[] | select(.description == null)' --expect none`.
