//! The control listener's HTTP surface: the route table, the health endpoint, the error
//! envelope, the session's event stream, and the layer every request passes through (ADR-0007,
//! ADR-0008).

pub mod app;
mod changes;
mod disk;
pub mod error;
mod forward;
pub mod guard;
pub mod health;
mod history;
// `pub(crate)` since HS-072: `holosmith hub host` is a **client** of these three routes, and a client that
// re-declared `HostList` would be a second spelling of one wire shape.
pub(crate) mod hosts;
mod merge;
mod publish;
mod push;
mod sessions;
mod streams;
mod tool_image;
mod tool_output;

pub use app::{
    App, AppState, ManifestEntry, RequestCounter, RouteKind, Routes, is_guard_exempt, router,
};
pub use guard::Guards;
pub use streams::KEEP_ALIVE_INTERVAL;

/// The delete flow's second half, named for its one caller outside this module (HS-064).
///
/// The forced-session expiry's daily job runs the *same* removal an operator's `DELETE` does — that is the
/// whole of item 15's "using the delete flow from item 9" — and this is the one name it may reach, rather than
/// the module being opened to the crate. The alternative (a second implementation beside the policy) is the
/// thing the story exists to prevent: the keep-rules, the log's removal and the streams' closing would be
/// written twice.
pub(crate) use sessions::complete_deletion;
