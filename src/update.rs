// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Best-effort "is there a newer release?" check against the crates.io **sparse
//! index**, wired into the `ct` umbrella so the suite can tell you when an update
//! is available without ever getting in your way.
//!
//! The design follows the crates.io guidance for polite, CDN-friendly polling:
//!
//! * **Sparse protocol over the CDN.** We `GET` the crate's index file at
//!   `https://index.crates.io/<path>` (the same host cargo's sparse registry
//!   uses, fronted by a CDN), rather than cloning the git index. The path is
//!   built from the crate name by cargo's rule ([`index_path`]).
//! * **Conditional requests.** We send the previous response's `ETag` back as
//!   `If-None-Match`, so an unchanged index answers `304 Not Modified` with no
//!   body — the cheap path the CDN is built for.
//! * **Throttled.** At most one network poll per interval (daily by default,
//!   `CT_UPDATE_CHECK` overrides), recorded in a small state file under the
//!   user's cache directory.
//! * **Never blocking.** The foreground `ct` invocation only reads that cached
//!   state ([`on_invocation`]); the actual network poll runs in a **detached
//!   background process** ([`run_background_poll`]) that writes the state for a
//!   later run to notice. A `ct` command never waits on the network.
//!
//! Everything here is best-effort: any error — no network, a malformed index, an
//! unwritable cache — is swallowed silently. An update check must never fail a
//! command or print a diagnostic of its own.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// The published crate name (hyphenated), used for the index path and messages.
const PKG_NAME: &str = "coding-tools";
/// The project URL, included in the `User-Agent` so crates.io can identify us.
const REPO: &str = "https://github.com/jshook/coding-tools";
/// The sparse-index host (the CDN-fronted endpoint cargo itself uses).
const INDEX_HOST: &str = "https://index.crates.io";
/// The state file under the user cache dir.
const STATE_FILE: &str = "update-check.json";
/// The hidden `ct` flag that runs the background network poll.
pub const BG_FLAG: &str = "--update-check-run";
/// The default poll interval: once a day.
const DAILY: u64 = 86_400;

// ----- Configuration -----------------------------------------------------------

/// Parse a `CT_UPDATE_CHECK` value into a poll interval in seconds, or [`None`]
/// to disable the check entirely.
///
/// Accepts the friendly words `daily` (the default), `weekly`, `hourly`,
/// `always` (every run — for testing), and the off-switches `never` / `off` /
/// `no` / `false` / `0`; a bare positive integer is taken as seconds. Anything
/// unrecognised falls back to the daily default rather than disabling.
///
/// ```
/// use coding_tools::update::parse_interval;
/// assert_eq!(parse_interval(None), Some(86_400));
/// assert_eq!(parse_interval(Some("daily")), Some(86_400));
/// assert_eq!(parse_interval(Some("weekly")), Some(604_800));
/// assert_eq!(parse_interval(Some("never")), None);
/// assert_eq!(parse_interval(Some("0")), None);
/// assert_eq!(parse_interval(Some("3600")), Some(3_600));
/// assert_eq!(parse_interval(Some("always")), Some(0));
/// assert_eq!(parse_interval(Some("garbage")), Some(86_400));
/// ```
pub fn parse_interval(value: Option<&str>) -> Option<u64> {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("daily") => Some(DAILY),
        Some("never" | "off" | "no" | "false" | "0") => None,
        Some("weekly") => Some(7 * DAILY),
        Some("hourly") => Some(3_600),
        Some("always") => Some(0),
        Some(other) => Some(other.parse::<u64>().unwrap_or(DAILY)),
    }
}

/// The configured interval from the environment (`CT_UPDATE_CHECK`).
fn interval_from_env() -> Option<u64> {
    parse_interval(std::env::var("CT_UPDATE_CHECK").ok().as_deref())
}

// ----- Index path + version pick (pure) ----------------------------------------

/// The crates.io sparse-index path for a crate name, by cargo's rule: 1- and
/// 2-char names live under `1/`/`2/`, 3-char under `3/<first>/`, and everything
/// else under `<first-two>/<next-two>/`. The name is lower-cased.
///
/// ```
/// use coding_tools::update::index_path;
/// assert_eq!(index_path("coding-tools"), "co/di/coding-tools");
/// assert_eq!(index_path("a"), "1/a");
/// assert_eq!(index_path("ab"), "2/ab");
/// assert_eq!(index_path("abc"), "3/a/abc");
/// assert_eq!(index_path("serde"), "se/rd/serde");
/// ```
pub fn index_path(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    match n.len() {
        0 => n,
        1 => format!("1/{n}"),
        2 => format!("2/{n}"),
        3 => format!("3/{}/{}", &n[0..1], n),
        _ => format!("{}/{}/{}", &n[0..2], &n[2..4], n),
    }
}

