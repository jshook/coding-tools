// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-await`'s built-in **local-endpoint** probes: "is a local service
//! listening yet?" and "is its health endpoint answering yet?".
//!
//! The single most common agent wait — a service was just started, and the next
//! step must not run until it accepts connections — could not be expressed with
//! the read-only command allowlist, because the honest way to ask is to open a
//! socket, and no allow-listed program does that. Hand-rolled poll loops filled
//! the gap, which is exactly the pattern `ct-await` exists to retire.
//!
//! These probes stay inside the suite's read-only philosophy two ways. They are
//! **observations**: a TCP connect that is immediately closed, or one `GET`.
//! And they are **loopback-only** — [`loopback_addrs`] refuses any host that is
//! not `127.0.0.0/8`, `::1`, or `localhost`, so the wait cannot become a
//! general-purpose network client, and `ct-await` cannot be pointed at a third
//! party's machine.
//!
//! The module also owns the wait's **outcome reasons** ([`expired_reason`],
//! [`deadline_reason`]). They live here, next to the probes, because they exist
//! to distinguish *"the condition never became true"* from *"the probe was too
//! slow to answer"* — a distinction the old single message
//! (`probe run 3 killed`) collapsed, making a slow probe read as a false
//! negative.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::pulse::limit_label;

/// The reason for a wait that ran out of `--timeout` *between* probe runs.
///
/// When `--probe-timeout` was cutting runs short, the count is the diagnosis:
/// the probe never finished inside its own bound, so the wait was never really
/// answered. Saying that is the difference between "the condition never became
/// true" and "the probe is too slow to answer".
///
/// `last` is what the final run reported, carried into the reason so an expiry
/// says what was actually observed rather than only that nothing was: an
/// `--ok-status 503` wait that kept seeing `HTTP 200` is a *wrong expectation*,
/// not an absent service, and the two are indistinguishable without it.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use coding_tools::probe::expired_reason;
///
/// let s = Duration::from_secs;
/// // No per-run bound in play: just the bound and the run count.
/// assert_eq!(expired_reason(s(15), 4, 0, None, ""), "timed out after 15s (4 probe run(s))");
///
/// // What the last run saw, when there was anything to see.
/// let seen = expired_reason(s(2), 4, 0, None, "HTTP 200 from 127.0.0.1:9/h (want 503)");
/// assert!(seen.ends_with("; last probe: HTTP 200 from 127.0.0.1:9/h (want 503)"));
///
/// // Every run cut short: the probe, not the condition, is the problem.
/// let all = expired_reason(s(15), 3, 3, Some(s(1)), "");
/// assert!(all.contains("every run was cut short by --probe-timeout 1s"));
///
/// // Some cut, some not: report the proportion without over-claiming.
/// assert!(expired_reason(s(15), 5, 2, Some(s(1)), "").contains("of which 2 were cut short"));
/// ```
pub fn expired_reason(
    limit: Duration,
    ticks: u64,
    cut_runs: u64,
    per_run: Option<Duration>,
    last: &str,
) -> String {
    let base = format!(
        "timed out after {} ({ticks} probe run(s))",
        limit_label(limit)
    );
    let mut reason = match (cut_runs, per_run) {
        (0, _) | (_, None) => base,
        (n, Some(p)) if n == ticks => format!(
            "{base}; every run was cut short by --probe-timeout {} without finishing — the \
             probe is too slow to answer inside its bound, so narrow it (a smaller --base, a \
             more specific --name) or raise --probe-timeout",
            limit_label(p)
        ),
        (n, Some(p)) => format!(
            "{base}, of which {n} were cut short by --probe-timeout {}",
            limit_label(p)
        ),
    };
    if let Some(tail) = last_line(last) {
        reason.push_str(&format!("; last probe: {tail}"));
    }
    reason
}

