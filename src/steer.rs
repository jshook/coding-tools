// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Redirection steering: recognise the ad-hoc shell idioms a `ct` tool serves
//! better, and (as a Claude Code `PreToolUse` hook) steer the agent to the
//! `ct` equivalent instead.
//!
//! Agents reach for raw shell — `find | xargs grep`, `sed -i`, `cat | head`,
//! `for`/`while` loops, sleep-polling waits, `wc -l` counts, and `python -c`/
//! `jq` file reads — even when a suite tool would do the job bounded,
//! deterministic, and self-verifying. [`analyze`] is the pure heart: it
//! classifies a shell
//! command string into an optional [`Steer`] naming the `ct` tool that serves
//! it and a best-effort equivalent command. The [`hook`] submodule wraps that
//! in the Claude Code `PreToolUse` JSON protocol (deny / ask / warn); the
//! [`install`] submodule wires the hook into a project's `.claude/settings.json`.
//!
//! The matcher is deliberately **conservative**: it only fires on a fixed set
//! of high-confidence 1:1 idioms, never re-steers a command that already
//! invokes `ct`, and returns [`None`] (allow) whenever it is unsure. The hook
//! is **fail-open** — any malformed input or unrecognised command is allowed —
//! because it runs ahead of *every* shell call.

/// What the hook does when a command matches a steering rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Block the call and feed the `ct` suggestion back to the agent (default).
    #[default]
    Deny,
    /// Surface a confirmation prompt naming the `ct` suggestion.
    Ask,
    /// Allow the call, but inject the `ct` suggestion as context.
    Warn,
}

impl Mode {
    /// Parse the `--mode` value.
    ///
    /// ```
    /// use coding_tools::steer::Mode;
    /// assert_eq!(Mode::from_name("ask"), Some(Mode::Ask));
    /// assert_eq!(Mode::from_name("nope"), None);
    /// ```
    pub fn from_name(s: &str) -> Option<Mode> {
        match s {
            "deny" => Some(Mode::Deny),
            "ask" => Some(Mode::Ask),
            "warn" => Some(Mode::Warn),
            _ => None,
        }
    }

    /// The canonical name, as accepted by `--mode` and written into settings.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Deny => "deny",
            Mode::Ask => "ask",
            Mode::Warn => "warn",
        }
    }
}

/// A steering match: a `ct` tool serves the inspected command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Steer {
    /// Stable identifier for the rule that fired (e.g. `"find-grep"`).
    pub rule_id: &'static str,
    /// The `ct` tool that serves the idiom (e.g. `"ct search"`).
    pub tool: &'static str,
    /// A best-effort equivalent `ct` command line.
    pub suggestion: String,
    /// One line teaching why the `ct` tool is the better fit.
    pub note: &'static str,
}

impl Steer {
    /// The reason text shown to the agent (the `ct` command plus the lesson).
    pub fn reason(&self) -> String {
        format!(
            "A `ct` tool serves this more reliably — bounded, deterministic, \
             and self-verifying. Use instead:\n  {}\n({})",
            self.suggestion, self.note
        )
    }
}

// ----- Lexing ------------------------------------------------------------------

/// A shell token: either a word (quoted regions collapse into the surrounding
/// word, so operators inside quotes are inert) or one of the control operators
/// we split on. Redirections and grouping are dropped to word boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    Pipe,
    And,
    Or,
    Semi,
}

/// Tokenise a command string. Single/double quotes and backslash escapes keep
/// their contents inside the current word, so `echo "a | b"` is one word and
/// the `|` does not register as a pipe.
fn lex(cmd: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut have = false; // cur holds a word (possibly empty, from `""`)
    let mut chars = cmd.chars().peekable();

    fn flush(toks: &mut Vec<Tok>, cur: &mut String, have: &mut bool) {
        if *have {
            toks.push(Tok::Word(std::mem::take(cur)));
            *have = false;
        }
    }

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                have = true;
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    cur.push(d);
                }
            }
            '"' => {
                have = true;
                while let Some(d) = chars.next() {
                    if d == '"' {
                        break;
                    }
                    if d == '\\' {
                        if let Some(e) = chars.next() {
                            cur.push(e);
                        }
                    } else {
                        cur.push(d);
                    }
                }
            }
            '\\' => {
                if let Some(d) = chars.next() {
                    cur.push(d);
                    have = true;
                }
            }
            '|' => {
                flush(&mut toks, &mut cur, &mut have);
                if chars.peek() == Some(&'|') {
                    chars.next();
                    toks.push(Tok::Or);
                } else {
                    toks.push(Tok::Pipe);
                }
            }
            '&' => {
                flush(&mut toks, &mut cur, &mut have);
                if chars.peek() == Some(&'&') {
                    chars.next();
                    toks.push(Tok::And);
                } else {
                    toks.push(Tok::Semi); // a lone `&` (background) ends a command
                }
            }
            ';' => {
                flush(&mut toks, &mut cur, &mut have);
                toks.push(Tok::Semi);
            }
            // Redirections and grouping: end the current word, drop the symbol.
            '>' | '<' | '(' | ')' | '{' | '}' | '`' => {
                flush(&mut toks, &mut cur, &mut have);
            }
            c if c.is_whitespace() => flush(&mut toks, &mut cur, &mut have),
            _ => {
                cur.push(c);
                have = true;
            }
        }
    }
    flush(&mut toks, &mut cur, &mut have);
    toks
}

/// Split a token stream into control segments (on `&&` / `||` / `;`) plus the
/// list of joiner operators between them (length = segments − 1).
fn control_segments(toks: &[Tok]) -> (Vec<Vec<Tok>>, Vec<Tok>) {
    let mut segs = vec![Vec::new()];
    let mut joiners = Vec::new();
    for t in toks {
        match t {
            Tok::And | Tok::Or | Tok::Semi => {
                joiners.push(t.clone());
                segs.push(Vec::new());
            }
            other => segs.last_mut().unwrap().push(other.clone()),
        }
    }
    // Drop a trailing empty segment (e.g. a command ending in `;`).
    if segs.last().is_some_and(Vec::is_empty) {
        segs.pop();
        joiners.pop();
    }
    (segs, joiners)
}

/// Split one control segment into pipeline stages (on `|`); each stage is its
/// word list.
fn pipe_stages(seg: &[Tok]) -> Vec<Vec<String>> {
    let mut stages = vec![Vec::new()];
    for t in seg {
        match t {
            Tok::Pipe => stages.push(Vec::new()),
            Tok::Word(w) => stages.last_mut().unwrap().push(w.clone()),
            _ => {}
        }
    }
    stages
}

// ----- Word/flag helpers -------------------------------------------------------

/// The basename of a command word (`/usr/bin/grep` → `grep`).
fn base_name(w: &str) -> &str {
    w.rsplit(['/', '\\']).next().unwrap_or(w)
}

/// The command name of a stage (its first word, basename-stripped).
fn cmd_of(stage: &[String]) -> Option<&str> {
    stage.first().map(|w| base_name(w))
}

/// Whether a stage carries a single-dash short flag containing `ch` (so `-r`,
/// `-rn`, `-Rl` all count for `'r'`), excluding `--long` words.
fn has_short(stage: &[String], ch: char) -> bool {
    stage
        .iter()
        .any(|w| w.starts_with('-') && !w.starts_with("--") && w[1..].chars().any(|c| c == ch))
}

/// Whether a stage carries `flag` exactly, or `flag=…`.
fn has_flag(stage: &[String], flag: &str) -> bool {
    stage
        .iter()
        .any(|w| w == flag || w.starts_with(&format!("{flag}=")))
}