/// The full sparse-index URL for a crate.
///
/// ```
/// use coding_tools::update::index_url;
/// assert_eq!(index_url("coding-tools"), "https://index.crates.io/co/di/coding-tools");
/// ```
pub fn index_url(name: &str) -> String {
    format!("{INDEX_HOST}/{}", index_path(name))
}

/// The highest non-yanked version in a sparse-index document (one JSON object
/// per line). Lines that don't parse, lack a `vers`, or are yanked are skipped;
/// [`None`] means nothing usable was found.
///
/// ```
/// use coding_tools::update::latest_from_index;
/// let body = r#"{"name":"x","vers":"0.8.3","yanked":false}
/// {"name":"x","vers":"0.9.0","yanked":false}
/// {"name":"x","vers":"0.10.0","yanked":true}"#;
/// assert_eq!(latest_from_index(body).as_deref(), Some("0.9.0"));
/// ```
pub fn latest_from_index(body: &str) -> Option<String> {
    let mut best: Option<(Version, String)> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("yanked").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(vers) = v.get("vers").and_then(Value::as_str) else {
            continue;
        };
        let Some(parsed) = Version::parse(vers) else {
            continue;
        };
        if best.as_ref().is_none_or(|(b, _)| parsed > *b) {
            best = Some((parsed, vers.to_string()));
        }
    }
    best.map(|(_, s)| s)
}

/// Whether `latest` is a strictly newer release than `current`. Unparsable
/// versions compare as "not newer" — we never nag on garbage.
///
/// ```
/// use coding_tools::update::is_newer;
/// assert!(is_newer("0.9.0", "0.8.4"));
/// assert!(is_newer("1.0.0", "1.0.0-rc.1")); // a release beats its pre-release
/// assert!(!is_newer("0.8.4", "0.8.4"));
/// assert!(!is_newer("0.8.3", "0.8.4"));
/// assert!(!is_newer("nonsense", "0.8.4"));
/// ```
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (Version::parse(latest), Version::parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// A minimal semantic version: the `major.minor.patch` core plus a pre-release
/// marker. Build metadata is ignored; pre-release identifiers compare as a
/// lexical fallback, which is ample for "is there a newer release than mine?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    core: (u64, u64, u64),
    /// `None` for a release; `Some(ids)` for a pre-release (e.g. `rc.1`).
    pre: Option<String>,
}

impl Version {
    /// Parse `MAJOR.MINOR.PATCH[-pre][+build]`; [`None`] if the core isn't three
    /// integers.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.split('+').next().unwrap_or(s); // drop build metadata
        let (core_str, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p.to_string())),
            None => (s, None),
        };
        let mut it = core_str.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        let patch = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None; // more than three core components
        }
        Some(Version {
            core: (major, minor, patch),
            pre,
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::Equal;
        match self.core.cmp(&other.core) {
            Equal => match (&self.pre, &other.pre) {
                // A release outranks a pre-release of the same core.
                (None, None) => Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            },
            ord => ord,
        }
    }
}

// ----- State (the cache file) --------------------------------------------------

/// The cached check state. All best-effort: a missing or corrupt file reads as
/// the default (a fresh install that has never checked).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct State {
    /// Unix seconds of the last poll attempt (0 = never).
    last_check: u64,
    /// Unix seconds we last printed the "update available" notice (0 = never).
    last_notified: u64,
    /// The highest version seen at the index, if any.
    latest: Option<String>,
    /// The `ETag` of the last index response, for conditional requests.
    etag: Option<String>,
    /// Whether the one-time "this checks for updates" notice has been shown.
    notice_shown: bool,
}

impl State {
    /// Read state from `path`, defaulting on any error.
    fn load(path: &Path) -> State {
        let Ok(text) = std::fs::read_to_string(path) else {
            return State::default();
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return State::default();
        };
        let u64f = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let strf = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        };
        State {
            last_check: u64f("last_check"),
            last_notified: u64f("last_notified"),
            latest: strf("latest"),
            etag: strf("etag"),
            notice_shown: v
                .get("notice_shown")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }

    /// Write state to `path` (creating the parent dir). Errors are ignored.
    fn save(&self, path: &Path) -> bool {
        if let Some(dir) = path.parent()
            && std::fs::create_dir_all(dir).is_err()
        {
            return false;
        }
        let v = json!({
            "last_check": self.last_check,
            "last_notified": self.last_notified,
            "latest": self.latest,
            "etag": self.etag,
            "notice_shown": self.notice_shown,
        });
        std::fs::write(path, format!("{v}\n")).is_ok()
    }
}

