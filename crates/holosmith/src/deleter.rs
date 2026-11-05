//! The delete flow's background half: what happens after the row is `deleting` (HS-019).
//!
//! `DELETE /api/sessions/{id}` does everything a client must be able to *see* the result of —
//! ending a running turn, taking the WIP snapshot, applying the transition — and then hands the
//! session to this task and answers 202. Removing what the backend owns, the key and the row is
//! off the request's task for the same reason provisioning is: a `docker rm` must never be
//! something a tap waits for.
//!
//! Four steps, in this order, and each one is a decision:
//!
//! 1. **Revoke the session's dummy key.** First, because a key that spends money should stop
//!    working the moment the operator asked [(`revoke_key`)](revoke_key).
//! 2. **Remove what the backend owns** — [`SessionBackend::delete`], which is the container, the
//!    worktree, the session directory and the network in one call. A backend that *cannot* delete
//!    has created nothing to remove, so the flow carries on; any other failure means resources may
//!    still be there, so the row stays `deleting` and a repeat request retries this task.
//! 3. **Remove the row and its event log**, in one transaction: the row is what names the
//!    resources, so nothing before it may claim the session is gone.
//! 4. **Close the session's streams.** Last, so that a backend whose teardown outlives the writer's
//!    25 ms tick still lets a client already watching see `deleting` before the stream ends; a client
//!    that reconnects afterwards opens a stream for a row that is gone and is answered `not_found`.
//!    (With the stub, and with today's Docker backend, the removal finishes inside that tick, so the
//!    frame is usually dropped by the writer rather than delivered — the ordering is for the backend
//!    that takes real time, not for these.)
//!
//! This is not a state machine: the row is `deleting` before this task exists, and the table has no
//! way out of `deleting` — the only exit is the removal, which is step 3.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::task::JoinHandle;

use crate::backend::{BackendError, Backends, SessionBackend};
use crate::db::Database;
use crate::events::StreamRegistry;
use crate::sessions::Session;

/// The reason the ladder records when a delete had to force-stop a running turn.
///
/// It reaches the row's `status_reason` for the moment between rung 2 and the removal — long enough
/// for the `session.status` event that reports it, which is why it is a word rather than nothing.
pub const DELETED: &str = "deleted";

/// The delete flow's handles: what removes the resources, where the row is, and who to close.
///
/// `Clone` for the same reason [`crate::provisioner::Provisioner`] is: the handler builds one per
/// accepted delete and hands it to a task, and the three fields are a backend value, one `Arc` and
/// a cheap registry handle.
#[derive(Debug, Clone)]
pub struct Deleter {
    backend: Backends,
    database: Arc<Mutex<Database>>,
    streams: StreamRegistry,
    /// `<data>/secrets/keys.toml`: the session's dummy key is removed here before the backend tears anything
    /// down (HS-039). A key that spends money must stop working the moment the operator asked.
    secrets: PathBuf,
}

impl Deleter {
    pub fn new(
        backend: Backends,
        database: Arc<Mutex<Database>>,
        streams: StreamRegistry,
        secrets: PathBuf,
    ) -> Self {
        Self {
            backend,
            database,
            streams,
            secrets,
        }
    }

    /// Hand `session` to a background task and return immediately.
    pub fn spawn(self, session: Session) -> JoinHandle<()> {
        tokio::spawn(async move { self.remove(&session).await })
    }

    /// Remove `session`, which the caller has already moved to `deleting`.
    ///
    /// Public for the reason the other runners' `run` is: a test can await the removal instead of
    /// racing a spawned task.
    ///
    /// Every ending closes the streams, because a session in `deleting` is nobody's to watch any
    /// more. A failure leaves the row as it is, with the message in the log and the operator's next
    /// `DELETE` as the retry.
    pub async fn remove(&self, session: &Session) {
        revoke_key(session, &self.secrets);
        // No expected-stop flag here, deliberately: the *backend* is what makes a delete's exit quiet, by
        // forgetting the launch before it removes the container (HS-031, ADR-0039). The flag would say
        // "the server asked for this stop", which the row is about to stop existing for.
        match self.backend.delete(session).await {
            Ok(()) => {}
            // A backend that cannot delete has never built anything for this session: it refuses
            // provisioning too (the one thing `BackendError::Unsupported` is for, ADR-0015). So the
            // removal is what the client was promised, and the row goes.
            Err(error @ BackendError::Unsupported { .. }) => tracing::warn!(
                target: "holosmith::deleter",
                session = %session.id,
                %error,
                "the backend cannot remove a session, and has nothing to remove"
            ),
            // A failure here means the resources may still exist: removing the row would hide them,
            // and the operator can retry the delete, which runs this task again.
            Err(error) => {
                tracing::error!(
                    target: "holosmith::deleter",
                    session = %session.id,
                    %error,
                    "the backend did not remove the session; it is left deleting"
                );
                self.close(&session.id);
                return;
            }
        }
        match self.remove_row(&session.id) {
            Ok(true) => tracing::info!(
                target: "holosmith::deleter",
                session = %session.id,
                "session removed"
            ),
            Ok(false) => tracing::warn!(
                target: "holosmith::deleter",
                session = %session.id,
                "the session was already gone"
            ),
            Err(error) => tracing::error!(
                target: "holosmith::deleter",
                session = %session.id,
                %error,
                "the session's row could not be removed; it is left deleting"
            ),
        }
        self.close(&session.id);
    }

    /// The one transaction: the log and then the row, through the repository that owns each.
    fn remove_row(&self, session_id: &str) -> Result<bool, crate::sessions::RepoError> {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        database.sessions().remove(session_id)
    }

    /// End every stream open on the session, which is the last thing the client sees of it.
    fn close(&self, session_id: &str) {
        let closed = self.streams.close(session_id);
        tracing::debug!(
            target: "holosmith::deleter",
            session = session_id,
            closed,
            "closed the session's streams"
        );
    }
}

/// Revoke the dummy API key this session spends with (spec item 9, item 14's risk table: "key
/// revoked on delete").
///
/// **A named no-op, deliberately.** No session has a key yet: HS-039 issues one per session and
/// wires the revocation here. The call site is the story's, because "revoked on delete" is a promise
/// that has to have somewhere to land — a later story that found no hook would have to invent the
/// ordering as well as the mechanism.
fn revoke_key(session: &Session, secrets: &std::path::Path) {
    let store = crate::key::Store::new(secrets);
    match store.revoke(&session.id) {
        Ok(true) => tracing::info!(
            target: "holosmith::deleter",
            session = %session.id,
            "the session's dummy key was revoked"
        ),
        Ok(false) => tracing::debug!(
            target: "holosmith::deleter",
            session = %session.id,
            "the session had no dummy key to revoke"
        ),
        Err(error) => tracing::warn!(
            target: "holosmith::deleter",
            session = %session.id,
            %error,
            "the session's dummy key could not be revoked; the key file is the operator's to repair"
        ),
    }
}