/// The value of `-flag VALUE`, `--flag VALUE`, or `--flag=VALUE` in a stage.
fn flag_value<'a>(stage: &'a [String], names: &[&str]) -> Option<&'a str> {
    for (i, w) in stage.iter().enumerate() {
        for n in names {
            if w == n {
                return stage.get(i + 1).map(String::as_str);
            }
            let eq = format!("{n}=");
            if let Some(v) = w.strip_prefix(&eq) {
                return Some(v);
            }
        }
    }
    None
}

/// The positional (non-flag) words of a stage after its command. Imperfect —
/// a value-taking flag's value (e.g. the `40` in `head -n 40`) leaks through —
/// so callers that care filter further.
fn positionals(stage: &[String]) -> Vec<&str> {
    stage
        .iter()
        .skip(1)
        .filter(|w| !w.starts_with('-'))
        .map(String::as_str)
        .collect()
}

/// A `find` start path: the first argument, when it is not a `-option`
/// (`find <path> -name …`; a bare `find -name …` defaults to the cwd).
fn find_base(find: &[String]) -> Option<&str> {
    find.get(1)
        .filter(|w| !w.starts_with('-'))
        .map(String::as_str)
}

/// Single-quote a value for display inside a suggested command.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ----- Rules -------------------------------------------------------------------

/// Classify a shell command. [`None`] means "allow" — no `ct` tool clearly
/// serves it. The matcher only fires on high-confidence idioms and never
/// re-steers a command that already invokes `ct`.
///
/// ```
/// use coding_tools::steer::analyze;
/// let s = analyze("find . -name '*.rs' | xargs grep TODO").unwrap();
/// assert_eq!(s.tool, "ct search");
/// assert!(analyze("cargo build && cargo test").is_none());
/// assert!(analyze("ct search --grep TODO").is_none());
/// ```
pub fn analyze(command: &str) -> Option<Steer> {
    let toks = lex(command);
    if toks.is_empty() {
        return None;
    }
    let (segs, joiners) = control_segments(&toks);
    let seg_stages: Vec<Vec<Vec<String>>> = segs.iter().map(|s| pipe_stages(s)).collect();

    // Never re-steer a command that already involves `ct` / `ct-*` anywhere
    // (as a command, or behind `xargs`/`env`/…). Erring toward allow here is
    // safe — at worst we decline to steer a grep that merely mentions `ct-…`.
    let touches_ct = seg_stages.iter().flatten().flatten().any(|w| {
        let b = base_name(w);
        b == "ct" || b.starts_with("ct-")
    });
    if touches_ct {
        return None;
    }

    // Shell loops (`for`/`while`/`until`) — a control word starting the first
    // segment. A loop whose body `sleep`s and re-probes is a bounded *wait*
    // (steer to `ct await`); any other loop is a per-item map (`ct each`).
    if let Some(first) = seg_stages
        .first()
        .and_then(|s| s.first())
        .and_then(|s| cmd_of(s))
        && matches!(first, "for" | "while" | "until")
    {
        let waits = seg_stages
            .iter()
            .flatten()
            .flatten()
            .any(|w| matches!(base_name(w), "sleep" | "usleep" | "Start-Sleep"));
        return Some(if waits {
            Steer {
                rule_id: "wait-loop",
                tool: "ct await",
                suggestion: "ct await --timeout <SECS> --every <N> -- <probe-argv>".to_string(),
                note: "ct await polls a read-only probe until it passes (or a timeout/abort fires) with no shell loop — and being the wait itself, it should be launched in the background, never wrapped in `for/while … sleep`",
            }
        } else {
            Steer {
                rule_id: "shell-loop",
                tool: "ct each",
                suggestion: "ct each --items <a> <b> -- <cmd-template-with-{ITEM}>".to_string(),
                note: "ct each runs a command template once per item with no shell and an aggregate --expect verdict",
            }
        });
    }

    // A single command (possibly with pipes): the common, high-value case.
    if segs.len() == 1 {
        return analyze_segment(&seg_stages[0]);
    }

    // A chain (`&&` / `||`): only steer when *every* segment is itself
    // ct-serviceable and the joiners are uniform, so `ct and`/`ct or`
    // reproduces it faithfully. A mixed chain (e.g. `grep -r x && make`) is
    // left alone.
    let matches: Vec<Steer> = seg_stages
        .iter()
        .filter_map(|st| analyze_segment(st))
        .collect();
    if matches.len() == segs.len() && !joiners.is_empty() {
        if joiners.iter().all(|j| *j == Tok::And) {
            return Some(chain_steer("ct and", &matches));
        }
        if joiners.iter().all(|j| *j == Tok::Or) {
            return Some(chain_steer("ct or", &matches));
        }
    }
    None
}

/// Build the chain suggestion from each segment's own `ct` suggestion, joined
/// with the suite's shell-less `:::` separator.
fn chain_steer(head: &'static str, parts: &[Steer]) -> Steer {
    let body = parts
        .iter()
        .map(|p| p.suggestion.trim_start_matches("ct ").to_string())
        .collect::<Vec<_>>()
        .join(" ::: ");
    let (rule_id, note) = if head == "ct and" {
        (
            "and-chain",
            "ct and runs each step in turn, stopping at the first failure — a shell-less && with no quoting",
        )
    } else {
        (
            "or-chain",
            "ct or runs each step in turn, stopping at the first success — a shell-less || with no quoting",
        )
    };
    Steer {
        rule_id,
        tool: head,
        suggestion: format!("{head} {body}"),
        note,
    }
}

/// Classify a single control segment (its pipeline stages). Rule order encodes
/// priority: the most specific idiom wins.
fn analyze_segment(stages: &[Vec<String>]) -> Option<Steer> {
    rule_find_grep(stages)
        .or_else(|| rule_grep_recursive(stages))
        .or_else(|| rule_grep_count(stages))
        .or_else(|| rule_sed_inplace(stages))
        .or_else(|| rule_read_range(stages))
        .or_else(|| rule_interpreter_read(stages))
        .or_else(|| rule_find_files(stages))
        .or_else(|| rule_list_recursive(stages))
        .or_else(|| rule_count_lines(stages))
}

/// `find … | xargs grep` / `find … -exec grep` → `ct search`.
fn rule_find_grep(stages: &[Vec<String>]) -> Option<Steer> {
    let find = stages.iter().find(|s| cmd_of(s) == Some("find"))?;
    // grep appearing anywhere (its own stage, after xargs, or after -exec).
    let grep_stage = stages
        .iter()
        .find(|s| s.iter().any(|w| base_name(w) == "grep"))?;
    let glob = flag_value(find, &["-name", "-iname"]);
    let pat = grep_pattern(grep_stage);
    Some(Steer {
        rule_id: "find-grep",
        tool: "ct search",
        suggestion: search_suggestion(find_base(find), glob, pat),
        note: "ct search recurses, filters by name/type/size, and greps in one declarative pass — find | xargs grep in a single command",
    })
}

/// `grep -r` / `rg` / `ag` → `ct search`.
fn rule_grep_recursive(stages: &[Vec<String>]) -> Option<Steer> {
    for s in stages {
        let Some(cmd) = cmd_of(s) else { continue };
        let recursive_grep =
            cmd == "grep" && (has_short(s, 'r') || has_short(s, 'R') || has_flag(s, "--recursive"));
        if recursive_grep || matches!(cmd, "rg" | "ripgrep" | "ag") {
            let pat = grep_pattern(s);
            // `grep -r PAT PATH` / `rg PAT PATH`: the second positional is the path.
            let base = positionals(s).get(1).copied();
            return Some(Steer {
                rule_id: "grep-recursive",
                tool: "ct search",
                suggestion: search_suggestion(base, None, pat),
                note: "ct search is the suite's recursive content search, with a framed --expect verdict (e.g. --expect none asserts absence)",
            });
        }
    }
    None
}