/// The user cache directory for the suite's state, honoring an explicit
/// `CT_STATE_DIR` override (handy for tests and unusual setups). Platform
/// defaults: `%LOCALAPPDATA%` on Windows, `~/Library/Caches` on macOS,
/// `$XDG_CACHE_HOME` (or `~/.cache`) elsewhere. [`None`] if none can be found.
fn state_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CT_STATE_DIR") {
        return Some(PathBuf::from(d));
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join(PKG_NAME))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Caches").join(PKG_NAME))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))
            .map(|p| p.join(PKG_NAME))
    }
}

/// Unix seconds now (0 if the clock is before the epoch — never panics).
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ----- Foreground: cheap, never blocks -----------------------------------------

/// Called once per real `ct` invocation. Reads the cached state, prints the
/// first-run and "update available" notices when due (only to a terminal, so
/// scripts and pipes stay clean), and — if a poll is due — claims the slot and
/// spawns the detached background poll. Does **no** network I/O itself. Silent
/// and infallible: any problem is swallowed.
pub fn on_invocation() {
    let _ = try_on_invocation();
}

fn try_on_invocation() -> Option<()> {
    let interval = interval_from_env()?; // None → disabled
    // Captured agent/CI calls are precisely where a nominally detached child
    // may remain attached to the harness job and add its network timeout to the
    // foreground command. Update notices are interactive anyway, so do not
    // schedule polling from a non-terminal invocation.
    {
        use std::io::IsTerminal;
        if !std::io::stderr().is_terminal() {
            return Some(());
        }
    }
    let dir = state_dir()?;
    let path = dir.join(STATE_FILE);
    let mut state = State::load(&path);

    let now = unix_now();
    let current = env!("CARGO_PKG_VERSION");
    let tty = {
        use std::io::IsTerminal;
        std::io::stderr().is_terminal()
    };

    // One-time "we check for updates" notice (only shown interactively, and only
    // marked shown once it actually has been).
    if tty && !state.notice_shown {
        eprint!("{}", first_run_notice());
        state.notice_shown = true;
    }

    // "A newer version is available", from cache, at most once per interval.
    if tty
        && let Some(latest) = state.latest.clone()
        && is_newer(&latest, current)
        && now.saturating_sub(state.last_notified) >= interval
    {
        eprint!("{}", update_available_notice(&latest, current));
        state.last_notified = now;
    }

    // Claim and spawn the background poll when due. The claim is persisted before
    // spawning so concurrent `ct` runs don't each launch a poller.
    let due = now.saturating_sub(state.last_check) >= interval;
    if due {
        state.last_check = now;
    }
    let claimed = state.save(&path);
    if due && claimed {
        spawn_background();
    }
    Some(())
}

/// Spawn `ct --update-check-run` as a detached, output-suppressed background
/// process. Best-effort: a spawn failure is ignored.
fn spawn_background() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg(BG_FLAG)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    let _ = cmd.spawn();
}

// ----- Background: the actual network poll -------------------------------------

/// The entry point for `ct --update-check-run`: perform one conditional GET
/// against the sparse index and update the cached state. Silent and infallible.
pub fn run_background_poll() {
    let _ = try_poll();
}

fn try_poll() -> Option<()> {
    interval_from_env()?; // honor `CT_UPDATE_CHECK=never` even here
    let dir = state_dir()?;
    let path = dir.join(STATE_FILE);
    let mut state = State::load(&path);

    match fetch(env!("CARGO_PKG_VERSION"), state.etag.as_deref()) {
        Fetch::Updated { latest, etag } => {
            state.latest = Some(latest);
            if etag.is_some() {
                state.etag = etag;
            }
        }
        Fetch::NotModified | Fetch::Failed => {}
    }
    state.last_check = unix_now();
    let _ = state.save(&path);
    Some(())
}

/// The outcome of one index fetch.
enum Fetch {
    /// `200 OK`: the highest version parsed from the body, plus the new `ETag`.
    Updated {
        latest: String,
        etag: Option<String>,
    },
    /// `304 Not Modified`: the cached `latest`/`etag` still stand.
    NotModified,
    /// Any network or protocol error — left for the next interval.
    Failed,
}