/// The last non-empty line of a probe's output, bounded for a one-line reason.
/// A probe that printed nothing (the common `--quiet` case) contributes
/// nothing, rather than an empty `last probe:` clause.
fn last_line(text: &str) -> Option<String> {
    const CAP: usize = 160;
    let line = text.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    Some(if line.chars().count() > CAP {
        format!("{}…", line.chars().take(CAP).collect::<String>())
    } else {
        line.to_string()
    })
}

/// The reason for a wait whose `--timeout` arrived while a probe run was still
/// going.
///
/// The run's own elapsed time is the whole point: `probe run 3 killed` reads as
/// "the condition was false", while `probe run 3 was still running at the
/// deadline (it had run 13s)` reads as "this probe is slower than the wait" —
/// which is the actual fix.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use coding_tools::probe::deadline_reason;
///
/// let r = deadline_reason(Duration::from_secs(15), 3, Duration::from_millis(12_950));
/// assert!(r.starts_with("timed out after 15s: probe run 3 was still running"));
/// assert!(r.contains("it had run 12.95s"));
/// assert!(r.contains("--probe-timeout"));
/// ```
pub fn deadline_reason(limit: Duration, tick: u64, run_took: Duration) -> String {
    format!(
        "timed out after {}: probe run {tick} was still running at the deadline (it had run \
         {}); the probe is slower than the wait — narrow it, raise --timeout, or bound each \
         run with --probe-timeout",
        limit_label(limit),
        // Milliseconds: a run's elapsed time should read as `0.9s`, not
        // `0.903221s`, and never as a bare `0s`.
        limit_label(Duration::from_millis(run_took.as_millis().max(1) as u64)),
    )
}

/// One built-in probe run, shaped like a supervised command's outcome so the
/// wait loop treats argv probes and endpoint probes identically: `detail` is
/// the probe's "output" (what `--ok-match`/`--err-match` search), and
/// `established` is its "exit 0".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointRun {
    /// Whether the condition holds: the port accepted a connection, or the
    /// HTTP response carried the expected status.
    pub established: bool,
    /// A one-line description of what happened, matchable and printable.
    pub detail: String,
}

impl EndpointRun {
    fn yes(detail: impl Into<String>) -> Self {
        Self {
            established: true,
            detail: detail.into(),
        }
    }
    fn no(detail: impl Into<String>) -> Self {
        Self {
            established: false,
            detail: detail.into(),
        }
    }
}

/// The loopback socket addresses to try for `host`, in order, or a refusal.
///
/// `localhost` fans out to both stacks because a service may bind only one of
/// them. Any other host — a name, a LAN address, a public IP — is refused
/// rather than resolved: loopback-only is the guarantee that keeps a bounded
/// wait from becoming an outbound network tool.
///
/// # Examples
///
/// ```
/// use coding_tools::probe::loopback_addrs;
///
/// assert_eq!(loopback_addrs("127.0.0.1", 8080).unwrap().len(), 1);
/// assert_eq!(loopback_addrs("localhost", 80).unwrap().len(), 2); // v4 and v6
/// assert!(loopback_addrs("example.com", 80).is_err());
/// assert!(loopback_addrs("10.0.0.5", 80).is_err());
/// ```
pub fn loopback_addrs(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.eq_ignore_ascii_case("localhost") {
        return Ok(vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        ]);
    }
    match bare.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => Ok(vec![SocketAddr::new(ip, port)]),
        _ => Err(format!(
            "'{host}' is not loopback; ct-await's built-in probes only observe this machine \
             (127.0.0.0/8, ::1, or localhost). Watch a remote service through its local \
             effects instead — a log file, a status file — with an allow-listed probe."
        )),
    }
}

/// Probe whether something is accepting TCP connections on a loopback `port`.
///
/// The connection is opened and dropped; nothing is written, and nothing the
/// service sends is read. A refused connection is the ordinary "not yet" case,
/// not an error.
pub fn probe_tcp(addrs: &[SocketAddr], timeout: Duration) -> EndpointRun {
    let mut last = String::new();
    for addr in addrs {
        match TcpStream::connect_timeout(addr, timeout) {
            Ok(_) => return EndpointRun::yes(format!("connected to {addr}")),
            Err(e) => last = format!("no listener on {addr}: {e}"),
        }
    }
    EndpointRun::no(last)
}