/// `grep -c PATTERN FILE` (count matching lines) → `ct search … --summary`.
fn rule_grep_count(stages: &[Vec<String>]) -> Option<Steer> {
    for s in stages {
        let Some(cmd) = cmd_of(s) else { continue };
        if matches!(cmd, "grep" | "egrep" | "fgrep") && has_short(s, 'c') {
            // `grep -c PATTERN FILE`: the second positional is the path.
            let base = positionals(s).get(1).copied();
            return Some(Steer {
                rule_id: "grep-count",
                tool: "ct search",
                suggestion: format!(
                    "{} --summary",
                    search_suggestion(base, None, grep_pattern(s))
                ),
                note: "ct search --summary reports the match count directly (and --expect +N|=N turns it into a pass/fail assertion), replacing grep -c",
            });
        }
    }
    None
}

/// An interpreter one-liner that READS a file — `jq EXPR FILE`,
/// `python -c '…open("x")…'`, `node -e`, `perl -e`, `ruby -e` — with no write
/// signal → `ct view` / `ct search`. Pure-compute one-liners (no file read) and
/// anything that looks like it writes are left alone.
fn rule_interpreter_read(stages: &[Vec<String>]) -> Option<Steer> {
    for s in stages {
        let Some(cmd) = cmd_of(s) else { continue };
        // `jq EXPR FILE…`: a file argument means it reads a file, not a stream.
        if cmd == "jq" {
            if let Some(&file) = positionals(s).get(1) {
                return Some(interpreter_steer(Some(file)));
            }
            continue;
        }
        // `python/node/perl/ruby -c|-e '<body>'`: inspect the inline script.
        let interp = matches!(
            cmd,
            "python" | "python3" | "node" | "nodejs" | "perl" | "ruby"
        );
        if interp
            && let Some(body) = flag_value(s, &["-c", "-e"])
            && reads_file(body)
            && !writes_file(body)
        {
            return Some(interpreter_steer(quoted_path(body)));
        }
    }
    None
}

/// `find … -name` with no grep → `ct search` (name filter only).
fn rule_find_files(stages: &[Vec<String>]) -> Option<Steer> {
    let find = stages.iter().find(|s| cmd_of(s) == Some("find"))?;
    let glob = flag_value(find, &["-name", "-iname"])?;
    let base = find_base(find);
    Some(Steer {
        rule_id: "find-files",
        tool: "ct search",
        suggestion: search_suggestion(base, Some(glob), None),
        note: "ct search selects files by --name/--type/--size and reports them, replacing a bare find",
    })
}

/// `sed -i` / `perl -i` → `ct edit`.
fn rule_sed_inplace(stages: &[Vec<String>]) -> Option<Steer> {
    let stage = stages.iter().find(|s| {
        let cmd = cmd_of(s);
        let sed_i =
            cmd == Some("sed") && s.iter().any(|w| w.starts_with("-i") || w == "--in-place");
        let perl_i = cmd == Some("perl") && s.iter().any(|w| w.starts_with("-i"));
        sed_i || perl_i
    })?;
    let (find, replace) = sed_subst(stage);
    let suggestion = match (find, replace) {
        (Some(f), Some(r)) => format!(
            "ct edit --base . --find {} --replace {} --expect =1 --dry-run",
            q(f),
            q(r)
        ),
        _ => "ct edit --base . --find <text> --replace <text> --expect =1 --dry-run".to_string(),
    };
    Some(Steer {
        rule_id: "sed-inplace",
        tool: "ct edit",
        suggestion,
        note: "ct edit previews the diff (--dry-run) and writes only when the match count matches --expect, so a wrong-sized in-place edit fails loudly instead of applying silently",
    })
}

/// `head`/`tail`/`sed -n 'A,Bp'` on a file → `ct view --range`.
fn rule_read_range(stages: &[Vec<String>]) -> Option<Steer> {
    // sed -n 'A,Bp'
    for s in stages {
        if cmd_of(s) == Some("sed")
            && has_flag(s, "-n")
            && let Some((a, b)) = positionals(s).into_iter().find_map(parse_sed_range)
        {
            let file = positionals(s).into_iter().find(|&w| !is_sed_script(w));
            return Some(view_steer(file, Some((a, b))));
        }
    }
    // head / tail, reading a named file or fed by `cat FILE`.
    for (i, s) in stages.iter().enumerate() {
        let cmd = cmd_of(s);
        if cmd != Some("head") && cmd != Some("tail") {
            continue;
        }
        let n = head_count(s);
        // The file is head/tail's own positional (not the numeric `-n` value),
        // or an upstream `cat FILE`.
        let own = positionals(s)
            .into_iter()
            .find(|w| w.parse::<u64>().is_err());
        let upstream = (i > 0 && cmd_of(&stages[i - 1]) == Some("cat"))
            .then(|| positionals(&stages[i - 1]).into_iter().next())
            .flatten();
        let file = own.or(upstream)?; // no concrete file → not a file read; skip
        let range = match (cmd, n) {
            (Some("head"), Some(n)) => Some((1, n)),
            _ => None, // tail = last-N lines; leave the range to the agent
        };
        return Some(view_steer(Some(file), range));
    }
    None
}

/// `ls -R` / `tree` → `ct tree`.
fn rule_list_recursive(stages: &[Vec<String>]) -> Option<Steer> {
    let stage = stages
        .iter()
        .find(|s| cmd_of(s) == Some("tree") || (cmd_of(s) == Some("ls") && has_short(s, 'R')))?;
    let base = positionals(stage).first().copied();
    let suggestion = match base {
        Some(b) => format!("ct tree --base {b}"),
        None => "ct tree".to_string(),
    };
    Some(Steer {
        rule_id: "list-recursive",
        tool: "ct tree",
        suggestion,
        note: "ct tree reports the file tree with per-file line/word/char counts, filtering and sorting — a richer, bounded ls -R / tree",
    })
}

/// `wc` over files (not a bare piped stream) → `ct tree`. Counts files named
/// directly, fed by `find`/`ls`, or read from a `cat FILE…` upstream; a stream
/// with no file behind it (e.g. `ps aux | wc -l`) is left alone.
fn rule_count_lines(stages: &[Vec<String>]) -> Option<Steer> {
    for (i, s) in stages.iter().enumerate() {
        if cmd_of(s) != Some("wc") {
            continue;
        }
        let has_files = !positionals(s).is_empty();
        let upstream = i.checked_sub(1).map(|j| &stages[j]);
        let from_find = upstream.is_some_and(|u| matches!(cmd_of(u), Some("find") | Some("ls")));
        let from_cat =
            upstream.is_some_and(|u| cmd_of(u) == Some("cat") && !positionals(u).is_empty());
        if has_files || from_find || from_cat {
            return Some(Steer {
                rule_id: "count-lines",
                tool: "ct tree",
                suggestion: "ct tree --summary".to_string(),
                note: "ct tree reports per-file and total line/word/char counts directly, replacing wc -l over a file set",
            });
        }
    }
    None
}

// ----- Extraction helpers ------------------------------------------------------

/// Assemble a `ct search` suggestion from optional base/name/grep parts.
fn search_suggestion(base: Option<&str>, name: Option<&str>, grep: Option<&str>) -> String {
    let mut out = String::from("ct search");
    if let Some(b) = base {
        out.push_str(&format!(" --base {b}"));
    }
    if let Some(n) = name {
        out.push_str(&format!(" --name {}", q(n)));
    }
    match grep {
        Some(g) => out.push_str(&format!(" --grep {}", q(g))),
        None => out.push_str(" --grep <pattern>"),
    }
    out
}

