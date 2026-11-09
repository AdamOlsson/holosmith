//! The host health probe: one `GET /api/health` out of this machine (HS-068, spec item 4).
//!
//! Item 4 gives the registration three checks and this module is two of them: "the `/api/health` reply must carry
//! the Holosmith name and version fields", and "redirects are not followed". The third — where the URL may point —
//! is [`super::host_problem`], because it is a rule about a string and this is a rule about a conversation.
//!
//! **Two transports, one exchange.** `http` is a plain socket with the shape [`crate::http::health::ask`] already
//! has (connect, one request, `Connection: close`, a deadline, an 8 KiB cap). `https` is one `openssl s_client`
//! child — the same child ADR-0071's push sender runs and for the same reasons: `-quiet` because the session
//! chatter would otherwise be the first line, `-verify_return_error` and `-verify_hostname` so a certificate that
//! does not match is a refusal, and **no `-CAfile`** so the trust store is the process's own `SSL_CERT_FILE` and a
//! machine with a private CA configures the child the way it configures every other tool.
//!
//! **The child's streams are pipes and the wait is a deadline.** An `s_client` that accepts a connection and then
//! goes quiet never closes its stdout, so nothing here may read to EOF before the child has exited: the loop is
//! `try_wait` against [`PROBE_TIMEOUT`], killing the child when the deadline passes, and only then is the output
//! read. The answer is a health payload — a kilobyte at most — far inside a pipe's buffer, so the child cannot block
//! writing it while this thread waits (`push::send` uses files for the same reason: its answer is bounded too, and
//! what it cannot do is *read* a pipe that will not end).
//!
//! **Three outcomes, not two.** `Healthy` is the row's reward; `Unreachable` is "nothing answered" and becomes a row
//! marked unreachable *with the sentence* (item 16's "marked unreachable with the last error, not removed"); and
//! `Refused` is "something answered, and it is not a Holosmith" — item 4's "anything else is rejected with a
//! distinct code". The split is the difference between a machine that is asleep and a machine that is not one of
//! ours, which is the difference between a registration that is stored and one that is not.
//!
//! **A redirect is never followed.** Not because a hop counter would be hard, but because a health check that
//! moves is a health check somebody else answers: the `302` is reported as what it is, and the stand-in that sent
//! it is asked exactly once.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::Origin;
use crate::http::health::{self, Answer};
use crate::preflight::Tools;

/// How long one probe may take — connect, request and answer included.
///
/// Three seconds, the same bound [`health::ASK_TIMEOUT`] puts on a loopback question, because the two ask the same
/// endpoint: this one may be a tailnet's latency away rather than a syscall's, which is the whole distance between
/// them and still not a reason for a registration to hang.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The largest answer a probe reads before giving up on it being a health payload.
const MAX_ANSWER: usize = 8 * 1024;

/// How often the TLS child's wait asks whether it has finished.
const POLL: Duration = Duration::from_millis(20);

/// What one probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The host answered its own health payload: the row's version, and a registration worth storing.
    Healthy {
        /// The `version` the host reported, as it spelled it.
        version: String,
    },
    /// Nothing answered. The sentence is stored as the row's `last_error` (item 16's "not removed").
    Unreachable(String),
    /// Something answered, and it is not a Holosmith health reply. The request is refused.
    Refused(String),
}

/// Ask `origin` for `GET /api/health`.
pub fn ask(origin: &Origin, tools: &Tools) -> Outcome {
    let Some((scheme, authority)) = origin.as_str().split_once("://") else {
        return Outcome::Unreachable(format!("{:?} is not an origin", origin.as_str()));
    };
    let (Some(host), Some(port)) = (origin.host(), origin.port()) else {
        return Outcome::Unreachable(format!(
            "{:?} does not name a host and a port",
            origin.as_str()
        ));
    };
    let request =
        format!("GET /api/health HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    // The host as an address, without the brackets an IPv6 literal travels in.
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    let answer = match scheme {
        "http" => plain(bare, port, request.as_bytes()),
        "https" => tls(bare, port, request.as_bytes(), tools),
        other => Err(format!(
            "{:?} is not an http or https origin, so there is nothing to ask it",
            other
        )),
    };
    match answer {
        Ok(answer) => interpret(&answer),
        Err(sentence) => Outcome::Unreachable(sentence),
    }
}

/// The plain-socket half: resolve, connect, one request, read until the peer closes or the deadline passes.
fn plain(host: &str, port: u16, request: &[u8]) -> Result<Vec<u8>, String> {
    let target = resolve(host, port)?;
    let mut stream = TcpStream::connect_timeout(&target, PROBE_TIMEOUT)
        .map_err(|error| format!("{host}:{port} could not be reached ({error})"))?;
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
    stream
        .write_all(request)
        .map_err(|error| format!("{host}:{port} did not accept the request ({error})"))?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut answer = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        if answer.len() >= MAX_ANSWER {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "{host}:{port} did not finish answering within {} s",
                PROBE_TIMEOUT.as_secs()
            ));
        }
        let _ = stream.set_read_timeout(Some(remaining));
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => answer.extend_from_slice(&buffer[..read]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(format!(
                    "{host}:{port} did not answer within {} s",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Err(error) => {
                return Err(format!("{host}:{port} was not answered ({error})"));
            }
        }
    }
    Ok(answer)
}