/// One conditional GET of the crate's sparse-index file.
fn fetch(current: &str, etag: Option<&str>) -> Fetch {
    let url = index_url(PKG_NAME);
    let ua = format!("{PKG_NAME}/{current} ({REPO})");
    // `http_status_as_error(false)` so a 304 arrives as a normal response we can
    // inspect, rather than an error.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    let mut req = agent.get(&url).header("User-Agent", &ua);
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    let Ok(mut resp) = req.call() else {
        return Fetch::Failed;
    };
    let status = resp.status().as_u16();
    if status == 304 {
        return Fetch::NotModified;
    }
    if status != 200 {
        return Fetch::Failed;
    }
    let new_etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    match resp.body_mut().read_to_string() {
        Ok(body) => match latest_from_index(&body) {
            Some(latest) => Fetch::Updated {
                latest,
                etag: new_etag,
            },
            None => Fetch::Failed,
        },
        Err(_) => Fetch::Failed,
    }
}

// ----- Notices -----------------------------------------------------------------

/// The one-time notice shown on first interactive use.
fn first_run_notice() -> String {
    format!(
        "{PKG_NAME}: checking crates.io for updates about once a day, in the background.\n\
         {PKG_NAME}: set CT_UPDATE_CHECK=never to disable (or =weekly / =hourly / a number of seconds).\n"
    )
}

/// The "a newer version is available" notice.
fn update_available_notice(latest: &str, current: &str) -> String {
    format!(
        "{PKG_NAME}: a newer version is available: {latest} (you have {current}).\n\
         {PKG_NAME}: update with `cargo install {PKG_NAME}` — or set CT_UPDATE_CHECK=never to silence.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_orders_core_and_prerelease() {
        let v = Version::parse;
        assert!(v("0.9.0").unwrap() > v("0.8.4").unwrap());
        assert!(v("1.0.0").unwrap() > v("0.99.99").unwrap());
        assert!(v("1.2.10").unwrap() > v("1.2.9").unwrap());
        // a release outranks its own pre-release; pre-releases order lexically
        assert!(v("1.0.0").unwrap() > v("1.0.0-rc.1").unwrap());
        assert!(v("1.0.0-rc.2").unwrap() > v("1.0.0-rc.1").unwrap());
        // build metadata is ignored
        assert_eq!(v("1.2.3+abc").unwrap(), v("1.2.3").unwrap());
        // malformed cores don't parse
        assert!(v("1.2").is_none());
        assert!(v("1.2.3.4").is_none());
        assert!(v("x.y.z").is_none());
    }

    #[test]
    fn latest_from_index_picks_highest_unyanked() {
        let body = "\
{\"name\":\"coding-tools\",\"vers\":\"0.8.3\",\"yanked\":false}\n\
{\"name\":\"coding-tools\",\"vers\":\"0.8.4\",\"yanked\":false}\n\
{\"name\":\"coding-tools\",\"vers\":\"0.9.0\",\"yanked\":true}\n\
not even json\n\
{\"name\":\"coding-tools\",\"vers\":\"0.8.10\",\"yanked\":false}\n";
        assert_eq!(latest_from_index(body).as_deref(), Some("0.8.10"));
        // an all-yanked / empty document yields nothing
        assert_eq!(latest_from_index("").as_deref(), None);
        assert_eq!(
            latest_from_index("{\"vers\":\"1.0.0\",\"yanked\":true}").as_deref(),
            None
        );
    }

    #[test]
    fn state_round_trips_through_a_file() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/update");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("state.json");
        let _ = std::fs::remove_file(&path);

        // a missing file reads as default
        assert_eq!(State::load(&path), State::default());

        let s = State {
            last_check: 111,
            last_notified: 222,
            latest: Some("0.9.0".to_string()),
            etag: Some("\"abc\"".to_string()),
            notice_shown: true,
        };
        assert!(s.save(&path));
        assert_eq!(State::load(&path), s);

        // a corrupt file also reads as default
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(State::load(&path), State::default());
    }

    #[test]
    fn notices_name_the_versions_and_the_off_switch() {
        let avail = update_available_notice("0.9.0", "0.8.4");
        assert!(
            avail.contains("0.9.0") && avail.contains("0.8.4"),
            "{avail}"
        );
        assert!(avail.contains("cargo install coding-tools"), "{avail}");
        assert!(avail.contains("CT_UPDATE_CHECK=never"), "{avail}");

        let first = first_run_notice();
        assert!(first.contains("once a day"), "{first}");
        assert!(first.contains("CT_UPDATE_CHECK=never"), "{first}");
    }

    #[test]
    fn empty_string_etag_loads_as_none() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/update");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("state-empty.json");
        std::fs::write(&path, r#"{"etag":"","latest":""}"#).unwrap();
        let s = State::load(&path);
        assert_eq!(s.etag, None);
        assert_eq!(s.latest, None);
    }
}