/// Build a `ct view` suggestion for a file and optional line range.
fn view_steer(file: Option<&str>, range: Option<(u32, u32)>) -> Steer {
    let f = file.unwrap_or("<file>");
    let suggestion = match range {
        Some((a, b)) => format!("ct view {f} --range {a}:{b}"),
        None => format!("ct view {f} --range <start>:<end>"),
    };
    Steer {
        rule_id: "read-range",
        tool: "ct view",
        suggestion,
        note: "ct view shows a file's lines by range (or the regions around a pattern with context) — a precise, bounded read",
    }
}

/// Build a `ct view` suggestion for an interpreter one-liner that reads `file`.
fn interpreter_steer(file: Option<&str>) -> Steer {
    let f = file.unwrap_or("<file>");
    Steer {
        rule_id: "interpreter-read",
        tool: "ct view",
        suggestion: format!("ct view {f} --range <start>:<end>"),
        note: "an interpreter one-liner that reads a file is a bounded read — `ct view` shows a line range (or `--match <pat> --context N`), and `ct search <file> --grep <pat> --detail` finds the matching record, both without a hand-rolled parser",
    }
}

/// Whether an inline interpreter script reads a file.
fn reads_file(body: &str) -> bool {
    const READS: &[&str] = &[
        "open(",
        "json.load",
        "readlines",
        "read_text",
        "readFileSync",
        "JSON.parse",
        "File.read",
        "IO.read",
        "Get-Content",
    ];
    READS.iter().any(|m| body.contains(m))
}

/// Whether an inline interpreter script appears to write/produce a file — used
/// to leave read+write one-liners alone (only pure reads are steered).
fn writes_file(body: &str) -> bool {
    const WRITES: &[&str] = &[
        ",'w'",
        ", 'w'",
        ",\"w\"",
        ", \"w\"",
        ",'a'",
        ", 'a'",
        ",\"a\"",
        "'r+'",
        "\"r+\"",
        "'wb'",
        "\"wb\"",
        ".write(",
        "writeFile",
        "json.dump",
        "to_csv",
        "to_json(",
        "File.write",
    ];
    WRITES.iter().any(|m| body.contains(m))
}

/// The first quoted token in an interpreter body that looks like a file path
/// (contains `.` or `/`), for use in the suggested `ct view` command.
fn quoted_path(body: &str) -> Option<&str> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if (c == b'\'' || c == b'"')
            && let Some(rel) = body[i + 1..].find(c as char)
        {
            let inner = &body[i + 1..i + 1 + rel];
            if inner.contains('.') || inner.contains('/') {
                return Some(inner);
            }
            i += 1 + rel + 1;
            continue;
        }
        i += 1;
    }
    None
}

/// The PATTERN of a grep-family stage: an explicit `-e VALUE`, else the first
/// bare word *after the grep token* (which may follow `xargs`, `-exec`, …, so
/// keying off the stage's own command word would pick up the wrong thing).
fn grep_pattern(stage: &[String]) -> Option<&str> {
    if let Some(v) = flag_value(stage, &["-e", "--regexp"]) {
        return Some(v);
    }
    let start = stage
        .iter()
        .position(|w| {
            matches!(
                base_name(w),
                "grep" | "egrep" | "fgrep" | "rg" | "ripgrep" | "ag"
            )
        })
        .map_or(1, |i| i + 1);
    stage[start..]
        .iter()
        .find(|w| !w.starts_with('-'))
        .map(String::as_str)
}

/// Parse `s/FIND/REPLACE/flags` (any single-char delimiter) → (find, replace).
fn sed_subst(stage: &[String]) -> (Option<&str>, Option<&str>) {
    for w in stage.iter().skip(1) {
        if let Some(rest) = w.strip_prefix('s')
            && let Some(delim) = rest.chars().next()
            && !delim.is_alphanumeric()
        {
            let parts: Vec<&str> = rest[delim.len_utf8()..].split(delim).collect();
            if parts.len() >= 2 {
                return (Some(parts[0]), Some(parts[1]));
            }
        }
    }
    (None, None)
}

/// The N from a `head`/`tail` count flag: `-n N`, `-nN`, or `-N`.
fn head_count(stage: &[String]) -> Option<u32> {
    if let Some(v) = flag_value(stage, &["-n", "--lines"])
        && let Ok(n) = v.parse::<u32>()
    {
        return Some(n);
    }
    stage
        .iter()
        .skip(1)
        .find_map(|w| w.strip_prefix('-').and_then(|d| d.parse::<u32>().ok()))
}

/// Whether a word is a `sed` script (`A,Bp`, `Np`, or an `s<delim>…` subst)
/// rather than a file. Deliberately narrow so filenames like `src/lib.rs`
/// (which begin with `s`) are not misread as scripts.
fn is_sed_script(w: &str) -> bool {
    if parse_sed_range(w).is_some() {
        return true;
    }
    let mut ch = w.chars();
    ch.next() == Some('s') && ch.next().is_some_and(|d| !d.is_alphanumeric())
}

/// Parse a `sed -n` line range like `10,20p` or `10p` → `(start, end)`.
fn parse_sed_range(w: &str) -> Option<(u32, u32)> {
    let body = w.strip_suffix('p').unwrap_or(w);
    match body.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => {
            let n = body.parse().ok()?;
            Some((n, n))
        }
    }
}

// ----- Harness tool-envelope steers --------------------------------------------
//
// The hook can gate not just `Bash` but the harness's own `Grep` / `Glob` /
// `Read` tools — the *other* channel by which an agent reaches around `ct`.
// Those calls carry structured fields (a `pattern`, a `path`, a `file_path`)
// rather than a shell line, so each gets its own builder rather than going
// through [`analyze`].

/// Steer a harness `Grep` call to `ct search` (the suite's content search).
pub fn grep_steer(pattern: &str, path: Option<&str>, glob: Option<&str>) -> Steer {
    Steer {
        rule_id: "harness-grep",
        tool: "ct search",
        suggestion: search_suggestion(path, glob, Some(pattern)),
        note: "ct search is the suite's content search — recursive, filtered by name/type/size, with a framed --expect verdict; ct outline maps a file's symbols when you are after a definition",
    }
}

/// Steer a harness `Glob` call to `ct search` (name filter from a root).
pub fn glob_steer(pattern: &str, path: Option<&str>) -> Steer {
    let (glob_base, name) = split_glob(pattern);
    let base = path.map(str::to_string).or(glob_base);
    let mut out = String::from("ct search");
    if let Some(b) = base {
        out.push_str(&format!(" --base {b}"));
    }
    out.push_str(&format!(" --name {} --type f", q(&name)));
    Steer {
        rule_id: "harness-glob",
        tool: "ct search",
        suggestion: out,
        note: "ct search selects files by --name/--type/--size from a chosen root and reports them — the suite's glob, recursive by default",
    }
}

/// Split a glob into a literal directory prefix (its leading wildcard-free
/// segments) and the file-name segment (its last component): `src/**/*.rs` →
/// `(Some("src"), "*.rs")`; `**/*.rs` → `(None, "*.rs")`.
fn split_glob(pattern: &str) -> (Option<String>, String) {
    let segs: Vec<&str> = pattern.split('/').collect();
    let name = segs.last().copied().unwrap_or(pattern).to_string();
    let is_wild = |s: &str| s.contains(['*', '?', '[', '{']);
    let literal: Vec<&str> = segs
        .iter()
        .take(segs.len().saturating_sub(1))
        .take_while(|s| !is_wild(s) && !s.is_empty())
        .copied()
        .collect();
    ((!literal.is_empty()).then(|| literal.join("/")), name)
}

