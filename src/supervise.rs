// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Bounded child-command execution for the dispatching tools (`ct-test`,
//! `ct-each`).
//!
//! [`run_captured`] spawns a command with both streams captured, optionally
//! feeds it literal stdin, and enforces an optional timeout: when the limit
//! passes the child's whole **process group** is killed (on Unix; the child
//! alone elsewhere) and the run is reported as [`timed_out`](Outcome::timed_out)
//! rather than aborting the tool — so a timeout folds into the framed verdict
//! (`ERROR`, `{CODE}` = `timeout`) instead of producing an unexplained death.
//!
//! [`resolve_program`] is the suite's sibling resolution: a bare `ct-*` name is
//! looked up next to the running executable before falling back to `PATH`, so
//! the tools compose whether installed or freshly built.

use std::ffi::OsString;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// What a supervised run produced.
pub struct Outcome {
    /// Captured standard output (lossy UTF-8).
    pub stdout: String,
    /// Captured standard error (lossy UTF-8).
    pub stderr: String,
    /// The child's exit status; `None` when the run timed out and was killed.
    pub status: Option<ExitStatus>,
    /// Whether the timeout fired (the child and its process group were killed).
    pub timed_out: bool,
}

/// Resolve the program to launch. A bare `ct-*` name is resolved to a sibling
/// of the current executable first — the same resolution the `ct` umbrella
/// uses — so suite tools compose without `PATH` games; anything else launches
/// by name via `PATH`.
pub fn resolve_program(cmd: &str, name: &str) -> OsString {
    if name.starts_with("ct-")
        && !cmd.contains('/')
        && let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return candidate.into_os_string();
        }
    }
    OsString::from(cmd)
}

/// Kill the child's process group (Unix) or the child itself (elsewhere).
#[cfg(unix)]
fn kill_tree(child: &mut Child) {
    // The child was made a process-group leader at spawn, so a negative pid
    // signals the whole group — a build tool's own forked children included.
    let pid = child.id() as i32;
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_tree(child: &mut Child) {
    let _ = child.kill();
}

/// Run `command` to completion with stdout/stderr captured, writing
/// `stdin_text` (if any) to its standard input, killing it if it outlives
/// `timeout`.
pub fn run_captured(
    mut command: Command,
    stdin_text: Option<&str>,
    timeout: Option<Duration>,
) -> Result<Outcome, String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Lead a fresh process group so a timeout can kill the whole tree.
        command.process_group(0);
    }

    let mut child = command.spawn().map_err(|e| format!("failed to launch: {e}"))?;

    // Feed stdin from a thread so a child that never reads cannot deadlock the
    // supervisor. With no input the pipe handle drops here unused, closing the
    // child's stdin immediately.
    let stdin_pipe = child.stdin.take();
    let stdin_thread = stdin_text.map(|text| {
        let text = text.to_string();
        std::thread::spawn(move || {
            if let Some(mut pipe) = stdin_pipe {
                use std::io::Write;
                let _ = pipe.write_all(text.as_bytes());
            }
        })
    });

    // Drain both streams concurrently so a chatty child never blocks on a full
    // pipe while we wait.
    let mut out_pipe = child.stdout.take().expect("stdout was piped");
    let mut err_pipe = child.stderr.take().expect("stderr was piped");
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = timeout.map(|t| Instant::now() + t);
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Some(status), false),
            Ok(None) => {}
            Err(e) => return Err(format!("waiting for command: {e}")),
        }
        if let Some(d) = deadline
            && Instant::now() >= d
        {
            kill_tree(&mut child);
            let _ = child.wait();
            break (None, true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    if let Some(t) = stdin_thread {
        let _ = t.join();
    }
    let stdout = String::from_utf8_lossy(&out_thread.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).into_owned();

    Ok(Outcome {
        stdout,
        stderr,
        status,
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_falls_back_to_bare_name() {
        // Not a ct-* tool: resolution must hand back the name for PATH lookup.
        assert_eq!(resolve_program("grep", "grep"), OsString::from("grep"));
        // A pathed command is never sibling-resolved.
        assert_eq!(
            resolve_program("/bin/ls", "ls"),
            OsString::from("/bin/ls")
        );
    }

    #[cfg(unix)]
    #[test]
    fn captures_streams_and_status() {
        let mut c = Command::new("sh");
        c.args(["-c", "echo out; echo err >&2; exit 3"]);
        let r = run_captured(c, None, None).unwrap();
        assert_eq!(r.stdout, "out\n");
        assert_eq!(r.stderr, "err\n");
        assert_eq!(r.status.unwrap().code(), Some(3));
        assert!(!r.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn stdin_text_reaches_the_child() {
        let mut c = Command::new("cat");
        c.arg("-");
        let r = run_captured(c, Some("hello\n"), None).unwrap();
        assert_eq!(r.stdout, "hello\n");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_and_reports() {
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 30"]);
        let started = Instant::now();
        let r = run_captured(c, None, Some(Duration::from_millis(100))).unwrap();
        assert!(r.timed_out);
        assert!(r.status.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "kill must be prompt"
        );
    }
}
