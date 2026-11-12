//! The disk floor's request-path half (spec item 14): `507 disk_floor` for the two requests that *start*
//! work.
//!
//! Item 14's first sentence — "`min_free_gb` (default 20): session creation and resume fail with 507 below it" —
//! has three readers and one measurement. The measurement is [`crate::disk`]; the boot's reading of it is
//! [`crate::preflight`]'s, which `doctor` prints; and this module is the request paths'. It lives in a file of its
//! own, beside `changes`, `merge` and `publish`, for the reason those do — and for one more: the source-level
//! guard in `disk.rs` asserts that `disk::measure` is named outside that module only in `preflight.rs` and here,
//! so "running turns are not interrupted by crossing the floor" is a fact about the tree. `http/sessions.rs` is
//! *not* exempt: it holds `prompt`, `abort` and `steer` as well as the two starts, and a disk read added to a turn
//! is exactly what such a guard should catch (ADR-0068).
//!
//! The sentence is [`crate::http::error::ApiError::disk_floor`]'s, not this module's: there is one refusal for
//! both requests because item 14 gives them one fix, and the words belong beside every other code's.

use std::path::PathBuf;

use crate::http::AppState;
use crate::http::error::{ApiError, internal};

/// The floor, as a refusal: `None` when this run may start work, or is not measured at all.
///
/// Both callers ask this before they change anything — the create before its project's refusals, the resume
/// before the project's read and the cap — so a full machine is answered as a *machine* fact, no `git` process
/// runs, and nothing is created to clean up (ADR-0060's "a machine fact before a session fact", ADR-0068 D1).
///
/// Four ways to answer `None`, and each is a decision:
///
/// - **This run provisions no containers** ([`crate::disk::guarded`]): a stub session starts no container and
///   fills no filesystem, which is `preflight::check`'s own rule one level down.
/// - **`min_free_gb` is `0`**, which means *no floor*: there is nothing to compare against, so nothing is
///   measured — and a machine with no `df` is not refused a session over a floor it does not have.
/// - The measurement was made and the filesystem is at or above the floor.
///
/// A measurement that **could not be made** is not a 507 — there is no number to report and "free some space"
/// would be a lie about a machine nobody measured — so it is the envelope's 500, logged with its reason. On this
/// path that means the machine changed under a server that passed the same check at boot.
pub(crate) async fn refusal(state: &AppState) -> Option<ApiError> {
    if !crate::disk::guarded(state.runtime.backend, state.config.role)
        || state.config.min_free_gb == 0
    {
        return None;
    }
    let df: PathBuf = state.tools.df.clone();
    let root = state.config.data_root.clone();
    // On a blocking task for the reason every subprocess on a request path is: this forks `df`, and a create's
    // request path must not park a runtime worker on one.
    let measured = tokio::task::spawn_blocking(move || crate::disk::measure(&df, &root)).await;
    let available_gb = match measured {
        Ok(Ok(available_gb)) => available_gb,
        Ok(Err(error)) => return Some(internal("disk", error)),
        Err(error) => return Some(internal("disk", error)),
    };
    match crate::disk::verdict(available_gb, state.config.min_free_gb) {
        crate::disk::Verdict::Enough => None,
        crate::disk::Verdict::Short => Some(ApiError::disk_floor(
            available_gb,
            state.config.min_free_gb,
            &state.config.data_root,
        )),
    }
}
