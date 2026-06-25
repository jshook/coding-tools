// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Run-bounding and liveness, shared by every tool: the `--timeout` watchdog
//! that keeps any run bounded, and the `--heartbeat` pulse that gives an agent
//! a sign of life during a long one.
//!
//! Two enforcement styles, chosen per tool:
//!
//! * [`Watchdog`] — a hard self-bound for the tools that do their own work
//!   (`ct-search`, `ct-view`, `ct-tree`, `ct-edit`, `ct-patch`): when the limit
//!   passes, the process prints a one-line message and exits `2`. The mutating
//!   tools [`disarm`](Watchdog::disarm) it before their write phase, so a
//!   timeout can never interrupt a file write halfway.
//! * The child-running tools (`ct-test`, `ct-each`) instead bound the **child**
//!   through [`supervise`](crate::supervise), folding a timeout into the
//!   verdict rather than aborting — see that module.
//!
//! The [`Heartbeat`] is a small thread that prints a templated line every
//! interval — minimal by default (`[{ELAPSED}s]`), token-customisable with
//! `--heartbeat-emit`, and routable to stdout or stderr with `--heartbeat-to`.
//! Dynamic tokens (e.g. `ct-each`'s current `{ITEM}`) flow through a shared
//! [`PulseState`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::template;

/// The default `--heartbeat-emit` template: deliberately minimal.
pub const DEFAULT_HEARTBEAT_TEMPLATE: &str = "[{ELAPSED}s]";

/// Convert a positive seconds value (fractional allowed) into a [`Duration`].
///
/// # Examples
///
/// ```
/// use coding_tools::pulse::secs;
/// use std::time::Duration;
///
/// assert_eq!(secs("--timeout", 1.5).unwrap(), Duration::from_millis(1500));
/// assert!(secs("--timeout", 0.0).is_err());
/// assert!(secs("--heartbeat", -3.0).is_err());
/// ```
pub fn secs(option: &str, value: f64) -> Result<Duration, String> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!(
            "invalid {option} '{value}': must be a positive number of seconds"
        ));
    }
    Duration::try_from_secs_f64(value).map_err(|e| format!("invalid {option} '{value}': {e}"))
}

/// Render a duration limit for messages: `2s`, `1.5s` — no trailing zeros.
pub fn limit_label(limit: Duration) -> String {
    let v = limit.as_secs_f64();
    if v == v.trunc() {
        format!("{}s", v as u64)
    } else {
        format!("{v}s")
    }
}

// ----- Watchdog -----------------------------------------------------------------

/// A hard `--timeout` bound for a self-contained (non-child-running) tool.
///
/// [`arm`](Watchdog::arm) spawns a thread that, once the limit passes, prints
/// `<tool>: timed out after <limit>; aborted` to stderr and exits the process
/// with status `2` (the suite's usage/runtime-error code). Dropping the guard
/// — or calling [`disarm`](Watchdog::disarm) — defuses it, so a run that
/// finishes in time (or is about to start un-interruptible work, like
/// `ct-edit`'s write phase) is never killed.
pub struct Watchdog {
    disarmed: Arc<AtomicBool>,
}

impl Watchdog {
    /// Arm a timeout for `tool`; the returned guard must stay alive while the
    /// bound should be enforced.
    pub fn arm(tool: &'static str, limit: Duration) -> Watchdog {
        let disarmed = Arc::new(AtomicBool::new(false));
        let flag = disarmed.clone();
        std::thread::spawn(move || {
            std::thread::sleep(limit);
            if !flag.load(Ordering::SeqCst) {
                eprintln!("{tool}: timed out after {}; aborted", limit_label(limit));
                std::process::exit(2);
            }
        });
        Watchdog { disarmed }
    }

    /// Defuse the watchdog; after this the limit is never enforced.
    pub fn disarm(&self) {
        self.disarmed.store(true, Ordering::SeqCst);
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.disarm();
    }
}

/// Arm a [`Watchdog`] from a raw `--timeout` value, if one was given. The
/// returned guard must be held for the span the bound should cover.
pub fn watchdog(tool: &'static str, timeout: Option<f64>) -> Result<Option<Watchdog>, String> {
    match timeout {
        Some(v) => Ok(Some(Watchdog::arm(tool, secs("--timeout", v)?))),
        None => Ok(None),
    }
}

// ----- Heartbeat ----------------------------------------------------------------

/// Stream selector for `--heartbeat-to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PulseTo {
    /// Write pulses to standard error (the default; never pollutes `--emit`).
    Stderr,
    /// Write pulses to standard output.
    Stdout,
}

/// Live token values a heartbeat renders each pulse, updatable while running
/// (e.g. `ct-each` sets `{ITEM}`/`{INDEX}`/`{DONE}`/`{TOTAL}` as it advances).
#[derive(Default)]
pub struct PulseState {
    pairs: Mutex<Vec<(String, String)>>,
}

impl PulseState {
    /// A fresh, empty state behind an [`Arc`] for sharing with the pulse thread.
    pub fn new() -> Arc<PulseState> {
        Arc::new(PulseState::default())
    }