/// The TLS half: one `openssl s_client` child, its answer read after it has exited.
fn tls(host: &str, port: u16, request: &[u8], tools: &Tools) -> Result<Vec<u8>, String> {
    let connect = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut command = Command::new(&tools.openssl);
    command
        .args([
            "s_client",
            // Without `-quiet` the session and certificate chatter comes first and the answer would be a
            // certificate's subject (ADR-0071 measured it).
            "-quiet",
            "-connect",
            &connect,
            "-servername",
            host,
            "-verify_return_error",
            "-verify_hostname",
            host,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("the TLS client for {host} could not be started ({error})"))?;
    {
        // The handle is **held**, not dropped: closing `s_client`'s stdin makes it send its close-notify before the
        // response arrives (`keyproxy::forward` measured that), so `Connection: close` in the head is what ends the
        // exchange. A request is a few hundred octets, far inside a pipe's buffer, so this write cannot block — the
        // wait that needs a bound is the one below.
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| format!("the TLS client for {host} has no stdin"))?;
        stdin.write_all(request).map_err(|error| {
            format!("the TLS client for {host} did not accept the request ({error})")
        })?;
    }
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{host}:{port} did not answer within {} s",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the TLS client for {host} could not be waited for ({error})"
                ));
            }
        }
    }
    // The child has exited, so both pipes are at EOF: this read is bounded by the child's own end.
    let output = child
        .wait_with_output()
        .map_err(|error| format!("the TLS client for {host} printed nothing readable ({error})"))?;
    // **The bytes are the answer; the exit code only explains an empty one.** OpenSSL 3 reports a peer that closes
    // without a TLS `close_notify` as `unexpected eof while reading` and exits 1 — after the response has been read
    // and printed. A probe that demanded a zero exit would call a perfectly good answer unreachable, which is exactly
    // what this story's first gate run measured against its own stand-in.
    let said = String::from_utf8_lossy(&output.stderr)
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .unwrap_or_default();
    if output.stdout.is_empty() {
        return Err(format!(
            "the TLS client for {host} failed{}{said}",
            if said.is_empty() { "" } else { ": " }
        ));
    }
    if !output.status.success() {
        // Kept for an operator reading the log rather than the row: the answer arrived, and the session ended badly.
        tracing::debug!(
            target: "holosmith::hosts",
            host = %host,
            tail = %said,
            "the TLS client answered and then ended with an error"
        );
    }
    Ok(output.stdout)
}

/// One address for `host:port`: the resolver's first answer.
///
/// `to_socket_addrs` has no timeout of its own — a resolver that never answers is the platform's problem, not this
/// probe's, and the same is true of the `s_client` child that resolves the other transport.
fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    let mut addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("{host} could not be resolved ({error})"))?;
    addresses
        .next()
        .ok_or_else(|| format!("{host} resolved to no address"))
}

