//! `last_active_at`: the one column two runners and one handler all move.
//!
//! Activity is not a state change — it is what HS-020's eviction order and HS-021's idle stop read —
//! so it is moved through the repository's `touch` and never through the transition table. Three
//! callers do it (the turn runner at the prompt and at the settle, and the steer handler), and they
//! share one function so the rule "log it and carry on" cannot drift between them: a row's activity is
//! a hint for eviction, and losing it is never a reason to fail a request or strand a session.
//!
//! It lives here rather than in the repository because the repository owns the statement and this owns
//! the *lock* and the decision that a failure is not fatal (ADR-0012/ADR-0013: the guard is taken for
//! one synchronous call and never across an `.await`).

use std::sync::{Arc, Mutex, PoisonError};

use crate::clock;
use crate::db::Database;

/// Record that `session_id` was active now, logging a failure rather than returning it.
///
/// Synchronous on purpose: a caller that is a background task awaits elsewhere, and a caller that is a
/// handler has already awaited everything it needs by the time it gets here.
pub(crate) fn touch(database: &Arc<Mutex<Database>>, session_id: &str) {
    let database = database.lock().unwrap_or_else(PoisonError::into_inner);
    if let Err(error) = database.sessions().touch(session_id, clock::unix_now()) {
        tracing::warn!(
            target: "holosmith::sessions",
            session = session_id,
            %error,
            "last_active_at did not move"
        );
    }
}