    /// Set (or replace) one token's current value.
    pub fn set(&self, key: &str, value: &str) {
        let mut pairs = self.pairs.lock().unwrap();
        match pairs.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value.to_string(),
            None => pairs.push((key.to_string(), value.to_string())),
        }
    }

    fn snapshot(&self) -> Vec<(String, String)> {
        self.pairs.lock().unwrap().clone()
    }
}

/// The shared `--heartbeat` option group, `#[command(flatten)]`-ed into every
/// leaf tool's CLI so the flags are named and documented identically.
#[derive(clap::Args, Debug)]
pub struct HeartbeatOpts {
    /// Print a liveness pulse every SECS seconds (fractional allowed) while the run is in progress.
    #[arg(long, value_name = "SECS")]
    pub heartbeat: Option<f64>,

    /// Heartbeat line template. Tokens: {ELAPSED} (whole seconds so far), {TOOL}, plus per-tool tokens. Default: "[{ELAPSED}s]".
    #[arg(long, value_name = "TEMPLATE")]
    pub heartbeat_emit: Option<String>,

    /// Stream heartbeat pulses are written to.
    #[arg(long, value_enum, default_value_t = PulseTo::Stderr)]
    pub heartbeat_to: PulseTo,
}

impl HeartbeatOpts {
    /// Start the pulse if `--heartbeat` was given. `state` carries the dynamic
    /// tokens; the `{TOOL}` token is set here. Returns the guard that stops the
    /// pulse on drop (`None` when no heartbeat was requested).
    pub fn start(&self, tool: &str, state: Arc<PulseState>) -> Result<Option<Heartbeat>, String> {
        let Some(every) = self.heartbeat else {
            return Ok(None);
        };
        let interval = secs("--heartbeat", every)?;
        state.set("TOOL", tool);
        let template = self
            .heartbeat_emit
            .clone()
            .unwrap_or_else(|| DEFAULT_HEARTBEAT_TEMPLATE.to_string());
        Ok(Some(Heartbeat::start(
            interval,
            template,
            self.heartbeat_to,
            state,
        )))
    }
}

/// A running heartbeat: a thread printing one templated line per interval.
/// Dropping the guard stops the pulse promptly (before drop returns), so no
/// pulse can land after a tool's final output.
pub struct Heartbeat {
    stop: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeat {
    /// Start pulsing every `interval`, rendering `template` from `{ELAPSED}`
    /// plus the current `state` tokens, onto the `to` stream.
    pub fn start(
        interval: Duration,
        template: String,
        to: PulseTo,
        state: Arc<PulseState>,
    ) -> Heartbeat {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let shared = stop.clone();
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let (lock, cvar) = &*shared;
            let mut stopped = lock.lock().unwrap();
            loop {
                // Sleep one interval, waking early only when stopped.
                let tick_start = Instant::now();
                while !*stopped {
                    let elapsed = tick_start.elapsed();
                    if elapsed >= interval {
                        break;
                    }
                    let (guard, _) = cvar.wait_timeout(stopped, interval - elapsed).unwrap();
                    stopped = guard;
                }
                if *stopped {
                    break;
                }
                let elapsed_s = started.elapsed().as_secs().to_string();
                let mut pairs = vec![("ELAPSED".to_string(), elapsed_s)];
                pairs.extend(state.snapshot());
                let refs: Vec<(&str, &str)> = pairs
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                let line = template::render(&template, &refs);
                match to {
                    PulseTo::Stdout => println!("{line}"),
                    PulseTo::Stderr => eprintln!("{line}"),
                }
            }
        });
        Heartbeat {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.stop;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secs_accepts_positive_fractions_only() {
        assert_eq!(secs("--timeout", 0.25).unwrap(), Duration::from_millis(250));
        assert!(secs("--timeout", 0.0).is_err());
        assert!(secs("--timeout", -1.0).is_err());
        assert!(secs("--timeout", f64::NAN).is_err());
    }

    #[test]
    fn limit_label_drops_trailing_zeroes() {
        assert_eq!(limit_label(Duration::from_secs(2)), "2s");
        assert_eq!(limit_label(Duration::from_millis(1500)), "1.5s");
    }

    #[test]
    fn pulse_state_set_replaces_existing_keys() {
        let state = PulseState::new();
        state.set("ITEM", "a");
        state.set("ITEM", "b");
        state.set("INDEX", "1");
        let snap = state.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap.contains(&("ITEM".to_string(), "b".to_string())));
    }

    #[test]
    fn watchdog_disarmed_by_drop_does_not_kill() {
        // Arm a tiny watchdog and drop it immediately; if disarm-on-drop failed,
        // the process would exit(2) and the test run itself would die.
        let w = Watchdog::arm("pulse-test", Duration::from_millis(20));
        drop(w);
        std::thread::sleep(Duration::from_millis(60));
    }

    #[test]
    fn heartbeat_stops_on_drop() {
        let state = PulseState::new();
        let hb = Heartbeat::start(
            Duration::from_millis(5),
            "[{ELAPSED}s]".to_string(),
            PulseTo::Stderr,
            state,
        );
        std::thread::sleep(Duration::from_millis(12));
        drop(hb); // must join promptly rather than hanging the test
    }
}
