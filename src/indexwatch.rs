// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! Opportunistic filesystem watcher for the OKF index.
//!
//! The daemon is deliberately an optimization: clients use a bounded file-based
//! barrier when it is healthy and run synchronous reconciliation otherwise.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use notify::{RecursiveMode, Watcher};
use serde_json::{Value, json};

use crate::indexing::{self, Plan, ScanMetrics};
use crate::okfindex::{Index, UpdateReport};
use crate::{okfindex, okfroots};

const RUNTIME_DIR: &str = "runtime";
const STATUS_FILE: &str = "watch-status.json";
const START_CLAIM: &str = "start.claim";
const START_FAILURE: &str = "start.failed";
const LIFECYCLE_LOG: &str = "daemon.log";
const LIFECYCLE_LOG_MAX_BYTES: u64 = 32 * 1024;
const LIFECYCLE_LOG_ROTATIONS: usize = 2;
const UPDATE_LOCK: &str = "update.lock";
const DAEMON_LOCK: &str = "daemon.lock";
const STOP_FILE: &str = "stop.request";
const REQUESTS_DIR: &str = "requests";
const HEALTHY_MS: u64 = 3_000;
const BARRIER_MS: u64 = 750;
const START_CLAIM_STALE_SECONDS: u64 = 10;

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn shutdown_signal(_signal: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
fn install_shutdown_handlers() -> Result<(), String> {
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
    // SAFETY: the handler only stores to a lock-free atomic, and `sigaction`
    // copies the action before this stack frame ends.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = shutdown_signal as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(format!(
                    "install signal handler {signal}: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
unsafe extern "system" fn shutdown_console_event(event: u32) -> i32 {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };

    if matches!(
        event,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    ) {
        SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
        1
    } else {
        0
    }
}

#[cfg(windows)]
fn install_shutdown_handlers() -> Result<(), String> {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
    // SAFETY: the callback has the required ABI, is process-static, and only
    // touches a lock-free atomic flag.
    if unsafe { SetConsoleCtrlHandler(Some(shutdown_console_event), 1) } == 0 {
        Err(format!(
            "install console control handler: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn install_shutdown_handlers() -> Result<(), String> {
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
    Ok(())
}

pub fn runtime_dir(project: &Path) -> PathBuf {
    okfroots::index_dir(project).join(RUNTIME_DIR)
}

fn status_path(project: &Path) -> PathBuf {
    runtime_dir(project).join(STATUS_FILE)
}

fn requests_dir(project: &Path) -> PathBuf {
    runtime_dir(project).join(REQUESTS_DIR)
}

/// Append one sparse lifecycle record, rotating before the active log exceeds
/// 32 KiB. Logging is diagnostic only and must never affect daemon behavior.
fn lifecycle_log(project: &Path, event: &str) {
    let dir = runtime_dir(project);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(LIFECYCLE_LOG);
    let mut event = event.replace(['\r', '\n'], " ");
    if event.len() > 2048 {
        let mut end = 2048;
        while !event.is_char_boundary(end) {
            end -= 1;
        }
        event.truncate(end);
        event.push_str("...");
    }
    let line = format!(
        "{} pid={} {event}\n",
        indexing::unix_millis(),
        std::process::id()
    );
    let rotate = std::fs::metadata(&path).is_ok_and(|metadata| {
        metadata.len().saturating_add(line.len() as u64) > LIFECYCLE_LOG_MAX_BYTES
    });
    if rotate {
        let oldest = dir.join(format!("{LIFECYCLE_LOG}.{LIFECYCLE_LOG_ROTATIONS}"));
        let _ = std::fs::remove_file(oldest);
        for generation in (1..LIFECYCLE_LOG_ROTATIONS).rev() {
            let from = dir.join(format!("{LIFECYCLE_LOG}.{generation}"));
            let to = dir.join(format!("{LIFECYCLE_LOG}.{}", generation + 1));
            let _ = std::fs::rename(from, to);
        }
        let _ = std::fs::rename(&path, dir.join(format!("{LIFECYCLE_LOG}.1")));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    crate::atomicfile::write(path, format!("{}\n", value))
}

#[derive(Debug, Clone)]
pub struct Status {
    pub pid: u64,
    pub heartbeat_ms: u64,
    pub lane: String,
    pub backend: String,
    pub generation: u64,
    pub dirty_paths: usize,
    pub last_reconcile_ms: u64,
    pub entries_visited: usize,
    pub last_batch_ms: u64,
    pub last_batch_paths: usize,
    pub last_event_latency_ms: u64,
    pub source_bytes: u64,
    pub index_bytes: u64,
    pub documents: usize,
    pub segments: usize,
    pub tombstones: usize,
    pub memory_rss_bytes: u64,
    pub memory_limit_bytes: u64,
    pub system_memory_bytes: u64,
    pub started_ms: u64,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            pid: std::process::id() as u64,
            heartbeat_ms: indexing::unix_millis(),
            lane: "unavailable".to_string(),
            backend: "none".to_string(),
            generation: 0,
            dirty_paths: 0,
            last_reconcile_ms: 0,
            entries_visited: 0,
            last_batch_ms: 0,
            last_batch_paths: 0,
            last_event_latency_ms: 0,
            source_bytes: 0,
            index_bytes: 0,
            documents: 0,
            segments: 0,
            tombstones: 0,
            memory_rss_bytes: 0,
            memory_limit_bytes: 0,
            system_memory_bytes: 0,
            started_ms: indexing::unix_millis(),
        }
    }
}

impl Status {
    pub fn to_json(&self) -> Value {
        json!({
            "pid": self.pid,
            "heartbeat_ms": self.heartbeat_ms,
            "lane": self.lane,
            "backend": self.backend,
            "generation": self.generation,
            "dirty_paths": self.dirty_paths,
            "last_reconcile_ms": self.last_reconcile_ms,
            "entries_visited": self.entries_visited,
            "last_batch_ms": self.last_batch_ms,
            "last_batch_paths": self.last_batch_paths,
            "last_event_latency_ms": self.last_event_latency_ms,
            "source_bytes": self.source_bytes,
            "index_bytes": self.index_bytes,
            "documents": self.documents,
            "segments": self.segments,
            "tombstones": self.tombstones,
            "memory_rss_bytes": self.memory_rss_bytes,
            "memory_limit_bytes": self.memory_limit_bytes,
            "system_memory_bytes": self.system_memory_bytes,
            "started_ms": self.started_ms,
        })
    }

    fn from_json(v: &Value) -> Option<Status> {
        let u = |name: &str| v.get(name).and_then(Value::as_u64).unwrap_or(0);
        Some(Status {
            pid: u("pid"),
            heartbeat_ms: u("heartbeat_ms"),
            lane: v.get("lane")?.as_str()?.to_string(),
            backend: v
                .get("backend")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            generation: u("generation"),
            dirty_paths: u("dirty_paths") as usize,
            last_reconcile_ms: u("last_reconcile_ms"),
            entries_visited: u("entries_visited") as usize,
            last_batch_ms: u("last_batch_ms"),
            last_batch_paths: u("last_batch_paths") as usize,
            last_event_latency_ms: u("last_event_latency_ms"),
            source_bytes: u("source_bytes"),
            index_bytes: u("index_bytes"),
            documents: u("documents") as usize,
            segments: u("segments") as usize,
            tombstones: u("tombstones") as usize,
            memory_rss_bytes: u("memory_rss_bytes"),
            memory_limit_bytes: u("memory_limit_bytes"),
            system_memory_bytes: u("system_memory_bytes"),
            started_ms: u("started_ms"),
        })
    }

    pub fn healthy(&self) -> bool {
        self.lane != "unavailable"
            && indexing::unix_millis().saturating_sub(self.heartbeat_ms) <= HEALTHY_MS
    }
}

pub fn read_status(project: &Path) -> Option<Status> {
    let text = std::fs::read_to_string(status_path(project)).ok()?;
    let mut status = Status::from_json(&serde_json::from_str(&text).ok()?)?;
    if status.lane != "unavailable" && !daemon_running(project).unwrap_or(false) {
        status.lane = "unavailable".to_string();
    }
    Some(status)
}

pub fn start_failure(project: &Path) -> Option<String> {
    std::fs::read_to_string(runtime_dir(project).join(START_FAILURE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_status(project: &Path, status: &mut Status) -> Result<(), String> {
    status.heartbeat_ms = indexing::unix_millis();
    atomic_json(&status_path(project), &status.to_json())
}

/// An OS-backed cross-process lock. It prevents the daemon and a synchronous
/// fallback from publishing competing manifests, and is released on crash.
struct UpdateGuard {
    file: std::fs::File,
}

/// The per-project process authority. Unlike status or PID files, this lock is
/// released by the operating system even after an ungraceful process exit.
struct DaemonGuard {
    file: std::fs::File,
}

struct StartClaimGuard(PathBuf);

impl Drop for StartClaimGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl UpdateGuard {
    fn acquire(project: &Path, timeout: Duration) -> Result<UpdateGuard, String> {
        let dir = runtime_dir(project);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(UPDATE_LOCK);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let start = Instant::now();
        loop {
            match try_lock(&file) {
                Ok(true) => return Ok(UpdateGuard { file }),
                Ok(false) => {
                    if start.elapsed() >= timeout {
                        return Err(format!(
                            "timed out waiting for index update lock {}",
                            path.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
    }
}

impl DaemonGuard {
    fn try_acquire(project: &Path) -> Result<Option<DaemonGuard>, String> {
        let dir = runtime_dir(project);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(DAEMON_LOCK);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        try_lock(&file)
            .map(|acquired| acquired.then_some(DaemonGuard { file }))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

impl Drop for UpdateGuard {
    fn drop(&mut self) {
        unlock(&self.file);
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        unlock(&self.file);
    }
}

fn daemon_running(project: &Path) -> Result<bool, String> {
    Ok(DaemonGuard::try_acquire(project)?.is_none())
}

#[cfg(windows)]
fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::LockFile;

    // SAFETY: the handle remains valid for the call and the byte range is a
    // conventional whole-file advisory lock shared by all ct writers.
    if unsafe { LockFile(file.as_raw_handle() as _, 0, 0, u32::MAX, u32::MAX) } != 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(33) {
        // ERROR_LOCK_VIOLATION means another process owns the range.
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(windows)]
fn unlock(file: &std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFile;

    // SAFETY: this uses the same live handle and range passed to `LockFile`.
    let _ = unsafe { UnlockFile(file.as_raw_handle() as _, 0, 0, u32::MAX, u32::MAX) };
}

#[cfg(unix)]
fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;

    // SAFETY: `file` owns a valid descriptor for the duration of this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(unix)]
fn unlock(file: &std::fs::File) {
    use std::os::fd::AsRawFd;

    // SAFETY: `file` owns a valid descriptor for the duration of this call.
    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

#[cfg(not(any(unix, windows)))]
fn try_lock(_file: &std::fs::File) -> std::io::Result<bool> {
    Ok(true)
}

#[cfg(not(any(unix, windows)))]
fn unlock(_file: &std::fs::File) {}

/// The authoritative synchronous reconciliation path shared by normal commands
/// and the watcher. Events only decide when this needs to run.
pub fn reconcile(
    project: &Path,
    plan: &Plan,
) -> Result<(Index, UpdateReport, ScanMetrics), String> {
    let _guard = UpdateGuard::acquire(project, Duration::from_secs(30))?;
    let mut idx = okfindex::Index::open(&okfroots::index_dir(project))?;
    let (files, metrics) = indexing::scan(plan);
    let report = idx.update(&files, |f| okfroots::load_doc(&f.path))?;
    if !report.is_empty() {
        idx.save()?;
    }
    Ok((idx, report, metrics))
}

/// Apply a coalesced native-event dirty set without walking unchanged scopes.
/// `None` requests a full reconciliation because a directory or ignore policy
/// changed and a path-local delta cannot prove completeness.
fn apply_dirty(
    project: &Path,
    plan: &Plan,
    dirty: &BTreeSet<PathBuf>,
) -> Result<Option<(Index, UpdateReport, ScanMetrics)>, String> {
    let started = Instant::now();
    let mut upserts = Vec::new();
    let mut removed = BTreeSet::new();
    let mut metrics = ScanMetrics {
        entries_visited: dirty.len(),
        ..ScanMetrics::default()
    };
    for path in dirty {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(name, ".gitignore" | ".ignore") {
            return Ok(None);
        }
        let key = path
            .strip_prefix(&plan.project)
            .map(indexing::path_key)
            .unwrap_or_else(|_| indexing::path_key(path));
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => return Ok(None),
            Ok(meta) if meta.is_file() => {
                metrics.files_considered += 1;
                if plan.decide(path, Some(&meta)).included {
                    metrics.files_included += 1;
                    metrics.logical_bytes += meta.len();
                    upserts.push(indexing::file_stat(plan, path, &meta));
                } else {
                    removed.insert(key);
                }
            }
            Ok(_) => {
                removed.insert(key);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // The path may have been a file or a whole directory. Prefix
                // removal is safe for both and is scoped to existing manifest keys.
                removed.insert(key);
            }
            Err(_) => return Ok(None),
        }
    }
    let _guard = UpdateGuard::acquire(project, Duration::from_secs(30))?;
    let mut idx = okfindex::Index::open(&okfroots::index_dir(project))?;
    let removed: Vec<String> = removed.into_iter().collect();
    let report = idx.update_delta(&upserts, &removed, |f| okfroots::load_doc(&f.path))?;
    if !report.is_empty() {
        idx.save()?;
    }
    metrics.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(Some((idx, report, metrics)))
}

pub fn condense(project: &Path) -> Result<(Index, bool), String> {
    let _guard = UpdateGuard::acquire(project, Duration::from_secs(30))?;
    let mut idx = okfindex::Index::open(&okfroots::index_dir(project))?;
    let changed = idx.condense()?;
    if changed {
        idx.save()?;
    }
    Ok((idx, changed))
}

pub fn rebuild(project: &Path, plan: &Plan) -> Result<(Index, UpdateReport, ScanMetrics), String> {
    let _guard = UpdateGuard::acquire(project, Duration::from_secs(30))?;
    let mut idx = okfindex::Index::open(&okfroots::index_dir(project))?;
    idx.reset();
    let (files, metrics) = indexing::scan(plan);
    let report = idx.update(&files, |f| okfroots::load_doc(&f.path))?;
    idx.save()?;
    Ok((idx, report, metrics))
}

fn refresh_metrics(project: &Path, status: &mut Status, idx: &Index) {
    status.generation = idx.generation();
    status.source_bytes = idx.source_bytes();
    status.index_bytes = indexing::directory_bytes(&okfroots::index_dir(project));
    status.documents = idx.doc_count();
    status.segments = idx.segment_count();
    status.tombstones = idx.tombstone_count();
}

struct MemoryMonitor {
    system: sysinfo::System,
    pid: sysinfo::Pid,
}

impl MemoryMonitor {
    fn new() -> Result<MemoryMonitor, String> {
        let pid = sysinfo::get_current_pid().map_err(|e| format!("current process id: {e}"))?;
        Ok(MemoryMonitor {
            system: sysinfo::System::new(),
            pid,
        })
    }

    fn rss_bytes(&mut self) -> u64 {
        self.system
            .refresh_processes(sysinfo::ProcessesToUpdate::Some(&[self.pid]), true);
        self.system
            .process(self.pid)
            .map(sysinfo::Process::memory)
            .unwrap_or(0)
    }
}

/// Ask a healthy daemon to drain observed events. Returns false quickly when no
/// trustworthy daemon is present so the caller can reconcile synchronously.
pub fn barrier(project: &Path) -> bool {
    let Some(status) = read_status(project) else {
        return false;
    };
    if !status.healthy() {
        return false;
    }
    let dir = requests_dir(project);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let id = format!("{}-{}", std::process::id(), indexing::unix_millis());
    let req = dir.join(format!("{id}.req"));
    let ack = dir.join(format!("{id}.ack"));
    if std::fs::write(&req, b"flush\n").is_err() {
        return false;
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(BARRIER_MS) {
        if ack.is_file() {
            let _ = std::fs::remove_file(&ack);
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_file(req);
    false
}

fn env_disabled() -> bool {
    matches!(
        std::env::var("CT_INDEX_WATCH").ok().as_deref(),
        Some("never" | "off" | "false" | "0")
    )
}

/// Start one detached watcher if the per-project OS lock proves that no daemon
/// exists. The short-lived file claim closes the parent/child lock-handoff race
/// and is age-recovered if a child dies before entering the daemon body.
pub fn ensure_started(exe: &Path, project: &Path, plan: &Plan) -> Result<bool, String> {
    if !plan.watch || env_disabled() {
        return Ok(false);
    }
    let dir = runtime_dir(project);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if daemon_running(project)? {
        return Ok(false);
    }
    let failed = dir.join(START_FAILURE);
    let recent_failure = std::fs::metadata(&failed)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < Duration::from_secs(60));
    if recent_failure {
        return Ok(false);
    }
    let claim = dir.join(START_CLAIM);
    let mut recovered = false;
    let mut claim_file = loop {
        match OpenOptions::new().write(true).create_new(true).open(&claim) {
            Ok(f) => break f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && !recovered => {
                let stale = std::fs::metadata(&claim)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| SystemTime::now().duration_since(t).ok())
                    .is_some_and(|age| age > Duration::from_secs(START_CLAIM_STALE_SECONDS));
                if !stale {
                    return Ok(false);
                }
                std::fs::remove_file(&claim)
                    .map_err(|e| format!("remove stale {}: {e}", claim.display()))?;
                recovered = true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(e) => return Err(format!("{}: {e}", claim.display())),
        }
    };
    // Another launcher may have won immediately before this claim was created.
    if daemon_running(project)? {
        let _ = std::fs::remove_file(&claim);
        return Ok(false);
    }
    writeln!(claim_file, "{}", indexing::unix_millis())
        .map_err(|e| format!("{}: {e}", claim.display()))?;
    let mut cmd = Command::new(exe);
    cmd.args([
        "--base",
        &project.to_string_lossy(),
        "index",
        "watch",
        "run",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // SAFETY: `setsid` is async-signal-safe and the closure does not touch
        // shared Rust state between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    match cmd.spawn() {
        Ok(_) => {
            let _ = std::fs::remove_file(failed);
            Ok(true)
        }
        Err(e) => {
            let _ = std::fs::remove_file(claim);
            let _ = std::fs::write(&failed, format!("{} {e}\n", indexing::unix_millis()));
            Err(format!("start index watcher: {e}"))
        }
    }
}

pub fn request_stop(project: &Path) -> Result<bool, String> {
    if !daemon_running(project)? {
        return Ok(false);
    }
    let path = runtime_dir(project).join(STOP_FILE);
    std::fs::write(&path, b"stop\n").map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

fn pending_requests(project: &Path, settle: Duration) -> Vec<PathBuf> {
    let Ok(items) = std::fs::read_dir(requests_dir(project)) else {
        return Vec::new();
    };
    items
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("req"))
        // Give the native backend one debounce window to deliver filesystem
        // events that happened immediately before the freshness request.
        .filter(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age >= settle)
        })
        .collect()
}

fn acknowledge(requests: Vec<PathBuf>) {
    for req in requests {
        let ack = req.with_extension("ack");
        let _ = std::fs::rename(req, ack);
    }
}

/// Foreground body of the detached watcher process. Standard streams remain
/// detached; only sparse lifecycle events are written to the bounded log.
pub fn run_daemon(project: &Path, plan: Plan) -> Result<(), String> {
    install_shutdown_handlers()?;
    run_daemon_logged(project, plan)
}

fn run_daemon_logged(project: &Path, plan: Plan) -> Result<(), String> {
    lifecycle_log(project, "start");
    let result = run_daemon_body(project, plan);
    if !matches!(&result, Ok(reason) if reason == "duplicate") {
        let mut status = read_status(project).unwrap_or_default();
        status.lane = "unavailable".to_string();
        let _ = write_status(project, &mut status);
    }
    match &result {
        Ok(reason) => lifecycle_log(project, &format!("stop reason={reason}")),
        Err(error) => {
            lifecycle_log(project, &format!("stop reason=error detail={error}"));
            let failed = runtime_dir(project).join(START_FAILURE);
            let _ = std::fs::write(&failed, format!("{} {error}\n", indexing::unix_millis()));
        }
    }
    result.map(|_| ())
}

fn run_daemon_body(project: &Path, plan: Plan) -> Result<String, String> {
    let dir = runtime_dir(project);
    let start_claim = StartClaimGuard(dir.join(START_CLAIM));
    let Some(_daemon_guard) = DaemonGuard::try_acquire(project)? else {
        return Ok("duplicate".to_string());
    };
    drop(start_claim);
    if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
        return Ok("signal".to_string());
    }
    let _ = std::fs::remove_file(dir.join(START_FAILURE));
    std::fs::create_dir_all(requests_dir(project))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = std::fs::remove_file(dir.join(STOP_FILE));

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx).map_err(|e| format!("watcher: {e}"))?;
    for scope in &plan.scopes {
        watcher
            .watch(&scope.root, RecursiveMode::Recursive)
            .map_err(|e| format!("watch {}: {e}", scope.root.display()))?;
    }
    let ct_dir = project.join(".ct");
    if ct_dir.is_dir() {
        watcher
            .watch(&ct_dir, RecursiveMode::NonRecursive)
            .map_err(|e| format!("watch {}: {e}", ct_dir.display()))?;
    }
    if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
        return Ok("signal".to_string());
    }

    let mut status = Status {
        lane: "reconcile".to_string(),
        backend: format!("{:?}", notify::RecommendedWatcher::kind()).to_ascii_lowercase(),
        memory_limit_bytes: plan.daemon_memory_limit_bytes,
        system_memory_bytes: plan.system_memory_bytes,
        ..Status::default()
    };
    let mut memory_monitor = MemoryMonitor::new()?;
    // Watcher is installed before the initial full comparison, closing the
    // usual scan-then-watch race. Events received during it remain queued.
    let (idx, _, initial) = reconcile(project, &plan)?;
    status.last_reconcile_ms = initial.elapsed_ms;
    status.entries_visited = initial.entries_visited;
    status.lane = "clean".to_string();
    refresh_metrics(project, &mut status, &idx);
    status.memory_rss_bytes = memory_monitor.rss_bytes();
    write_status(project, &mut status)?;
    if status.memory_rss_bytes > status.memory_limit_bytes {
        status.lane = "unavailable".to_string();
        write_status(project, &mut status)?;
        return Ok(format!(
            "memory-limit used={} limit={}",
            status.memory_rss_bytes, status.memory_limit_bytes
        ));
    }

    let mut dirty = BTreeSet::<PathBuf>::new();
    let mut first_dirty: Option<Instant> = None;
    let mut last_activity = Instant::now();
    let mut last_audit = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut plan_changed = false;
    let mut needs_reconcile = false;
    let stop_reason = loop {
        if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
            status.lane = "unavailable".to_string();
            write_status(project, &mut status)?;
            break "signal".to_string();
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok(event)) => {
                for path in event.paths {
                    if path == plan.config_path || path == project.join(".ct/okf.jsonc") {
                        plan_changed = true;
                        continue;
                    }
                    // Index storage is hard-excluded; avoid self-generated loops.
                    if !path.starts_with(okfroots::index_dir(project)) {
                        dirty.insert(path);
                    }
                }
                if !dirty.is_empty() {
                    first_dirty.get_or_insert_with(Instant::now);
                    status.lane = "dirty".to_string();
                    status.dirty_paths = dirty.len();
                }
            }
            Ok(Err(_)) => {
                status.lane = "reconcile".to_string();
                needs_reconcile = true;
                first_dirty.get_or_insert_with(Instant::now);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                status.lane = "unavailable".to_string();
                write_status(project, &mut status)?;
                break "watcher-disconnected".to_string();
            }
        }

        if plan_changed {
            // A client will load the new plan, reconcile synchronously, and
            // start a replacement watcher on its next indexed operation.
            status.lane = "unavailable".to_string();
            write_status(project, &mut status)?;
            break "configuration-changed".to_string();
        }

        let requests = pending_requests(project, Duration::from_millis(plan.debounce_ms));
        let debounce_due =
            first_dirty.is_some_and(|t| t.elapsed() >= Duration::from_millis(plan.debounce_ms));
        let audit_due = last_audit.elapsed() >= Duration::from_secs(plan.audit_seconds);
        if debounce_due || audit_due || !requests.is_empty() && !dirty.is_empty() {
            let started = Instant::now();
            let batch_paths = dirty.len();
            let event_age = first_dirty
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0);
            status.lane = "reconcile".to_string();
            let delta = if audit_due || needs_reconcile {
                None
            } else {
                apply_dirty(project, &plan, &dirty)?
            };
            let (idx, _, scan, did_full_reconcile) = match delta {
                Some((idx, report, metrics)) => (idx, report, metrics, false),
                None => {
                    let (idx, report, metrics) = reconcile(project, &plan)?;
                    (idx, report, metrics, true)
                }
            };
            status.last_batch_ms = started.elapsed().as_millis() as u64;
            status.last_batch_paths = batch_paths;
            status.last_event_latency_ms = event_age;
            if did_full_reconcile {
                status.last_reconcile_ms = scan.elapsed_ms;
                status.entries_visited = scan.entries_visited;
            }
            status.lane = "clean".to_string();
            status.dirty_paths = 0;
            refresh_metrics(project, &mut status, &idx);
            dirty.clear();
            first_dirty = None;
            needs_reconcile = false;
            last_audit = Instant::now();
            last_activity = Instant::now();
        }
        if !requests.is_empty() {
            // Any dirty work was drained above. If there was none, the current
            // clean generation itself satisfies the barrier.
            acknowledge(requests);
            last_activity = Instant::now();
        }

        if last_heartbeat.elapsed() >= Duration::from_secs(1) {
            status.memory_rss_bytes = memory_monitor.rss_bytes();
            if status.memory_rss_bytes > status.memory_limit_bytes {
                status.lane = "unavailable".to_string();
                write_status(project, &mut status)?;
                break format!(
                    "memory-limit used={} limit={}",
                    status.memory_rss_bytes, status.memory_limit_bytes
                );
            }
            write_status(project, &mut status)?;
            last_heartbeat = Instant::now();
        }
        if dir.join(STOP_FILE).is_file() {
            status.lane = "unavailable".to_string();
            write_status(project, &mut status)?;
            break "requested".to_string();
        }
        if last_activity.elapsed() >= Duration::from_secs(plan.idle_seconds) {
            status.lane = "unavailable".to_string();
            write_status(project, &mut status)?;
            break "idle".to_string();
        }
    };
    let _ = std::fs::remove_file(dir.join(STOP_FILE));
    Ok(stop_reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    static DAEMON_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn stale_status_is_not_healthy() {
        let status = Status {
            lane: "clean".into(),
            heartbeat_ms: indexing::unix_millis().saturating_sub(HEALTHY_MS + 1),
            ..Status::default()
        };
        assert!(!status.healthy());
    }

    #[test]
    fn watcher_barrier_reflects_create_modify_and_remove() {
        let _serial = DAEMON_TEST_LOCK.lock().unwrap();
        let project = std::env::temp_dir().join(format!(
            "ct-indexwatch-{}-{}",
            std::process::id(),
            indexing::unix_millis()
        ));
        let root = project.join("kb");
        std::fs::create_dir_all(project.join(".ct")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let concept = root.join("a.md");
        std::fs::write(&concept, "---\ntype: Note\ntitle: A\n---\nalpha\n").unwrap();
        let plan = Plan {
            project: project.clone(),
            scopes: vec![crate::indexing::Scope {
                root: root.clone(),
                provider: crate::indexing::PROVIDER_OKF.to_string(),
                include: vec!["**/*.md".to_string()],
                exclude: vec![],
                origin: crate::indexing::Origin::Derived,
            }],
            exclude: vec![],
            watch: true,
            debounce_ms: 50,
            audit_seconds: 60,
            idle_seconds: 10,
            max_file_bytes: 1024 * 1024,
            system_memory_bytes: 8 * 1024 * 1024 * 1024,
            daemon_memory_limit_bytes: 2 * 1024 * 1024 * 1024,
            daemon_memory_limit_automatic: true,
            config_path: project.join(".ct/index.jsonc"),
        };
        let daemon_project = project.clone();
        let daemon = std::thread::spawn(move || run_daemon_logged(&daemon_project, plan).unwrap());
        let start = Instant::now();
        while !read_status(&project).is_some_and(|s| s.healthy()) {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "watcher did not become healthy"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(daemon_running(&project).unwrap());
        assert!(!runtime_dir(&project).join(START_CLAIM).exists());
        assert!(
            Index::open(&okfroots::index_dir(&project))
                .unwrap()
                .search("alpha", 10)
                .unwrap()
                .len()
                == 1
        );

        std::fs::write(&concept, "---\ntype: Note\ntitle: A\n---\nbeta\n").unwrap();
        assert!(barrier(&project), "modify barrier timed out");
        let idx = Index::open(&okfroots::index_dir(&project)).unwrap();
        assert!(idx.search("alpha", 10).unwrap().is_empty());
        assert_eq!(idx.search("beta", 10).unwrap().len(), 1);

        std::fs::remove_file(&concept).unwrap();
        assert!(barrier(&project), "remove barrier timed out");
        assert!(
            Index::open(&okfroots::index_dir(&project))
                .unwrap()
                .search("beta", 10)
                .unwrap()
                .is_empty()
        );
        request_stop(&project).unwrap();
        daemon.join().unwrap();
        assert!(!daemon_running(&project).unwrap());
        let log = std::fs::read_to_string(runtime_dir(&project).join(LIFECYCLE_LOG)).unwrap();
        assert!(log.contains(" start\n"), "{log}");
        assert!(log.contains("stop reason=requested"), "{log}");
    }

    #[test]
    fn lifecycle_log_is_bounded_and_rotates() {
        let project = std::env::temp_dir().join(format!(
            "ct-indexwatch-log-{}-{}",
            std::process::id(),
            indexing::unix_millis()
        ));
        for _ in 0..180 {
            lifecycle_log(&project, &format!("exception {}", "x".repeat(700)));
        }
        let dir = runtime_dir(&project);
        for name in [LIFECYCLE_LOG.to_string(), format!("{LIFECYCLE_LOG}.1")] {
            let size = std::fs::metadata(dir.join(name)).unwrap().len();
            assert!(size <= LIFECYCLE_LOG_MAX_BYTES, "log grew to {size}");
        }
        assert!(dir.join(format!("{LIFECYCLE_LOG}.2")).is_file());
        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn daemon_stops_quietly_after_idle_timeout() {
        let _serial = DAEMON_TEST_LOCK.lock().unwrap();
        let project = std::env::temp_dir().join(format!(
            "ct-indexwatch-idle-{}-{}",
            std::process::id(),
            indexing::unix_millis()
        ));
        let root = project.join("kb");
        std::fs::create_dir_all(&root).unwrap();
        let plan = Plan {
            project: project.clone(),
            scopes: vec![crate::indexing::Scope {
                root,
                provider: crate::indexing::PROVIDER_OKF.to_string(),
                include: vec!["**/*.md".to_string()],
                exclude: vec![],
                origin: crate::indexing::Origin::Derived,
            }],
            exclude: vec![],
            watch: true,
            debounce_ms: 25,
            audit_seconds: 60,
            idle_seconds: 1,
            max_file_bytes: 1024 * 1024,
            system_memory_bytes: 8 * 1024 * 1024 * 1024,
            daemon_memory_limit_bytes: 2 * 1024 * 1024 * 1024,
            daemon_memory_limit_automatic: true,
            config_path: project.join(".ct/index.jsonc"),
        };
        let started = Instant::now();
        run_daemon_logged(&project, plan).unwrap();
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert!(started.elapsed() < Duration::from_secs(5));
        let log = std::fs::read_to_string(runtime_dir(&project).join(LIFECYCLE_LOG)).unwrap();
        assert!(log.contains("stop reason=idle"), "{log}");
        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn daemon_self_terminates_above_memory_limit() {
        let _serial = DAEMON_TEST_LOCK.lock().unwrap();
        let project = std::env::temp_dir().join(format!(
            "ct-indexwatch-memory-{}-{}",
            std::process::id(),
            indexing::unix_millis()
        ));
        let root = project.join("kb");
        std::fs::create_dir_all(&root).unwrap();
        let plan = Plan {
            project: project.clone(),
            scopes: vec![crate::indexing::Scope {
                root,
                provider: crate::indexing::PROVIDER_OKF.to_string(),
                include: vec!["**/*.md".to_string()],
                exclude: vec![],
                origin: crate::indexing::Origin::Derived,
            }],
            exclude: vec![],
            watch: true,
            debounce_ms: 25,
            audit_seconds: 60,
            idle_seconds: 60,
            max_file_bytes: 1024 * 1024,
            system_memory_bytes: 8 * 1024 * 1024 * 1024,
            daemon_memory_limit_bytes: 1,
            daemon_memory_limit_automatic: false,
            config_path: project.join(".ct/index.jsonc"),
        };
        run_daemon_logged(&project, plan).unwrap();
        let status = read_status(&project).unwrap();
        assert_eq!(status.lane, "unavailable");
        assert!(status.memory_rss_bytes > status.memory_limit_bytes);
        let log = std::fs::read_to_string(runtime_dir(&project).join(LIFECYCLE_LOG)).unwrap();
        assert!(log.contains("stop reason=memory-limit"), "{log}");
        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn shutdown_flag_marks_exit_and_releases_singleton() {
        let _serial = DAEMON_TEST_LOCK.lock().unwrap();
        let project = std::env::temp_dir().join(format!(
            "ct-indexwatch-signal-{}-{}",
            std::process::id(),
            indexing::unix_millis()
        ));
        let plan = Plan {
            project: project.clone(),
            scopes: vec![],
            exclude: vec![],
            watch: true,
            debounce_ms: 25,
            audit_seconds: 60,
            idle_seconds: 60,
            max_file_bytes: 1024 * 1024,
            system_memory_bytes: 8 * 1024 * 1024 * 1024,
            daemon_memory_limit_bytes: 2 * 1024 * 1024 * 1024,
            daemon_memory_limit_automatic: true,
            config_path: project.join(".ct/index.jsonc"),
        };
        SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
        run_daemon_logged(&project, plan).unwrap();
        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
        assert_eq!(read_status(&project).unwrap().lane, "unavailable");
        assert!(!daemon_running(&project).unwrap());
        let log = std::fs::read_to_string(runtime_dir(&project).join(LIFECYCLE_LOG)).unwrap();
        assert!(log.contains("stop reason=signal"), "{log}");
        let _ = std::fs::remove_dir_all(project);
    }
}