/// A parsed loopback `http://` URL: the addresses to connect to, the `Host`
/// header value, and the request target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpTarget {
    pub addrs: Vec<SocketAddr>,
    pub host_header: String,
    pub path: String,
}

/// Parse an `http://` URL into a loopback [`HttpTarget`], or refuse it.
///
/// Only `http://` is accepted: a loopback health check has nothing to protect
/// in transit, and a self-signed certificate on `127.0.0.1` would fail
/// validation anyway, turning "service is up" into an unrelated TLS error.
///
/// # Examples
///
/// ```
/// use coding_tools::probe::parse_http_url;
///
/// let t = parse_http_url("http://127.0.0.1:8080/health").unwrap();
/// assert_eq!(t.path, "/health");
/// assert_eq!(t.host_header, "127.0.0.1:8080");
///
/// // A bare authority means the site root.
/// assert_eq!(parse_http_url("http://localhost:9000").unwrap().path, "/");
///
/// assert!(parse_http_url("https://127.0.0.1/x").is_err());   // http:// only
/// assert!(parse_http_url("http://example.com/").is_err());   // loopback only
/// ```
pub fn parse_http_url(url: &str) -> Result<HttpTarget, String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        format!("'{url}': --http takes an http:// URL (https is not accepted for a loopback probe)")
    })?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() {
        return Err(format!("'{url}': no host in the URL"));
    }
    // `[::1]:8080` keeps its brackets around the colons of the address itself.
    let (host, port) = if let Some(close) = authority.rfind(']') {
        let h = &authority[..=close];
        match authority[close + 1..].strip_prefix(':') {
            Some(p) => (h, p),
            None => (h, "80"),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, p),
            None => (authority, "80"),
        }
    };
    let port: u16 = port
        .parse()
        .map_err(|_| format!("'{url}': '{port}' is not a port number"))?;
    Ok(HttpTarget {
        addrs: loopback_addrs(host, port)?,
        host_header: authority.to_string(),
        path,
    })
}

