# ctsearch — Coding Tools Search

> Recursively find files by name, type, size, and content from a chosen root.
> One declarative command in place of a `find … | xargs grep …` pipeline.

`ctsearch` combines the predicates you normally assemble from `find`, `xargs`,
and `grep`. You state *what* you are looking for; the tool handles the
traversal, the per-file work, and the reporting. An entry matches only when
**all** supplied predicates hold. Output defaults to a list of matching paths;
the exit status reports whether anything matched.

This document is the canonical reference for `ctsearch`. It is also what the tool
prints for `ctsearch --explain` (`--explain md`); `ctsearch --explain json`
prints the equivalent MCP / tool-use definition.

## When to use it

- Search a tree that is **not** the current directory, without `cd`-ing first (`--base`).
- Combine "name looks like X" **and** "contents contain Y" **and** "bigger than Z" in one pass.
- Ask only *"does anything match?"* (`--quiet` + exit code) or *"how many?"* (`--summary`).

## Replaces patterns like

```sh
cd somedir \
  && find . -type f \( -name "*.java" -o -name "*.kt" \) \
  | xargs grep -l "SimpleMFD\|knn_entries\|DataSetLoaderSimple" 2>/dev/null \
  | head -20
```

with:

```sh
ctsearch --base somedir \
  --type f \
  --name '*.java|*.kt' \
  --grep 'SimpleMFD|knn_entries|DataSetLoaderSimple' \
  --limit 20 \
  --list
```

## Options

### Predicates

An entry matches only when **all** supplied predicates hold.

| Option       | Argument  | Meaning                                                                                     |
| ------------ | --------- | ------------------------------------------------------------------------------------------- |
| `--base`     | `DIR`     | Search root, relative or absolute, regardless of the CWD at launch. Default: `.`            |
| `--name`     | `PATTERN` | Match the entry's file name. `\|`-separated alternatives; each is promoted (see *Pattern matching*) and anchored to the whole name. |
| `--type`     | `KINDS`   | Restrict to entry kinds: `f` (file), `d` (directory), `l` (symlink). Repeatable or comma-joined (`--type f,l`). |
| `--grep`     | `PATTERN` | Match file **contents** (promoted; searched unanchored). Implies regular files.             |
| `--size`     | `EXPR`    | Size predicate `[+\|-]N[k\|m\|g]`: `+N` larger than, `-N` smaller than, `N` at least N. Applies to regular files. |
| `--hidden`   | —         | Include dot-entries (names starting with `.`). Default: skipped, and dot-directories are not descended into. |
| `--follow`   | —         | Follow symlinks while traversing.                                                           |
| `--limit`    | `N`       | Stop after `N` matches.                                                                      |

### Output mode

Mutually exclusive; defaults to `--list`.

| Option      | Output                                                              |
| ----------- | ------------------------------------------------------------------ |
| `--list`    | One matching path per line. *(default)*                            |
| `--summary` | Counts only — files matched, and with `--grep`, total matching lines. |
| `--detail`  | Matching paths plus, for `--grep`, each hit as `path:line:text`.   |
| `--quiet`   | No output; communicate via exit status only.                       |

### Documentation

| Option                 | Effect                                                            |
| ---------------------- | ---------------------------------------------------------------- |
| `--explain [md\|json]` | Print this guide (`md`, default) or the MCP tool definition (`json`), then exit. |
| `-h`, `--help`         | Human help.                                                      |
| `-V`, `--version`      | Version.                                                         |

## Pattern matching

Every pattern argument (`--name`, `--grep`) is promoted to a regular expression
with one predictable rule — write the simplest thing that expresses your intent:

| The pattern contains…                                  | …it is treated as | Match semantics                          |
| ------------------------------------------------------ | ----------------- | ---------------------------------------- |
| no metacharacters at all                               | literal substring | matched verbatim (regex-escaped)         |
| glob metacharacters only, and is **not** a valid regex | glob              | converted to an equivalent regex         |
| regex metacharacters, and **is** a valid regex         | regex             | used exactly as written                  |

* **Glob metacharacters:** `*` `?` `[ … ]` — and `*`/`?` do not cross `/`.
* **Regex metacharacters:** `^ $ ( ) | + { } \ .`
* `--name` matches are **anchored to the whole name** (so `*.java` means "ends in
  `.java`"); `--grep` matches are **unanchored** (match anywhere).
* `--name` accepts `|`-separated alternatives (`*.java|*.kt`), matching any;
  `--grep` keeps `|` as ordinary regex alternation.

Examples: `ERROR:` → literal; `*.java` → glob (a leading `*` is not a valid
regex); `^ERROR`, `foo|bar`, `\d+` → regex.

## Exit status

| Code | Meaning                            |
| ---- | ---------------------------------- |
| `0`  | at least one entry matched         |
| `1`  | nothing matched                    |
| `2`  | usage or runtime error             |

The `0`/`1` split lets you chain `ctsearch` in `&&`/`||` pipelines without
parsing output; a distinct `2` keeps real errors from looking like a clean "no
match".

## Examples

```sh
# Any Rust file under ./src mentioning "TODO" — just tell me yes/no.
ctsearch --base src --type f --name '*.rs' --grep TODO --quiet

# Count config files larger than 4 KiB anywhere under the repo.
ctsearch --name '*.toml|*.yaml|*.json' --size +4k --summary

# Detailed grep-style report, capped at 20 hits, across Java/Kotlin.
ctsearch --name '*.java|*.kt' --grep 'load(Simple|Bulk)Data' --detail --limit 20

# Hand an agent the machine-readable tool definition.
ctsearch --explain json
```
