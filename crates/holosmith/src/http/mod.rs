//! The control listener's HTTP surface: the route table, the health endpoint, the error
//! envelope, the session's event stream, and the layer every request passes through (ADR-0007,
//! ADR-0008).

pub mod app;
pub mod error;
pub mod guard;
pub mod health;
mod history;
mod sessions;
mod streams;

pub use app::{
    App, AppState, ManifestEntry, RequestCounter, RouteKind, Routes, is_guard_exempt, router,
};
pub use guard::Guards;
pub use streams::KEEP_ALIVE_INTERVAL;
