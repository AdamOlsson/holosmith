//! Which sessions have an event stream open, and how to end one (HS-010, ADR-0014).
//!
//! Two later stories read this: HS-019 calls [`StreamRegistry::close`] when it deletes a session,
//! and HS-066 asks whether a session has a stream open before it pushes a notification. Neither
//! needs to know anything about SSE, so the registry holds no stream body and no connection —
//! only a count per session and one close signal per open stream. [`StreamRegistry::count`] is
//! the whole read surface: a "which sessions are open" list has no caller yet.
//!
//! The count is released by [`StreamGuard`]'s `Drop`, never by a line at the end of a handler:
//! the handler's future can be dropped mid-stream when the client disconnects or the shutdown
//! deadline cuts it (ADR-0007), and neither path runs code after the `await`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

/// The registry an `AppState` carries. Cheap to clone — one `Arc` — because the state is cloned
/// per request.
#[derive(Debug, Clone, Default)]
pub struct StreamRegistry {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    sessions: Mutex<HashMap<String, Session>>,
    /// Handed out one per registration, so a guard can find and remove its own closer and the
    /// list cannot grow with streams that already ended.
    next_id: AtomicU64,
}

/// The streams open on one session.
#[derive(Debug, Default)]
struct Session {
    open: usize,
    closers: Vec<(u64, oneshot::Sender<()>)>,
}

impl StreamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one open stream for `session`. The count drops when the returned guard is
    /// dropped, whichever way the stream ends.
    pub fn open(&self, session: &str) -> StreamGuard {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (closed, signal) = oneshot::channel();
        let mut sessions = self.entries();
        let entry = sessions.entry(session.to_string()).or_default();
        entry.open += 1;
        entry.closers.push((id, closed));
        StreamGuard {
            registry: self.clone(),
            session: session.to_string(),
            id,
            closed: signal,
        }
    }

    /// How many streams are open on `session`. What HS-066 reads.
    pub fn count(&self, session: &str) -> usize {
        self.entries().get(session).map_or(0, |entry| entry.open)
    }

    /// End every stream open on `session` and answer how many were asked to close.
    ///
    /// Called by the delete flow (HS-019). The count itself is released by each guard on the way
    /// out, so a second call, or a call on a session with nothing open, is a no-op rather than
    /// an error.
    pub fn close(&self, session: &str) -> usize {
        let mut sessions = self.entries();
        let Some(entry) = sessions.get_mut(session) else {
            return 0;
        };
        let closers = std::mem::take(&mut entry.closers);
        let count = closers.len();
        for (_, closer) in closers {
            // `Err` means that stream ended on its own between the take and this send; its guard
            // has already dropped, so there is nothing left to close.
            let _ = closer.send(());
        }
        count
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<String, Session>> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// One stream ended: forget its closer and, when it was the last one, the session.
    fn release(&self, session: &str, id: u64) {
        let mut sessions = self.entries();
        let Some(entry) = sessions.get_mut(session) else {
            return;
        };
        entry.closers.retain(|(closer, _)| *closer != id);
        entry.open = entry.open.saturating_sub(1);
        if entry.open == 0 {
            sessions.remove(session);
        }
    }
}

/// One open stream's registration. Dropping it is the only way the count goes back down.
#[derive(Debug)]
pub struct StreamGuard {
    registry: StreamRegistry,
    session: String,
    id: u64,
    closed: oneshot::Receiver<()>,
}

impl StreamGuard {
    /// Resolves when [`StreamRegistry::close`] ended this stream, so the body can end with it.
    ///
    /// Also resolves if the registry drops first, which happens only at process shutdown.
    pub async fn closed(&mut self) {
        let _ = (&mut self.closed).await;
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.registry.release(&self.session, self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_counts_until_it_drops() {
        let registry = StreamRegistry::new();
        let first = registry.open("a");
        assert_eq!(registry.count("a"), 1);
        assert_eq!(registry.count("b"), 0);

        let second = registry.open("a");
        assert_eq!(registry.count("a"), 2, "two streams are two subscribers");

        drop(first);
        assert_eq!(registry.count("a"), 1, "one stream ended");
        drop(second);
        assert_eq!(registry.count("a"), 0, "the last stream ended");
    }

    #[tokio::test]
    async fn closing_a_session_signals_every_open_stream_once() {
        let registry = StreamRegistry::new();
        let mut first = registry.open("a");
        let mut second = registry.open("a");
        // Another session must not be closed by it.
        let mut other = registry.open("b");

        assert_eq!(registry.close("a"), 2);
        for (label, guard) in [("first", &mut first), ("second", &mut second)] {
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(1), guard.closed())
                    .await
                    .is_ok(),
                "the {label} stream was not told to close"
            );
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), other.closed())
                .await
                .is_err(),
            "closing one session closed another's stream"
        );

        // The count is the guards' to release, so a close does not fake it.
        assert_eq!(registry.count("a"), 2);
        assert_eq!(
            registry.close("a"),
            0,
            "a second close has nothing to close"
        );
    }
}