/// What a host's answer means.
///
/// Pure, so every arm is a test: the status line, the payload's shape, and the one key this registration is *about*.
/// A redirect reaches the `Some(302)` arm and is refused there — nothing in this module follows one.
fn interpret(answer: &[u8]) -> Outcome {
    let text = String::from_utf8_lossy(answer);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return Outcome::Refused(
            "answered no complete header block, so it did not answer an HTTP request".to_string(),
        );
    };
    let status = head.lines().next().unwrap_or_default();
    let code = status
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok());
    match code {
        Some(200) => {}
        Some(code) => {
            return Outcome::Refused(format!(
                "answered HTTP {code} ({status:?}), and a health check is one 200: a redirect is not followed and \
                 nothing else is a health reply"
            ));
        }
        None => {
            return Outcome::Refused(format!(
                "answered {status:?}, which is not an HTTP status line"
            ));
        }
    }
    match health::parse_answer(body) {
        Ok(Answer { name, version, .. }) if name == health::NAME => Outcome::Healthy { version },
        Ok(Answer { name, .. }) => Outcome::Refused(format!(
            "answered a health payload named {name:?}, and this build registers only {:?}",
            health::NAME
        )),
        Err(error) => Outcome::Refused(format!(
            "answered a body that is not a Holosmith health payload ({error})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    const PAYLOAD: &str = r#"{"name":"holosmith","version":"0.1.0","role":"server","uptime_s":7,"preview_origin":"http://localhost:8081","application_server_key":null}"#;

    fn answer(status: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn a_health_payload_is_the_rows_version() {
        assert_eq!(
            interpret(&answer("200 OK", PAYLOAD)),
            Outcome::Healthy {
                version: "0.1.0".to_string()
            }
        );
    }

    #[test]
    fn a_redirect_is_refused_and_never_followed() {
        let outcome = interpret(&answer("302 Found", ""));
        match outcome {
            Outcome::Refused(sentence) => {
                assert!(sentence.contains("302"), "{sentence}");
                assert!(sentence.contains("not followed"), "{sentence}");
            }
            other => panic!("a 302 is a refusal: {other:?}"),
        }
        // A 404 and a 500 are the same answer for the same reason: something answered, and not with health.
        for status in ["404 Not Found", "500 Internal Server Error"] {
            assert!(matches!(
                interpret(&answer(status, "")),
                Outcome::Refused(_)
            ));
        }
    }

    #[test]
    fn a_body_that_is_not_the_payload_is_refused() {
        // A payload with no name.
        let nameless = r#"{"version":"0.1.0","role":"server","uptime_s":7}"#;
        assert!(matches!(
            interpret(&answer("200 OK", nameless)),
            Outcome::Refused(_)
        ));
        // A payload named something else.
        let other = PAYLOAD.replace(r#""name":"holosmith""#, r#""name":"something""#);
        match interpret(&answer("200 OK", &other)) {
            Outcome::Refused(sentence) => assert!(sentence.contains("something"), "{sentence}"),
            wrong => panic!("a foreign payload is a refusal: {wrong:?}"),
        }
        // Not JSON at all, and an answer with no body at all.
        assert!(matches!(
            interpret(&answer("200 OK", "<html>hello</html>")),
            Outcome::Refused(_)
        ));
        assert!(matches!(
            interpret(b"HTTP/1.1 200 OK\r\n\r\n"),
            Outcome::Refused(_)
        ));
    }

    #[test]
    fn an_answer_with_no_header_block_is_refused_rather_than_read_as_health() {
        assert!(matches!(interpret(b""), Outcome::Refused(_)));
        assert!(matches!(interpret(b"not http at all"), Outcome::Refused(_)));
        assert!(matches!(
            interpret(b"HTTP/1.1\r\n\r\n"),
            Outcome::Refused(_)
        ));
    }

    /// A listener that answers **one** request with `answer` and then stops, with the port it bound.
    ///
    /// One request and then the listener goes: a probe asks once, and a loop would leave this thread waiting for a
    /// connection nobody makes — which is what `handle.join()` measured the first time this test was written.
    fn stand_in(answer: Vec<u8>) -> (u16, std::thread::JoinHandle<usize>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback listener");
        let port = listener.local_addr().expect("the bound address").port();
        let handle = std::thread::spawn(move || {
            let mut served = 0;
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0_u8; 1024];
                if stream.read(&mut request).unwrap_or(0) > 0 {
                    served += 1;
                    let _ = stream.write_all(&answer);
                }
            }
            served
        });
        (port, handle)
    }

    #[test]
    fn the_plain_transport_really_asks_and_reads() {
        let (port, handle) = stand_in(answer("200 OK", PAYLOAD));
        let origin = Origin::parse(&format!("http://127.0.0.1:{port}")).expect("an origin");
        assert_eq!(
            ask(&origin, &Tools::default()),
            Outcome::Healthy {
                version: "0.1.0".to_string()
            }
        );
        // The stand-in served one request and stopped when the probe closed its end.
        assert!(handle.join().expect("the stand-in finished") >= 1);
    }

    #[test]
    fn a_peer_that_says_nothing_is_unreachable_not_refused() {
        // A port nobody listens on: connection refused, which is item 16's "fails health" and not item 4's refusal.
        let port = {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback listener");
            listener.local_addr().expect("the bound address").port()
        };
        let origin = Origin::parse(&format!("http://127.0.0.1:{port}")).expect("an origin");
        match ask(&origin, &Tools::default()) {
            Outcome::Unreachable(sentence) => {
                assert!(sentence.contains(&port.to_string()), "{sentence}")
            }
            wrong => panic!("a closed port is unreachable: {wrong:?}"),
        }
    }
}