/// Steer a harness `Read` call to `ct view` — unless the path is something
/// `ct view` (a line reader) cannot render (an image, PDF, or notebook), where
/// `Read` is the right tool and the call is left alone ([`None`]).
pub fn read_steer(file_path: &str, offset: Option<i64>, limit: Option<i64>) -> Option<Steer> {
    if is_unrenderable(file_path) {
        return None;
    }
    // Read's offset is a 1-based start line and limit a line count; map both to
    // `ct view --range`. A bare read (neither) views the whole file.
    let range = match (offset, limit) {
        (Some(o), Some(l)) => {
            let start = o.max(1);
            Some(format!("{start}:{}", (start + l - 1).max(start)))
        }
        (Some(o), None) => Some(format!("{}:", o.max(1))),
        (None, Some(l)) => Some(format!("1:{}", l.max(1))),
        (None, None) => None,
    };
    let suggestion = match range {
        Some(r) => format!("ct view {file_path} --range {r}"),
        None => format!("ct view {file_path}"),
    };
    Some(Steer {
        rule_id: "harness-read",
        tool: "ct view",
        suggestion,
        note: "ct view is the suite's bounded file reader — a line range, or --match with context (Read stays the tool for images, PDFs, and notebooks ct view cannot render)",
    })
}

/// Whether a path's extension is a binary/rendered format `ct view` cannot
/// usefully show as text — so a `Read` of it is left alone.
fn is_unrenderable(path: &str) -> bool {
    const EXTS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff", "pdf", "ipynb",
    ];
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    path.contains('.') && EXTS.contains(&ext.as_str())
}

// ----- Hook protocol -----------------------------------------------------------

/// The Claude Code `PreToolUse` hook protocol: turn a stdin envelope into a
/// steering decision.
pub mod hook {
    use super::{Mode, Steer, analyze, glob_steer, grep_steer, read_steer};
    use serde_json::{Value, json};

    /// Build the `PreToolUse` decision JSON for a [`Steer`] under `mode`.
    pub fn decision(steer: &Steer, mode: Mode) -> Value {
        let reason = steer.reason();
        match mode {
            Mode::Deny => json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }}),
            Mode::Ask => json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "ask",
                "permissionDecisionReason": reason,
            }}),
            Mode::Warn => json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": reason,
            }}),
        }
    }

    /// A string field of a tool-input object.
    fn str_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
        input.get(key).and_then(Value::as_str)
    }

    /// An integer field of a tool-input object.
    fn int_field(input: &Value, key: &str) -> Option<i64> {
        input.get(key).and_then(Value::as_i64)
    }

    /// Process a raw `PreToolUse` stdin envelope. Returns the decision JSON to
    /// print, or [`None`] to allow silently. The `Bash` command is classified by
    /// [`analyze`]; the harness's own `Grep` / `Glob` / `Read` calls are steered
    /// from their structured fields. **Fail-open:** any parse error, an
    /// unhandled tool, or a missing field all yield [`None`].
    pub fn process(envelope: &str, mode: Mode) -> Option<Value> {
        let v: Value = serde_json::from_str(envelope).ok()?;
        let tool = v.get("tool_name").and_then(Value::as_str)?;
        let input = v.get("tool_input")?;
        let steer = match tool {
            "Bash" => analyze(str_field(input, "command")?),
            "Grep" => Some(grep_steer(
                str_field(input, "pattern")?,
                str_field(input, "path"),
                str_field(input, "glob"),
            )),
            "Glob" => Some(glob_steer(
                str_field(input, "pattern")?,
                str_field(input, "path"),
            )),
            "Read" => read_steer(
                str_field(input, "file_path")?,
                int_field(input, "offset"),
                int_field(input, "limit"),
            ),
            _ => None,
        }?;
        Some(decision(&steer, mode))
    }
}

// ----- Settings install --------------------------------------------------------

/// Merging the steering hook into a Claude Code settings file. The merge runs
/// through the comment- and layout-preserving `ct-patch` engine
/// ([`crate::patch`]): the existing file is parsed only to *decide* which edits
/// to make, and those edits are byte-range splices against the original text,
/// so the user's comments and formatting survive.
pub mod install {
    use super::Mode;
    use crate::patch::{self, Op, parse_path};
    use serde_json::{Value, json};
    use std::path::{Path, PathBuf};

