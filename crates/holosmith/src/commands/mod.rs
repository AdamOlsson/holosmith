//! One module per command group. `main` only parses the command line and dispatches.

pub mod config;
pub mod container;
pub mod deploy;
pub mod doctor;
pub mod key;
pub mod proxy;
pub mod serve;
pub mod version;
