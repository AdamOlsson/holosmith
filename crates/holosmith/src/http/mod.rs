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
// `pub(crate)`: `holosmith hub host` is a **client** of these three routes, and a client that
// re-declared `HostList` would be a second spelling of one wire shape.
pub(crate) mod hosts;
mod merge;
mod publish;
mod push;
mod sessions;
mod streams;
mod tool_image;
mod tool_output;
// `pub`: [`AppState::updater`] names the handle the two `/api/update` routes that start and read the job are answered
// from, so the type is part of what a caller of the state can see — the same reason `error` and `health` are open.
pub mod update;

pub use app::{
    App, AppState, ManifestEntry, RequestCounter, RouteKind, Routes, is_guard_exempt, router,
};
pub use guard::Guards;
pub use streams::KEEP_ALIVE_INTERVAL;

/// The delete flow's second half, named for its one caller outside this module.
///
/// The forced-session expiry's daily job runs the *same* removal an operator's `DELETE` does — that is the
/// whole of item 15's "using the delete flow from item 9" — and this is the one name it may reach, rather than
/// the module being opened to the crate. The alternative (a second implementation beside the policy) is the
/// thing the story exists to prevent: the keep-rules, the log's removal and the streams' closing would be
/// written twice.
pub(crate) use sessions::complete_deletion;

/// A project's refusal, as the create answers it, named for its one caller outside this module.
///
/// `holosmith project add` prints the same code and sentence for a clone it refused with one of the three shapes
/// a repository is refused for, so a project added by the command and a project named by a create fail in one set
/// of words rather than two that drift. The fourth answer, [`crate::worktree::Refusal::Unreadable`], has no code
/// on either side: this mapping logs git's words and answers ADR-0008's sentence, and the command prints git's
/// words to the terminal the clone ran in.
pub(crate) use sessions::refusal_error;
