//! The newest database migration this binary embeds.
//!
//! HS-004 replaces this constant with the highest embedded migration number; the
//! type stays an integer so `version --json` keeps its shape.

pub const SCHEMA_VERSION: u32 = 0;