    /// Which settings file the hook is written to.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Scope {
        /// `.claude/settings.json` (shared, committed).
        Project,
        /// `.claude/settings.local.json` (personal, gitignored).
        Local,
        /// `~/.claude/settings.json` (all projects).
        User,
    }

    impl Scope {
        /// Parse the `--scope` value.
        pub fn from_name(s: &str) -> Option<Scope> {
            match s {
                "project" => Some(Scope::Project),
                "local" => Some(Scope::Local),
                "user" => Some(Scope::User),
                _ => None,
            }
        }

        /// The settings file path. `project`/`local` are relative to `root`
        /// (the project directory); `user` lives under `home`.
        pub fn path(self, root: &Path, home: &Path) -> PathBuf {
            match self {
                Scope::Project => root.join(".claude").join("settings.json"),
                Scope::Local => root.join(".claude").join("settings.local.json"),
                Scope::User => home.join(".claude").join("settings.json"),
            }
        }
    }

    /// A harness tool the steering hook can be installed to gate. Each becomes
    /// its own `PreToolUse` matcher entry; `Bash` is the default.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Tool {
        /// Shell commands — classified by the full shell-idiom matcher.
        Bash,
        /// The harness content search → `ct search`.
        Grep,
        /// The harness file glob → `ct search`.
        Glob,
        /// The harness file read → `ct view` (images/PDF/notebooks pass through).
        Read,
    }

    impl Tool {
        /// Parse a `--tools` value.
        pub fn from_name(s: &str) -> Option<Tool> {
            match s {
                "Bash" => Some(Tool::Bash),
                "Grep" => Some(Tool::Grep),
                "Glob" => Some(Tool::Glob),
                "Read" => Some(Tool::Read),
                _ => None,
            }
        }

        /// The `matcher` string this tool is written under in settings.
        pub fn matcher(self) -> &'static str {
            match self {
                Tool::Bash => "Bash",
                Tool::Grep => "Grep",
                Tool::Glob => "Glob",
                Tool::Read => "Read",
            }
        }
    }

    /// The hook command string written into settings for `mode`.
    pub fn hook_command(mode: Mode) -> String {
        match mode {
            Mode::Deny => "ct steer hook".to_string(),
            other => format!("ct steer hook --mode {}", other.name()),
        }
    }

    /// Whether a settings hook command is one of ours (any mode).
    fn is_steer_command(s: &str) -> bool {
        s.contains("steer") && s.contains("hook")
    }

    /// Parse existing settings text (JSONC tolerated) for read-only inspection.
    /// The actual mutation is a byte-splice on the original text via `ct-patch`,
    /// so this serde view is only used to *decide* which edits to make.
    fn inspect(text: &str) -> Result<Value, String> {
        let root = jsonc_parser::parse_to_serde_value(text, &jsonc_parser::ParseOptions::default())
            .map_err(|e| format!("parse settings: {e}"))?
            .unwrap_or_else(|| json!({}));
        if !root.is_object() {
            return Err("settings root must be a JSON object".to_string());
        }
        Ok(root)
    }

    /// The canonical full settings document, used only when there is no existing
    /// file to merge into (so there are no comments or layout to preserve). One
    /// `PreToolUse` matcher entry per requested tool.
    fn canonical(command: &str, tools: &[Tool]) -> String {
        let matchers: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({ "matcher": t.matcher(), "hooks": [ { "type": "command", "command": command } ] })
            })
            .collect();
        let v = json!({ "hooks": { "PreToolUse": matchers } });
        serde_json::to_string_pretty(&v).unwrap() + "\n"
    }

    fn op_set(path: &str, value: String) -> Result<Op, String> {
        Ok(Op::Set {
            path: parse_path(path)?,
            raw: path.to_string(),
            value,
        })
    }
    fn op_add(path: &str, value: String) -> Result<Op, String> {
        Ok(Op::Add {
            path: parse_path(path)?,
            raw: path.to_string(),
            value,
        })
    }
    fn op_delete(path: &str) -> Result<Op, String> {
        Ok(Op::Delete {
            path: parse_path(path)?,
            raw: path.to_string(),
        })
    }

    /// Apply a computed op sequence to the original text via the comment- and
    /// layout-preserving `ct-patch` engine.
    fn apply(text: &str, ops: &[Op]) -> Result<(String, bool), String> {
        if ops.is_empty() {
            return Ok((text.to_string(), false));
        }
        let (out, changes) =
            patch::apply_doc(text, ops).map_err(|e| format!("settings merge: {e}"))?;
        Ok((out, changes > 0))
    }

    /// Install the steering hook into `existing` settings text (or create a
    /// fresh document), gating each tool in `tools`. Returns the new text and
    /// whether it changed. Idempotent: re-installing the same command/tools is a
    /// no-op; a `--mode` change rewrites the command in place; a new tool adds
    /// its matcher. Comments and layout in `existing` are preserved.
    pub fn install(
        existing: Option<&str>,
        command: &str,
        tools: &[Tool],
    ) -> Result<(String, bool), String> {
        let Some(text) = existing.filter(|t| !t.trim().is_empty()) else {
            return Ok((canonical(command, tools), true));
        };
        let root = inspect(text)?;
        let ops = install_ops(&root, command, tools)?;
        apply(text, &ops)
    }

    /// Remove every steering hook from `existing` settings text, pruning emptied
    /// matcher entries (and the `PreToolUse`/`hooks` containers when they end up
    /// empty). Comments and layout elsewhere are preserved.
    pub fn uninstall(existing: Option<&str>) -> Result<(String, bool), String> {
        let Some(text) = existing.filter(|t| !t.trim().is_empty()) else {
            return Ok((existing.unwrap_or_default().to_string(), false));
        };
        let root = inspect(text)?;
        let ops = uninstall_ops(&root)?;
        apply(text, &ops)
    }

    /// The `hooks.PreToolUse` array, if present and well-shaped.
    fn pre_array(root: &Value) -> Option<&Vec<Value>> {
        root.get("hooks")
            .and_then(|h| h.get("PreToolUse"))
            .and_then(Value::as_array)
    }

    /// Whether an array element is a `"matcher": <name>` entry.
    fn is_matcher(entry: &Value, name: &str) -> bool {
        entry.get("matcher").and_then(Value::as_str) == Some(name)
    }

    /// Whether a matcher entry already carries one of our steer hooks.
    fn entry_has_steer(entry: &Value) -> bool {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|l| {
                l.iter().any(|h| {
                    h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(is_steer_command)
                })
            })
    }

    /// Compute the ops that install `command` for every tool in `tools`, given
    /// the parsed `root`. Indices come from the original parse and stay valid
    /// because every op only adds keys/elements, never reorders entries.
    fn install_ops(root: &Value, command: &str, tools: &[Tool]) -> Result<Vec<Op>, String> {
        let mut ops = Vec::new();

        // Ensure the `hooks` / `hooks.PreToolUse` containers exist.
        let hooks = root.get("hooks");
        match hooks {
            None => ops.push(op_set(".hooks", "{}".to_string())?),
            Some(h) if !h.is_object() => {
                return Err("settings `hooks` must be an object".to_string());
            }
            Some(_) => {}
        }
        let pre = hooks.and_then(|h| h.get("PreToolUse"));
        match pre {
            None => ops.push(op_set(".hooks.PreToolUse", "[]".to_string())?),
            Some(p) if !p.is_array() => {
                return Err("settings `hooks.PreToolUse` must be an array".to_string());
            }
            Some(_) => {}
        }
        let pre_arr = pre.and_then(Value::as_array);

        // Mode change: rewrite the command of any existing steer hook that differs.
        if let Some(arr) = pre_arr {
            for (ei, entry) in arr.iter().enumerate() {
                let Some(list) = entry.get("hooks").and_then(Value::as_array) else {
                    continue;
                };
                for (hi, h) in list.iter().enumerate() {
                    if let Some(c) = h.get("command").and_then(Value::as_str)
                        && is_steer_command(c)
                        && c != command
                    {
                        ops.push(op_set(
                            &format!(".hooks.PreToolUse[{ei}].hooks[{hi}].command"),
                            json!(command).to_string(),
                        )?);
                    }
                }
            }
        }

        // Per requested tool: ensure a matcher for it carries our command.
        let hook_obj = json!({ "type": "command", "command": command }).to_string();
        for tool in tools {
            let name = tool.matcher();
            // Already steered for this tool (mode change handled above)?
            if pre_arr.is_some_and(|arr| {
                arr.iter()
                    .any(|e| is_matcher(e, name) && entry_has_steer(e))
            }) {
                continue;
            }
            // Append to an existing matcher entry for this tool, else add one.
            let target =
                pre_arr.and_then(|arr| arr.iter().enumerate().find(|(_, e)| is_matcher(e, name)));
            match target {
                Some((ei, e)) if e.get("hooks").and_then(Value::as_array).is_some() => {
                    ops.push(op_add(
                        &format!(".hooks.PreToolUse[{ei}].hooks"),
                        hook_obj.clone(),
                    )?);
                }
                Some((ei, _)) => {
                    ops.push(op_set(
                        &format!(".hooks.PreToolUse[{ei}].hooks"),
                        format!("[{hook_obj}]"),
                    )?);
                }
                None => {
                    let matcher = json!({ "matcher": name, "hooks": [ { "type": "command", "command": command } ] })
                        .to_string();
                    ops.push(op_add(".hooks.PreToolUse", matcher)?);
                }
            }
        }
        Ok(ops)
    }

    /// Compute the ops that remove every steering hook, given the parsed `root`.
    fn uninstall_ops(root: &Value) -> Result<Vec<Op>, String> {
        let Some(pre) = pre_array(root) else {
            return Ok(vec![]);
        };
        // Per matcher entry: which of its hooks are ours, and whether removing
        // them empties the entry.
        let mut whole_entries = Vec::new(); // entry indices to delete outright
        let mut partial = Vec::new(); // (entry index, our hook indices)
        for (ei, entry) in pre.iter().enumerate() {
            let Some(list) = entry.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            let ours: Vec<usize> = list
                .iter()
                .enumerate()
                .filter(|(_, h)| {
                    h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(is_steer_command)
                })
                .map(|(hi, _)| hi)
                .collect();
            if ours.is_empty() {
                continue;
            }
            if ours.len() == list.len() {
                whole_entries.push(ei);
            } else {
                partial.push((ei, ours));
            }
        }
        if whole_entries.is_empty() && partial.is_empty() {
            return Ok(vec![]);
        }

        // Every entry removed outright and none surviving → the whole
        // PreToolUse goes (or `hooks` itself if PreToolUse was its only key).
        if partial.is_empty() && whole_entries.len() == pre.len() {
            let hooks_solo = root
                .get("hooks")
                .and_then(Value::as_object)
                .is_some_and(|o| o.len() == 1);
            let path = if hooks_solo {
                ".hooks"
            } else {
                ".hooks.PreToolUse"
            };
            return Ok(vec![op_delete(path)?]);
        }

        let mut ops = Vec::new();
        // Inner-hook deletes first (descending index, so earlier indices stay
        // valid), then whole-entry deletes (descending, likewise). Inner deletes
        // never shift entry indices, and partial vs whole entries are disjoint.
        for (ei, his) in &partial {
            for hi in his.iter().rev() {
                ops.push(op_delete(&format!(".hooks.PreToolUse[{ei}].hooks[{hi}]"))?);
            }
        }
        for ei in whole_entries.iter().rev() {
            ops.push(op_delete(&format!(".hooks.PreToolUse[{ei}]"))?);
        }
        Ok(ops)
    }
}

