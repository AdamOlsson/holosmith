//! Moving bytes between two sockets, and reading one request head: the mechanism this binary's
//! byte-level listeners share.
//!
//! Three listeners in this binary are byte tunnels rather than apps — the egress proxy (HS-037), the key
//! proxy (HS-039) and the preview listener (HS-041) — because what they carry is not theirs to understand:
//! a `CONNECT` tunnel is opaque TLS, a forwarded provider call is a request whose body must reach the wire
//! byte for byte (ADR-0050), and a preview carries whatever a dev server emits, chunked framing, an SSE
//! body or a WebSocket handshake. Each of them decides *policy* in its own module; this one moves *bytes*.
//!
//! The split is why [`crate::egress`] and [`crate::preview`] are testable without a network, a daemon or a
//! browser: the rules take values, and the tokio half only moves bytes through a decision it was given.
//!
//! The bounds are the caller's, deliberately: a `CONNECT` head is two lines and a preview's is a browser's
//! request, and a mechanism that chose the cap for both would be a policy in the wrong module.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Which listener is moving the bytes: its name is the tracing target of [`pump_both`]'s two `debug` lines.
///
/// An enum rather than a `&'static str` because `tracing`'s `target:` must be a literal — and one target for
/// every tunnel would attribute a preview's broken connection to the egress proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tunnel {
    Egress,
    Preview,
}

impl Tunnel {
    /// One direction of this tunnel stopped, as a `debug` line under this listener's target.
    fn stopped(self, direction: &'static str, error: &std::io::Error) {
        match self {
            Self::Egress => tracing::debug!(
                target: "holosmith::egress",
                direction,
                %error,
                "the tunnel's direction stopped"
            ),
            Self::Preview => tracing::debug!(
                target: "holosmith::preview",
                direction,
                %error,
                "the tunnel's direction stopped"
            ),
        }
    }
}

/// Read up to the blank line, bounded by `max` bytes and `within`.
///
/// Returns what was read as text — including the blank line, which is where a caller's parser stops — or
/// `None` for a head that never arrived, arrived too long, or was not UTF-8. A head is ASCII in practice.
///
/// **The result can carry more than the head**: the read is in chunks, so a body can arrive with it. A caller
/// that forwards bytes must split at the blank line (`crate::preview::split_head`) rather than treat the whole
/// value as headers; a caller that only *decides* (`crate::egress`) stops parsing at the blank line and never
/// notices.
pub async fn read_head(stream: &mut TcpStream, max: usize, within: Duration) -> Option<String> {
    let mut buffer = Vec::with_capacity(512);
    let read = tokio::time::timeout(within, async {
        loop {
            let mut chunk = [0u8; 512];
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n")
                || buffer.windows(2).any(|window| window == b"\n\n")
            {
                return Some(());
            }
            if buffer.len() >= max {
                return None;
            }
        }
    })
    .await;
    match read {
        Ok(Some(())) => String::from_utf8(buffer).ok(),
        _ => None,
    }
}

/// Copy in both directions until both are done, counting bytes **as they move**.
///
/// `tokio::io::copy_bidirectional` is the tidier call and it is what [`crate::egress`] used to be, but it
/// answers `(0, 0)` when either direction fails, which is the one case an operator reads the line for. Two
/// `copy`-shaped loops over a split connection count each chunk before the next read, so an error costs the
/// chunk that failed and nothing else. A `shutdown` on the far side is how the other half learns the stream
/// ended. [`Tunnel`] decides which listener's tracing target the two `debug` lines carry, because a target
/// has to be a literal for the macro and an egress failure under a preview's name would be a lie.
pub async fn pump_both(client: TcpStream, upstream: TcpStream, tunnel: Tunnel) -> (u64, u64) {
    use std::sync::atomic::{AtomicU64, Ordering};

    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);
    let up = AtomicU64::new(0);
    let down = AtomicU64::new(0);
    let (sent, received) = tokio::join!(
        pump(&mut client_read, &mut upstream_write, &up),
        pump(&mut upstream_read, &mut client_write, &down),
    );
    if let Err(error) = sent {
        tunnel.stopped("upload", &error);
    }
    if let Err(error) = received {
        tunnel.stopped("download", &error);
    }
    (up.load(Ordering::Relaxed), down.load(Ordering::Relaxed))
}

/// One direction of a tunnel, counting every byte that reached the other side.
pub async fn pump<R, W>(
    from: &mut R,
    to: &mut W,
    counted: &std::sync::atomic::AtomicU64,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use std::sync::atomic::Ordering;

    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let read = match from.read(&mut buffer).await {
            Ok(0) => {
                let _ = to.shutdown().await;
                return Ok(());
            }
            Ok(read) => read,
            Err(error) => return Err(error),
        };
        to.write_all(&buffer[..read]).await?;
        counted.fetch_add(read as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    const MAX: usize = 8 * 1024;
    const WITHIN: Duration = Duration::from_secs(5);

    /// A pair of sockets one loopback connection makes, so a test can write one side and read the other.
    async fn pair() -> (TcpStream, TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is bindable");
        let address = listener.local_addr().expect("a bound address");
        let client = TcpStream::connect(address)
            .await
            .expect("the loopback connection is made");
        let (server, _) = listener.accept().await.expect("the connection arrives");
        (client, server)
    }

    #[tokio::test]
    async fn a_head_is_everything_up_to_the_blank_line() {
        let (mut client, mut server) = pair().await;
        client
            .write_all(b"GET /x HTTP/1.1\r\nHost: a\r\n\r\nbody")
            .await
            .expect("the loopback write works");
        let head = read_head(&mut server, MAX, WITHIN).await.expect("a head");
        // The body was in the same chunk, so it comes back too: the blank line is where a caller's parser
        // stops, and `crate::preview::split_head` is what keeps the two apart when bytes are forwarded.
        assert_eq!(head, "GET /x HTTP/1.1\r\nHost: a\r\n\r\nbody");
    }

    #[tokio::test]
    async fn a_head_over_the_cap_is_none() {
        let (mut client, mut server) = pair().await;
        let written = tokio::spawn(async move {
            let _ = client.write_all(&vec![b'a'; MAX + 64]).await;
            client
        });
        assert!(
            read_head(&mut server, MAX, WITHIN).await.is_none(),
            "a head that never ends must not be returned"
        );
        drop(written.await);
    }

    #[tokio::test]
    async fn a_closed_connection_is_none() {
        let (client, mut server) = pair().await;
        drop(client);
        assert!(read_head(&mut server, MAX, WITHIN).await.is_none());
    }

    #[tokio::test]
    async fn a_pump_counts_every_byte_it_moved() {
        // A slice is a reader and a `Vec` is a writer, so one direction of a tunnel is a loop with no socket in
        // it at all — and the count is what an operator reads the log line for, so it is asserted against the
        // bytes that arrived rather than against the copy's own answer.
        let mut from: &[u8] = b"seven!!";
        let mut to: Vec<u8> = Vec::new();
        let counted = AtomicU64::new(0);
        pump(&mut from, &mut to, &counted)
            .await
            .expect("a slice has an end, so the loop ends");
        assert_eq!(to, b"seven!!");
        assert_eq!(counted.load(std::sync::atomic::Ordering::Relaxed), 7);
    }
}
