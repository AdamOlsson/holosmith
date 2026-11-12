//! The push request and the TLS child that delivers it (ADR-0071).
//!
//! Two facts live here, and both are shapes rather than policies:
//!
//! - **The request head**, byte for byte ([`request`]): a `POST` of an already-sealed body with the six headers a
//!   push service needs. One unit test pins the whole vector, because the cheapest way to break a push is to
//!   misspell a header a service silently ignores.
//! - **The disposition table** ([`Disposition`]): what a service's status means for the row. `200`/`201`/`202`
//!   is delivered, `404`/`410` is **gone** and the row is removed (spec item 11's "dead endpoints are removed"),
//!   and everything else — including `401`, `403`, `429` and every `5xx` — keeps the row and is a `warn`.
//!
//! **Why `401`/`403` must not delete a subscription.** They are *key* problems: a rotated VAPID pair, a `sub` the
//! service dislikes, a clock that drifted past `exp`. The endpoint is fine, the browser is still subscribed, and
//! deleting the row would mean the operator's phone silently stops receiving notifications because this machine
//! rotated a key. Only a service saying the endpoint itself is gone earns a removal.
//!
//! The transport is one `openssl s_client` child per subscription, and it is deliberately **not** `curl`: `curl` is
//! not a dependency of this build, `openssl` is (the deploy generates the pair with it and `keyproxy` speaks TLS
//! through it). `-verify_return_error` and `-verify_hostname` make the child fail closed, and the trust store is
//! the process's own — `SSL_CERT_FILE` when the caller set one, never a `-CAfile` flag, so a machine with a
//! private CA configures `openssl` the way it configures every other tool (ADR-0071).
//!
//! **The endpoint's URL is never logged.** A push endpoint is a capability URL: anybody holding it can send to
//! that browser. Every line here names the **host** and nothing else.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::push::Deliver;

/// How long a push service may hold the message before it gives up: one hour.
///
/// A constant, not a config key (ADR-0071's D10): a notification is about a turn that just happened, and one that
/// arrives an hour late is about a session the operator has already looked at. RFC 8030's `TTL` is seconds.
pub const TTL: u32 = 3600;

/// The `Content-Encoding` this build sends, and RFC 8291's own name for it.
pub const ENCODING: &str = "aes128gcm";

/// How long one delivery may take before the child is killed and the row kept.
///
/// A constant, and the build's own rule that **everything on a background path is bounded** (the key proxy's first
/// byte is 120 s, the egress proxy's head 10 s, the image build its own). Sixty is item 12's hub timeout for the two
/// requests that wait for a remote, and a push service that needs longer than that is a service that is not going to
/// take this message. The failure is a `Kept` row and a `warn`: an endpoint that goes quiet is a machine problem,
/// not proof that the browser is gone, which is `Disposition`'s rule one step out.
pub const DELIVER_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the wait above asks whether the child has finished.
///
/// Fifty milliseconds: a push service normally answers in tens of them, so an ordinary delivery pays one poll, and a
/// hung one costs twenty wakeups a second for a minute rather than a blocked thread.
const POLL: Duration = Duration::from_millis(50);

/// The request head and body for one message, as bytes to write to the child's stdin.
///
/// The order is the vector a test pins, and `Content-Length` is the **sealed** body's length, since the body is
/// already encrypted when this is called.
pub fn request(
    endpoint: &str,
    jwt: &str,
    public_key: &str,
    body: &[u8],
) -> Result<Vec<u8>, String> {
    let target = Target::of(endpoint)?;
    let head = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/octet-stream\r\n\
         Content-Length: {length}\r\n\
         TTL: {ttl}\r\n\
         Content-Encoding: {ENCODING}\r\n\
         Authorization: vapid t={jwt}, k={public_key}\r\n\
         Connection: close\r\n\
         \r\n",
        path = target.path,
        host = target.host_header,
        length = body.len(),
        ttl = TTL,
    );
    let mut request = Vec::with_capacity(head.len() + body.len());
    request.extend_from_slice(head.as_bytes());
    request.extend_from_slice(body);
    Ok(request)
}

