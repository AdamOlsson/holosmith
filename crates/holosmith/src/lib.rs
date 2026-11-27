//! The Holosmith binary: hub, server, proxies, container helpers and deploy tool.

/// The version this binary reports: `/api/health`'s `version`, `holosmith version`, and the tag on every image this
/// build names.
///
/// **`VERSION` at the repository root is the only copy of it.** `build.rs` reads that file and puts it here, so a
/// bump is one line and one commit — and, because the value is in no manifest, it is in no `Cargo.lock` either:
/// `scripts/check-crate-set.sh` digests that lock, and a version living in it would make every bump read as a stale
/// agent image. Nothing in this crate may read Cargo's `CARGO_PKG_VERSION`, which answers the placeholder that
/// `Cargo.toml`'s `[workspace.package]` carries; `tests/version_single_source.rs` is what says so.
pub const VERSION: &str = env!("HOLOSMITH_VERSION");

pub mod agent;
pub mod backend;
pub mod backup;
pub mod builder;
pub mod candidate;
pub mod changes;
pub mod cli;
pub mod clock;
pub mod commands;
pub mod compactor;
pub mod config;
pub mod container;
pub mod db;
pub mod deleter;
pub mod deploy;
pub mod disk;
pub mod docker;
pub mod doctor;
pub mod drain;
pub mod egress;
pub mod events;
pub mod exit;
pub mod expiry;
pub mod fake_pi;
pub mod forward;
pub mod hosts;
pub mod http;
pub mod identity;
pub mod imagebuild;
pub mod images;
pub mod key;
pub mod keyproxy;
pub mod ladder;
pub mod logging;
pub mod logs;
pub mod mcpproxy;
pub mod merge;
pub mod pass;
pub mod pins;
pub mod preflight;
pub mod preview;
pub mod progress;
pub mod projectimage;
pub mod projects;
pub mod promote;
pub mod provisioner;
pub mod proxies;
pub mod publish;
pub mod push;
pub mod quota;
pub mod reconcile;
pub mod relay;
pub mod release;
pub mod remote;
pub mod rollback;
pub mod rpc;
pub mod runtime;
pub mod schema;
pub mod selftest;
pub mod serve;
pub mod sessions;
pub mod smoke;
pub mod snapshot;
mod sql;
pub mod status;
pub mod stop;
pub mod sweeper;
pub mod tool;
pub mod trust;
pub mod tunnel;
pub mod turn;
pub mod uninstall;
pub mod update;
pub mod vapid;
pub mod version;
pub mod web;
pub mod worktree;
