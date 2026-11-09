//! Serving the assembled app, and the deadline that ends it (ADR-0007).
//!
//! Both `SIGTERM` and `SIGINT` stop accepting, let in-flight requests finish, and exit 0. A
//! streaming response has no natural end, so the drain has a deadline: after it, open
//! connections are cut and the process still exits 0. This is process shutdown, not the
//! **Drain** of item 10 (HS-051).

use std::fmt;
use std::future::Future;
use std::io;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::http::App;

/// How long in-flight work may run after the stop signal, in production.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// How long to drain before open connections are cut.
#[derive(Debug, Clone, Copy)]
pub struct ServeOptions {
    pub shutdown_grace: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        }
    }
}

/// Why serving stopped.
#[derive(Debug)]
pub enum ServeError {
    Io(io::Error),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ServeError {}

impl From<io::Error> for ServeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Serve `app` on `listener` until `shutdown` resolves and the drain is over.
///
/// The grace period starts when the signal arrives, not when the process starts, so a long
/// run does not shorten the drain.
pub async fn run<F>(
    listener: TcpListener,
    app: App,
    options: ServeOptions,
    shutdown: F,
) -> Result<(), ServeError>
where
    F: Future<Output = ()> + Send + 'static,
{
    // axum's graceful shutdown future only *starts* the drain; it does not bound it. The
    // deadline therefore lives outside it, in the second `select!` below, which needs to know
    // when the signal arrived.
    let (stop_tx, stop_rx) = oneshot::channel();
    // Served **with connect info** since HS-070: machine-to-machine trust asks the Tailscale daemon
    // who an address is, so the address has to reach the edge layer. `Router::into_make_service`
    // would drop it, and a router built without it is a router whose callers cannot be verified.
    let server = axum::serve(
        listener,
        app.into_router()
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown.await;
        let _ = stop_tx.send(());
    });
    // `WithGracefulShutdown` implements `IntoFuture`, not `Future`.
    let server = server.into_future();
    tokio::pin!(server);

    tokio::select! {
        served = &mut server => served.map_err(ServeError::from),
        stopped = stop_rx => {
            // The sending side is dropped without sending only if the server future ends
            // first, which the branch above already returns.
            let _ = stopped;
            tokio::select! {
                served = &mut server => served.map_err(ServeError::from),
                () = tokio::time::sleep(options.shutdown_grace) => {
                    tracing::warn!(
                        grace_ms = options.shutdown_grace.as_millis(),
                        "drain deadline reached; closing open connections"
                    );
                    Ok(())
                }
            }
        }
    }
}

/// The two signals that ask the process to stop: `SIGTERM` (systemd) and `SIGINT` (a
/// terminal `Ctrl-C`). Installed before the listener binds, so a handler that cannot be
/// installed is a startup failure rather than a process no signal can stop.
pub struct StopSignal {
    terminate: tokio::signal::unix::Signal,
    interrupt: tokio::signal::unix::Signal,
}

impl StopSignal {
    pub fn install() -> Result<Self, ServeError> {
        use tokio::signal::unix::{SignalKind, signal};

        Ok(Self {
            terminate: signal(SignalKind::terminate())?,
            interrupt: signal(SignalKind::interrupt())?,
        })
    }

    /// Resolves when either signal arrives.
    pub async fn recv(mut self) {
        tokio::select! {
            _ = self.terminate.recv() => {}
            _ = self.interrupt.recv() => {}
        }
    }
}