/// The status code a push service answered: the first line's second field, or `None` when the answer is not an
/// HTTP response at all.
///
/// Scans for the first line beginning `HTTP/` rather than reading line one, because an `s_client` that prints a
/// session line before the response would otherwise be read as a failed delivery.
pub fn status_of(head: &[u8]) -> Option<u16> {
    let text = String::from_utf8_lossy(head);
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("HTTP/1.1") && line.split_whitespace().next() != Some("HTTP/1.0") {
            continue;
        }
        return fields.next().and_then(|code| code.parse().ok());
    }
    None
}

/// What one delivery means for the subscription's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The service took the message.
    Delivered,
    /// The endpoint is dead — `404` or `410` — so the row is removed.
    Gone,
    /// Anything else: the row stays and the answer is logged. A `401`/`403` is a key problem, not a dead endpoint.
    Kept,
}

impl Disposition {
    /// The table, in one place.
    pub fn of(status: u16) -> Self {
        match status {
            200..=202 => Self::Delivered,
            404 | 410 => Self::Gone,
            _ => Self::Kept,
        }
    }
}

/// The production [`Deliver`]: one `openssl s_client` child per message, on the process's own trust store.
#[derive(Debug, Clone)]
pub struct SslClient {
    /// The program, from [`crate::preflight::Tools::openssl`] on a machine.
    pub openssl: String,
    /// `SSL_CERT_FILE` for the child, when the machine configured one. Never a `-CAfile` argument: a private CA is
    /// configured for `openssl` once, and this must read that configuration rather than a second copy of it.
    pub ca_file: Option<PathBuf>,
}

impl Deliver for SslClient {
    fn deliver(&self, endpoint: &str, request: &[u8], dir: &Path) -> Result<Disposition, String> {
        let target = Target::of(endpoint)?;
        // The child's two streams go to **files** in the caller's own `0700` directory rather than to pipes this
        // thread reads: a pipe has to be drained to EOF, and EOF is exactly what an unresponsive service never
        // sends. With files, the wait is `try_wait` against one deadline — the shape `docker::bounded` and the image
        // build already use — so a service that accepts the connection and then goes quiet costs one
        // [`DELIVER_TIMEOUT`], not the rest of the process's notifications.
        let head_path = dir.join("response");
        let tail_path = dir.join("stderr");
        let head_file = std::fs::File::create(&head_path)
            .map_err(|error| format!("{} could not be created ({error})", head_path.display()))?;
        let tail_file = std::fs::File::create(&tail_path)
            .map_err(|error| format!("{} could not be created ({error})", tail_path.display()))?;
        let mut command = Command::new(&self.openssl);
        command
            .args([
                "s_client",
                // `-quiet` is load-bearing: without it `s_client` prints the session and certificate chatter on
                // stdout, and the first line of the answer would be a certificate's subject.
                "-quiet",
                "-connect",
                &target.connect,
                "-servername",
                &target.host,
                "-verify_return_error",
                "-verify_hostname",
                &target.host,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::from(head_file))
            .stderr(Stdio::from(tail_file));
        if let Some(ca_file) = &self.ca_file {
            command.env("SSL_CERT_FILE", ca_file);
        }
        let mut child = Kill::of(command.spawn().map_err(|error| {
            format!(
                "the TLS client could not be started for {} ({error})",
                target.host
            )
        })?);
        let Some(mut stdin) = child.0.stdin.take() else {
            return Err(format!("the TLS client for {} has no stdin", target.host));
        };
        // A request is a few hundred octets — far inside a pipe's buffer — so this write cannot block; the wait that
        // needs a bound is the one below. The handle is **held**, not dropped: closing `s_client`'s stdin makes it
        // send its close-notify before the response arrives (`crate::keyproxy::forward` measured that), so
        // `Connection: close` in the request head is what ends the exchange.
        stdin.write_all(request).map_err(|error| {
            format!(
                "the TLS client for {} did not accept the request ({error})",
                target.host
            )
        })?;
        let deadline = Instant::now() + DELIVER_TIMEOUT;
        loop {
            match child.0.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {}
                Err(error) => {
                    return Err(format!(
                        "the TLS client for {} could not be waited for ({error})",
                        target.host
                    ));
                }
            }
            if Instant::now() >= deadline {
                // The kill is `Kill`'s on the way out; this is the answer the caller gets, and it keeps the row:
                // an endpoint that goes quiet is a *machine* problem, not proof that the browser is gone.
                return Err(format!(
                    "{} did not answer within {} s, so the message was dropped and the subscription kept",
                    target.host,
                    DELIVER_TIMEOUT.as_secs()
                ));
            }
            std::thread::sleep(POLL);
        }
        let head = std::fs::read(&head_path).unwrap_or_default();
        let tail = std::fs::read(&tail_path).unwrap_or_default();
        if !tail.is_empty() {
            tracing::debug!(
                target: "holosmith::push",
                host = %target.host,
                tail = %String::from_utf8_lossy(&tail).trim(),
                "the TLS client's stderr"
            );
        }
        let status = status_of(&head).ok_or_else(|| {
            format!(
                "{} answered something that is not an HTTP response: {}",
                target.host,
                String::from_utf8_lossy(&tail).trim()
            )
        })?;
        Ok(Disposition::of(status))
    }
}

