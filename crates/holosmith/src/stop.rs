//! The one way a session's container is stopped (HS-030, spec item 6).
//!
//! Three policies stop a container — the create's eviction (HS-020), the idle stop (HS-021) and the
//! abort ladder's second rung (HS-016) — and every one of them must record the same fact first: **this
//! stop was ours**. Spec item 6 makes that flag the whole of the exit classification's first case
//! ("Exit with the flag set means `stopped`"), and a classification that read a flag nobody wrote would
//! call every policy stop a crash.
//!
//! So the write and the call live together, in one routine, for the same reason the abort ladder lives in
//! one routine: three copies of "set the flag, then stop" is two copies a test cannot see, and the one
//! that is forgotten is the one that turns an eviction into an `error` row.
//!
//! **Why the flag is the server's and not the backend's.** A backend may emit frames but may not write a
//! row (ADR-0019), and the flag is a row's column ([`crate::sessions::repository`]). The caller therefore
//! records it and hands the backend a stop that has already been announced — which is also the order
//! every caller already uses for the *state*: the row moves before the container does (ADR-0020).
//!
//! A flag write that fails is logged and does not stop the stop. The policy asked for a container to be
//! stopped, and a row write that failed is not a reason to keep a runaway session alive; the price is a
//! misclassified exit, and the log line is what an operator has to go on.

use std::sync::{Arc, Mutex, PoisonError};

use crate::backend::{BackendError, Backends, SessionBackend};
use crate::db::Database;
use crate::sessions::Session;

/// Records that a stop was expected, then asks the backend to stop the container.
///
/// `Clone` for the same reason [`crate::provisioner::Provisioner`] is: a policy hands one to a task of
/// its own, because a stop must not make an answer wait (spec item 7).
#[derive(Debug, Clone)]
pub struct Stopper {
    backend: Backends,
    database: Arc<Mutex<Database>>,
}

impl Stopper {
    pub fn new(backend: Backends, database: Arc<Mutex<Database>>) -> Self {
        Self { backend, database }
    }

    /// Stop `session`'s container, having first recorded that this stop was the server's.
    ///
    /// The write comes **before** the call, and that order is the story's first acceptance bullet: an
    /// exit that arrives while the stop is in flight must already find the flag set. The lock is taken
    /// and released in its own scope, so it is never held across the backend's `.await` (the guard is not
    /// `Send`, and the compiler enforces it — ADR-0012/ADR-0013).
    ///
    /// A backend that answers `Err` still has the flag set: the container may be gone or wedged, and the
    /// exit that follows either way is one we asked for. Only a *row* failure is logged here — the
    /// backend's failure is the caller's to report, exactly as it was before this routine existed.
    pub async fn stop(&self, session: &Session) -> Result<(), BackendError> {
        self.record(session);
        self.backend.stop(session).await
    }

    /// The flag write, on its own so the synchronous scope is visible: no `await` inside it, and no
    /// early return, because the stop happens whether or not the row could be told about it.
    fn record(&self, session: &Session) {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(error) = database.sessions().set_expected_stop(&session.id, true) {
            tracing::warn!(
                target: "holosmith::stop",
                session = %session.id,
                %error,
                "the expected-stop flag was not written; the stop goes ahead"
            );
        }
    }
}