#[cfg(test)]
mod tests {
    use super::install::{Scope, Tool, install, uninstall};
    use super::*;
    use std::path::Path;

    /// Bash-only install, the common case in these tests.
    fn install_bash(existing: Option<&str>, command: &str) -> Result<(String, bool), String> {
        install(existing, command, &[Tool::Bash])
    }

    fn tool(cmd: &str) -> Option<&'static str> {
        analyze(cmd).map(|s| s.tool)
    }
    fn rule(cmd: &str) -> Option<&'static str> {
        analyze(cmd).map(|s| s.rule_id)
    }

    #[test]
    fn steers_high_confidence_idioms() {
        assert_eq!(
            tool("find . -name '*.rs' | xargs grep TODO"),
            Some("ct search")
        );
        assert_eq!(
            rule("find . -name '*.rs' | xargs grep TODO"),
            Some("find-grep")
        );
        assert_eq!(tool("grep -rn TODO src"), Some("ct search"));
        assert_eq!(tool("rg TODO src"), Some("ct search"));
        assert_eq!(tool("find src -name '*.rs'"), Some("ct search"));
        assert_eq!(tool("sed -i 's/foo/bar/g' src/x.rs"), Some("ct edit"));
        assert_eq!(tool("head -n 40 src/lib.rs"), Some("ct view"));
        assert_eq!(tool("cat src/lib.rs | head -n 20"), Some("ct view"));
        assert_eq!(tool("sed -n '10,20p' src/lib.rs"), Some("ct view"));
        assert_eq!(tool("ls -R src"), Some("ct tree"));
        assert_eq!(tool("wc -l src/lib.rs"), Some("ct tree"));
        assert_eq!(tool("for f in a b; do grep -r x $f; done"), Some("ct each"));
        assert_eq!(
            rule("for f in a b; do grep -r x $f; done"),
            Some("shell-loop")
        );
    }

    #[test]
    fn steers_wait_loops_to_await_not_each() {
        // a sleep-bearing poll/wait loop is a bounded wait → ct await
        assert_eq!(
            tool("for i in $(seq 1 900); do cat f; sleep 2; done"),
            Some("ct await")
        );
        assert_eq!(
            rule("for i in $(seq 1 900); do cat f; sleep 2; done"),
            Some("wait-loop")
        );
        assert_eq!(
            tool("while true; do check; sleep 5; done"),
            Some("ct await")
        );
        assert_eq!(
            tool("until curl -sf http://x; do sleep 3; done"),
            Some("ct await")
        );
        // a sleep-free loop stays a per-item map → ct each
        assert_eq!(tool("for f in a b; do grep -r x $f; done"), Some("ct each"));
        assert_eq!(
            rule("for f in a b; do grep -r x $f; done"),
            Some("shell-loop")
        );
    }

    #[test]
    fn steers_interpreter_file_reads() {
        // jq with a file argument reads a file → ct view
        assert_eq!(tool("jq '.note' feedback/x.jsonl"), Some("ct view"));
        assert_eq!(
            rule("jq '.note' feedback/x.jsonl"),
            Some("interpreter-read")
        );
        // python one-liner that opens a file and prints → ct view
        let s = analyze(
            "python -c \"rows=[json.loads(l) for l in open('feedback/x.jsonl')]; print(rows[-1])\"",
        )
        .unwrap();
        assert_eq!(s.tool, "ct view");
        assert!(
            s.suggestion.contains("feedback/x.jsonl"),
            "{}",
            s.suggestion
        );
        assert_eq!(
            tool("node -e 'const d=require(\"fs\").readFileSync(\"a.json\")'"),
            Some("ct view")
        );
        // pure-compute one-liner (no file read) is left alone
        assert!(analyze("python -c 'print(2+2)'").is_none());
        // a one-liner that writes is left alone
        assert!(analyze("python -c \"open('out.txt','w').write('hi')\"").is_none());
        // a jq fed by a pipe (no file) is left alone
        assert!(analyze("cat x | jq '.note'").is_none());
    }

    #[test]
    fn steers_count_idioms() {
        // grep -c counts matching lines → ct search --summary
        assert_eq!(tool("grep -c TODO src/lib.rs"), Some("ct search"));
        assert_eq!(rule("grep -c TODO src/lib.rs"), Some("grep-count"));
        let s = analyze("grep -c TODO src/lib.rs").unwrap();
        assert!(s.suggestion.contains("--grep 'TODO'") && s.suggestion.contains("--summary"));
        // cat FILES | wc -l counts lines of real files → ct tree
        assert_eq!(tool("cat a.jsonl b.jsonl | wc -l"), Some("ct tree"));
        // a bare stream count has no file behind it → left alone
        assert!(analyze("ps aux | wc -l").is_none());
    }

    #[test]
    fn extracts_obvious_slots() {
        let s = analyze("grep -rn TODO src").unwrap();
        assert!(s.suggestion.contains("--grep 'TODO'"), "{}", s.suggestion);
        let e = analyze("sed -i 's/foo/bar/g' f.rs").unwrap();
        assert!(
            e.suggestion.contains("--find 'foo'") && e.suggestion.contains("--replace 'bar'"),
            "{}",
            e.suggestion
        );
        let v = analyze("head -n 40 src/lib.rs").unwrap();
        assert!(
            v.suggestion.contains("src/lib.rs --range 1:40"),
            "{}",
            v.suggestion
        );
        // the grep pattern is taken after the `grep` token, not after `xargs`
        let fg = analyze("find . -name '*.rs' | xargs grep TODO").unwrap();
        assert!(fg.suggestion.contains("--grep 'TODO'"), "{}", fg.suggestion);
        assert!(fg.suggestion.contains("--name '*.rs'"), "{}", fg.suggestion);
    }

    #[test]
    fn chain_only_when_all_segments_serviceable() {
        let s = analyze("grep -r foo src && sed -i 's/a/b/' f.rs").unwrap();
        assert_eq!(s.tool, "ct and");
        assert!(
            s.suggestion.starts_with("ct and search"),
            "{}",
            s.suggestion
        );
        assert!(s.suggestion.contains(":::"), "{}", s.suggestion);
        // a mixed chain (one non-ct segment) is left alone
        assert!(analyze("grep -r foo src && make").is_none());
    }

    #[test]
    fn allows_safe_and_unknown_commands() {
        assert!(analyze("git status").is_none());
        assert!(analyze("cargo build && cargo test").is_none());
        assert!(analyze("ls -la").is_none());
        assert!(analyze("cat file.txt").is_none()); // whole-file read, not a range
        assert!(analyze("grep TODO file.rs").is_none()); // non-recursive, single file
        assert!(analyze("echo 'a | b && c'").is_none()); // operators inside quotes are inert
        assert!(analyze("ps aux | head -n 5").is_none()); // piped stream, no file
        assert!(analyze("").is_none());
    }

    #[test]
    fn never_resteers_a_ct_command() {
        assert!(analyze("ct search --grep TODO").is_none());
        assert!(analyze("ct-search --grep TODO").is_none());
        assert!(analyze("find . -name '*.rs' | xargs ct-edit").is_none());
    }

    #[test]
    fn hook_decisions_respect_mode() {
        let envelope = r#"{"tool_name":"Bash","tool_input":{"command":"grep -r TODO src"}}"#;
        let deny = hook::process(envelope, Mode::Deny).unwrap();
        assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(
            deny["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("ct search")
        );
        let ask = hook::process(envelope, Mode::Ask).unwrap();
        assert_eq!(ask["hookSpecificOutput"]["permissionDecision"], "ask");
        let warn = hook::process(envelope, Mode::Warn).unwrap();
        assert!(warn["hookSpecificOutput"]["additionalContext"].is_string());
        assert!(
            warn["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none()
        );
    }

    #[test]
    fn hook_steers_harness_grep_glob_read() {
        // Grep → ct search, carrying pattern / path / glob.
        let grep = hook::process(
            r#"{"tool_name":"Grep","tool_input":{"pattern":"TODO","path":"src","glob":"*.rs"}}"#,
            Mode::Deny,
        )
        .unwrap();
        let reason = grep["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("ct search"), "{reason}");
        assert!(
            reason.contains("--grep 'TODO'") && reason.contains("--base src"),
            "{reason}"
        );
        assert!(reason.contains("--name '*.rs'"), "{reason}");

        // Glob → ct search; a `dir/**/*.ext` glob splits into --base / --name.
        let s = glob_steer("src/**/*.rs", None);
        assert_eq!(s.tool, "ct search");
        assert!(s.suggestion.contains("--base src"), "{}", s.suggestion);
        assert!(s.suggestion.contains("--name '*.rs'"), "{}", s.suggestion);

        // Read → ct view with a range derived from offset/limit.
        let read = read_steer("src/lib.rs", Some(10), Some(20)).unwrap();
        assert_eq!(read.tool, "ct view");
        assert!(
            read.suggestion.contains("ct view src/lib.rs --range 10:29"),
            "{}",
            read.suggestion
        );
        // a bare read views the whole file
        assert_eq!(
            read_steer("notes.md", None, None).unwrap().suggestion,
            "ct view notes.md"
        );
        // images / PDFs / notebooks are left for Read (None)
        assert!(read_steer("diagram.png", None, None).is_none());
        assert!(read_steer("paper.pdf", None, None).is_none());
        assert!(read_steer("nb.ipynb", None, None).is_none());
    }

    #[test]
    fn install_covers_multiple_tools() {
        let (text, changed) =
            install(None, "ct steer hook", &[Tool::Bash, Tool::Grep, Tool::Read]).unwrap();
        assert!(changed);
        for m in ["\"Bash\"", "\"Grep\"", "\"Read\""] {
            assert!(text.contains(m), "missing matcher {m} in {text}");
        }
        // re-install with the same tools is a no-op
        let (_, again) = install(
            Some(&text),
            "ct steer hook",
            &[Tool::Bash, Tool::Grep, Tool::Read],
        )
        .unwrap();
        assert!(!again);
        // adding a tool to an existing install only appends its matcher
        let (grown, did) = install(Some(&text), "ct steer hook", &[Tool::Glob]).unwrap();
        assert!(did);
        assert!(grown.contains("\"Glob\""));
        assert_eq!(grown.matches("\"matcher\"").count(), 4);
        // uninstall clears every steer matcher we added
        let (cleared, _) = uninstall(Some(&grown)).unwrap();
        assert!(!cleared.contains("steer hook"));
    }

    #[test]
    fn hook_fails_open() {
        assert!(hook::process("not json", Mode::Deny).is_none());
        assert!(hook::process(r#"{"tool_name":"Read"}"#, Mode::Deny).is_none());
        assert!(hook::process(r#"{"tool_name":"Bash","tool_input":{}}"#, Mode::Deny).is_none());
        assert!(
            hook::process(
                r#"{"tool_name":"Bash","tool_input":{"command":"git status"}}"#,
                Mode::Deny
            )
            .is_none()
        );
    }

    #[test]
    fn install_is_idempotent_and_preserves_other_settings() {
        // fresh install
        let (text, changed) = install_bash(None, "ct steer hook").unwrap();
        assert!(changed);
        assert!(text.contains("PreToolUse"));
        assert!(text.contains("\"matcher\": \"Bash\""));
        assert!(text.contains("ct steer hook"));
        // re-install is a no-op
        let (text2, changed2) = install_bash(Some(&text), "ct steer hook").unwrap();
        assert!(!changed2);
        assert_eq!(text, text2);
        // a mode change rewrites in place (still one hook)
        let (text3, changed3) = install_bash(Some(&text), "ct steer hook --mode ask").unwrap();
        assert!(changed3);
        assert_eq!(text3.matches("steer hook").count(), 1);
        // existing unrelated settings survive
        let existing = r#"{ "model": "opus", "hooks": { "PreToolUse": [] } }"#;
        let (merged, _) = install_bash(Some(existing), "ct steer hook").unwrap();
        assert!(merged.contains("\"model\": \"opus\""));
    }

    #[test]
    fn uninstall_removes_only_our_hook() {
        let existing = r#"{
            "hooks": { "PreToolUse": [
                { "matcher": "Bash", "hooks": [
                    { "type": "command", "command": "ct steer hook" },
                    { "type": "command", "command": "./other.sh" }
                ] }
            ] }
        }"#;
        let (text, changed) = uninstall(Some(existing)).unwrap();
        assert!(changed);
        assert!(!text.contains("steer hook"));
        assert!(text.contains("./other.sh")); // the unrelated hook stays
        // uninstall on a clean file is a no-op
        let (_, changed2) = uninstall(Some("{}")).unwrap();
        assert!(!changed2);
    }

    #[test]
    fn install_and_uninstall_preserve_comments() {
        // a settings.json with comments the user cares about
        let existing = "{\n  \
            // pin the model\n  \
            \"model\": \"opus\", // do not change\n  \
            \"hooks\": {\n    \
            \"PreToolUse\": [\n      \
            { \"matcher\": \"Bash\", \"hooks\": [ { \"type\": \"command\", \"command\": \"./guard.sh\" } ] }\n    \
            ]\n  }\n}\n";
        let (installed, changed) = install_bash(Some(existing), "ct steer hook").unwrap();
        assert!(changed);
        // comments survive the merge
        assert!(installed.contains("// pin the model"), "{installed}");
        assert!(installed.contains("// do not change"), "{installed}");
        // the prior hook is untouched and ours is appended to the same matcher
        assert!(installed.contains("./guard.sh"), "{installed}");
        assert!(installed.contains("ct steer hook"), "{installed}");

        // uninstall removes only our hook, keeps the guard and the comments
        let (removed, changed2) = uninstall(Some(&installed)).unwrap();
        assert!(changed2);
        assert!(removed.contains("// pin the model"), "{removed}");
        assert!(removed.contains("./guard.sh"), "{removed}");
        assert!(!removed.contains("steer hook"), "{removed}");
    }

    #[test]
    fn scope_paths() {
        let root = Path::new("/proj");
        let home = Path::new("/home/u");
        assert!(
            Scope::Project
                .path(root, home)
                .ends_with(".claude/settings.json")
        );
        assert!(
            Scope::Local
                .path(root, home)
                .ends_with(".claude/settings.local.json")
        );
        assert!(Scope::User.path(root, home).starts_with("/home/u"));
    }
}