/// A child that is killed when this value goes out of scope, whatever path the code above took.
///
/// The `kill_on_drop` guard the brief names, written here because it is `tokio::process::Command`'s flag and this
/// sender is a **blocking** call (it runs inside `spawn_blocking`, and a `Deliver` a test implements is ordinary
/// blocking code). Without it, a `?` on any line above would leave `s_client` holding a TLS connection until the
/// process ended.
struct Kill(std::process::Child);

impl Kill {
    fn of(child: std::process::Child) -> Self {
        Self(child)
    }
}

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// An endpoint split into the four parts a request and a child need: nothing is logged from here but `host`.
pub(crate) struct Target {
    /// The host alone: `Host` without its port, and what `-servername`/`-verify_hostname` compare.
    pub(crate) host: String,
    /// `host:port` for `-connect`, with an IPv6 literal kept in brackets.
    pub(crate) connect: String,
    /// The authority as it goes on the wire, port included when the endpoint named one.
    pub(crate) host_header: String,
    /// The request target: the path with its query, or `/`.
    pub(crate) path: String,
}

impl Target {
    /// Split `endpoint`. A refusal names the endpoint's shape, never the endpoint.
    pub(crate) fn of(endpoint: &str) -> Result<Self, String> {
        let rest = endpoint
            .strip_prefix("https://")
            .ok_or_else(|| "a push endpoint is an https URL, and this one is not".to_string())?;
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..end];
        if authority.is_empty() {
            return Err("the push endpoint has no host".to_string());
        }
        let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
            let Some((host, after)) = rest.split_once(']') else {
                return Err("the push endpoint's IPv6 host is not closed".to_string());
            };
            let port = match after.strip_prefix(':') {
                Some(port) => port.to_string(),
                None if after.is_empty() => "443".to_string(),
                None => return Err("the push endpoint's host has trailing text".to_string()),
            };
            (host.to_string(), port)
        } else {
            match authority.split_once(':') {
                Some((host, port)) => (host.to_string(), port.to_string()),
                None => (authority.to_string(), "443".to_string()),
            }
        };
        if host.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("the push endpoint's host or port is not one".to_string());
        }
        let path = &rest[end..];
        let path = if path.is_empty() {
            "/".to_string()
        } else if path.starts_with('?') {
            format!("/{path}")
        } else {
            path.to_string()
        };
        let connect = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        Ok(Self {
            host,
            connect,
            host_header: authority.to_string(),
            path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token and a key, so the vector below reads as the request it is.
    const JWT: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9.eyJhdWQiOiJ4In0.c2ln";
    const KEY: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";

    #[test]
    fn the_request_head_is_the_vector_byte_for_byte() {
        let body = [0xaa, 0xbb, 0xcc];
        let request = request(
            "https://push.example.com:8443/one/two?three=four",
            JWT,
            KEY,
            &body,
        )
        .expect("the endpoint splits");
        let mut wanted = String::from(
            "POST /one/two?three=four HTTP/1.1\r\n\
             Host: push.example.com:8443\r\n\
             Content-Type: application/octet-stream\r\n\
             Content-Length: 3\r\n\
             TTL: 3600\r\n\
             Content-Encoding: aes128gcm\r\n\
             Authorization: vapid t=eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9.eyJhdWQiOiJ4In0.c2ln, k=BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8\r\n\
             Connection: close\r\n\
             \r\n",
        );
        let head_length = wanted.len();
        wanted.push_str("\u{0}\u{0}\u{0}");
        // The head is text and the body is bytes, so the vector is compared in two pieces: the head exactly, and
        // the body after it.
        assert_eq!(&request[..head_length], &wanted.as_bytes()[..head_length]);
        assert_eq!(&request[head_length..], &body);
    }

    #[test]
    fn an_endpoint_splits_into_the_four_parts_a_child_needs() {
        let target = Target::of("https://push.example.com/one?two").expect("a plain endpoint");
        assert_eq!(target.host, "push.example.com");
        assert_eq!(target.connect, "push.example.com:443");
        assert_eq!(target.host_header, "push.example.com");
        assert_eq!(target.path, "/one?two");

        let target = Target::of("https://push.example.com:8443").expect("no path");
        assert_eq!(target.connect, "push.example.com:8443");
        assert_eq!(target.path, "/");

        let target = Target::of("https://push.example.com?a=b").expect("a query with no path");
        assert_eq!(target.path, "/?a=b");

        let target = Target::of("https://[::1]:8443/x").expect("an IPv6 literal");
        assert_eq!(target.host, "::1");
        assert_eq!(target.connect, "[::1]:8443");
        assert_eq!(target.host_header, "[::1]:8443");

        for refused in ["http://push.example.com/x", "https:///x", "https://:8443/x"] {
            assert!(Target::of(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn the_disposition_table_is_four_arms_and_one_removal() {
        // The rule production runs is `Gone` or not, so that is the rule asserted — not a helper one step away
        // from it (the review of 's neighbour made the same point about a test that agreed with its code).
        for status in [200, 201, 202] {
            assert_eq!(Disposition::of(status), Disposition::Delivered, "{status}");
            assert!(
                !matches!(Disposition::of(status), Disposition::Gone),
                "{status}"
            );
        }
        for status in [404, 410] {
            assert_eq!(Disposition::of(status), Disposition::Gone, "{status}");
        }
        // Everything else keeps the row — including the two that are a *key* problem, not a dead endpoint.
        for status in [204, 301, 400, 401, 403, 413, 429, 500, 503] {
            assert_eq!(Disposition::of(status), Disposition::Kept, "{status}");
            assert!(
                !matches!(Disposition::of(status), Disposition::Gone),
                "{status}"
            );
        }
    }

    #[test]
    fn a_status_is_read_from_the_first_http_line() {
        assert_eq!(
            status_of(b"HTTP/1.1 201 Created\r\nLocation: x\r\n\r\n"),
            Some(201)
        );
        assert_eq!(status_of(b"HTTP/1.1 410 Gone\n\n"), Some(410));
        // An `s_client` line before the response is skipped rather than read as the answer.
        assert_eq!(
            status_of(b"depth=0 CN = push.example\r\nHTTP/1.1 403 Forbidden\r\n\r\n"),
            Some(403)
        );
        assert_eq!(status_of(b""), None);
        assert_eq!(status_of(b"not an answer\r\n"), None);
    }
}
