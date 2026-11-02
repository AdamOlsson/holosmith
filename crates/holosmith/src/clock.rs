//! The one clock the process reads.
//!
//! `now` is passed into the database and the repository rather than read inside them, so a test
//! asserts the value it wrote instead of racing the clock (ADR-0009, ADR-0011). This module is
//! the only place that asks the operating system what time it is: the startup's migration
//! timestamp (HS-004) and a create's `created_at`/`last_active_at` (HS-008).

use std::time::{SystemTime, UNIX_EPOCH};

/// Unix seconds. A clock set before 1970 reads as 0: a wrong timestamp is not worth failing a
/// start over.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}