/// Probe a loopback HTTP endpoint: one `GET`, read far enough to see the status
/// line, connection closed. `ok_status` is the status that establishes the
/// condition. A connection refusal, a malformed reply, or a different status
/// are all ordinary "not yet" outcomes.
///
/// Hand-written rather than routed through an HTTP client on purpose: the probe
/// must never follow a redirect, reuse a connection, or read an unbounded body
/// — it reads the status line and stops.
pub fn probe_http(target: &HttpTarget, ok_status: u16, timeout: Duration) -> EndpointRun {
    let mut last = String::new();
    for addr in &target.addrs {
        let mut stream = match TcpStream::connect_timeout(addr, timeout) {
            Ok(s) => s,
            Err(e) => {
                last = format!("no listener on {addr}: {e}");
                continue;
            }
        };
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(timeout));
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: ct-await\r\nAccept: */*\r\n\
             Connection: close\r\n\r\n",
            target.path, target.host_header
        );
        if let Err(e) = stream.write_all(req.as_bytes()) {
            last = format!("{addr}: sending the request failed: {e}");
            continue;
        }
        // The status line is the first ~64 bytes; a short read is enough and
        // keeps a chatty or streaming endpoint from filling memory.
        let mut buf = [0u8; 256];
        let mut seen = 0usize;
        let status = loop {
            match stream.read(&mut buf[seen..]) {
                Ok(0) => break None,
                Ok(n) => {
                    seen += n;
                    let text = String::from_utf8_lossy(&buf[..seen]);
                    if let Some(line) = text.split("\r\n").next()
                        && (text.contains("\r\n") || seen == buf.len())
                    {
                        break status_code(line);
                    }
                    if seen == buf.len() {
                        break None;
                    }
                }
                Err(e) => {
                    last = format!("{addr}: reading the response failed: {e}");
                    break None;
                }
            }
        };
        match status {
            Some(code) if code == ok_status => {
                return EndpointRun::yes(format!("HTTP {code} from {}{}", addr, target.path));
            }
            Some(code) => {
                last = format!(
                    "HTTP {code} from {}{} (want {ok_status})",
                    addr, target.path
                );
            }
            None if last.is_empty() => last = format!("{addr}: no HTTP status line in the reply"),
            None => {}
        }
    }
    EndpointRun::no(last)
}

/// The numeric status from an HTTP status line (`HTTP/1.1 200 OK` -> `200`).
///
/// # Examples
///
/// ```
/// use coding_tools::probe::status_code;
///
/// assert_eq!(status_code("HTTP/1.1 204 No Content"), Some(204));
/// assert_eq!(status_code("HTTP/1.0 500"), Some(500));
/// assert_eq!(status_code("garbage"), None);
/// ```
pub fn status_code(line: &str) -> Option<u16> {
    let mut parts = line.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn only_loopback_hosts_resolve() {
        assert!(loopback_addrs("127.0.0.1", 1).is_ok());
        assert!(loopback_addrs("127.5.5.5", 1).is_ok());
        assert!(loopback_addrs("::1", 1).is_ok());
        assert!(loopback_addrs("[::1]", 1).is_ok());
        assert_eq!(loopback_addrs("localhost", 1).unwrap().len(), 2);
        // Everything else is refused with the reason, not silently resolved.
        for host in ["example.com", "10.0.0.5", "0.0.0.0", "192.168.1.1", ""] {
            let e = loopback_addrs(host, 1).unwrap_err();
            assert!(e.contains("loopback"), "{host}: {e}");
        }
    }

    #[test]
    fn http_urls_split_into_addr_host_and_path() {
        let t = parse_http_url("http://127.0.0.1:8080/health").unwrap();
        assert_eq!(t.path, "/health");
        assert_eq!(t.host_header, "127.0.0.1:8080");
        assert_eq!(t.addrs[0].port(), 8080);
        // No path is the site root; no port is 80.
        assert_eq!(parse_http_url("http://127.0.0.1").unwrap().path, "/");
        assert_eq!(
            parse_http_url("http://127.0.0.1").unwrap().addrs[0].port(),
            80
        );
        // A bracketed v6 authority keeps its address intact.
        let v6 = parse_http_url("http://[::1]:9000/x").unwrap();
        assert_eq!(v6.addrs[0].port(), 9000);
        // Refusals name what is wrong.
        assert!(
            parse_http_url("https://127.0.0.1/")
                .unwrap_err()
                .contains("http://")
        );
        assert!(
            parse_http_url("http://example.com/")
                .unwrap_err()
                .contains("loopback")
        );
        assert!(parse_http_url("http://127.0.0.1:notaport/").is_err());
    }

    #[test]
    fn tcp_probe_sees_a_listener_and_its_absence() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let addrs = loopback_addrs("127.0.0.1", port).unwrap();
        let up = probe_tcp(&addrs, Duration::from_secs(2));
        assert!(up.established, "{}", up.detail);
        assert!(up.detail.contains("connected"), "{}", up.detail);

        drop(listener);
        let down = probe_tcp(&addrs, Duration::from_millis(500));
        assert!(!down.established);
        assert!(down.detail.contains("no listener"), "{}", down.detail);
    }

    #[test]
    fn http_probe_reads_the_status_line() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut sock, _)) = listener.accept() else {
                    return;
                };
                let mut sink = [0u8; 512];
                let _ = sock.read(&mut sink);
                let _ = sock.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n");
            }
        });

        let target = parse_http_url(&format!("http://127.0.0.1:{port}/health")).unwrap();
        // The status the service actually returns is not the one we want.
        let miss = probe_http(&target, 200, Duration::from_secs(2));
        assert!(!miss.established);
        assert!(miss.detail.contains("HTTP 503"), "{}", miss.detail);
        // Asking for that status establishes the condition.
        let hit = probe_http(&target, 503, Duration::from_secs(2));
        assert!(hit.established, "{}", hit.detail);
        let _ = server.join();
    }
}
