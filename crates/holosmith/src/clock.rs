//! The one clock the process reads, and the one arithmetic from config minutes to a duration.
//!
//! `now` is passed into the database and the repository rather than read inside them, so a test
//! asserts the value it wrote instead of racing the clock (ADR-0009, ADR-0011). This module is
//! the only place that asks the operating system what time it is: the startup's migration
//! timestamp and a create's `created_at`/`last_active_at`. It also holds [`minutes`],
//! which asks the clock nothing but is the one spelling of "a config key in minutes" the idle stop
//!  and the turn timeout both read, and [`days`], which is the same arithmetic for the one
//! key measured in days.

/// How many seconds a day is, named once: the multiplier [`days`] uses and the one the stopped-session
/// expiry's cutoff is stated in.
pub const SECONDS_PER_DAY: u64 = 86_400;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Unix seconds. A clock set before 1970 reads as 0: a wrong timestamp is not worth failing a
/// start over.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

/// A config key measured in whole minutes, as a duration.
///
/// **Saturating, not wrapping.** The config layer accepts any `u64` for the keys that use this
/// (`idle_stop_min`, `turn_timeout_min`), and a wrapped bound would turn "stop after a week" into "stop
/// immediately" — the opposite of what the operator asked for. Saturating makes an absurd number a bound
/// nothing reaches instead, which is the only failure mode these policies can afford. One spelling, so
/// the sweeper and the turn runner cannot disagree about what a minute is.
pub fn minutes(minutes: u64) -> Duration {
    Duration::from_secs(minutes.saturating_mul(60))
}

/// A config key measured in whole days, as a duration.
///
/// Saturating for [`minutes`]'s reason, one factor further out: `stopped_ttl_days` is a `u64` the config layer
/// accepts without a ceiling, and a wrapped window would turn "keep a stopped session for a week" into
/// "delete every stopped session the moment it stops" — the opposite of what the operator asked for, and the
/// one failure mode a policy that destroys rows cannot afford. Saturating makes an absurd number a window
/// nothing is past instead.
pub fn days(days: u64) -> Duration {
    Duration::from_secs(days.saturating_mul(SECONDS_PER_DAY))
}
