//! The one place the pinned values live: `pins/pi.txt` and `pins/rust.txt`, embedded at compile time.

/// `pins/pi.txt` exactly as stored, including the newline a text file ends with.
const PI_FILE: &str = include_str!("../../../pins/pi.txt");

/// `pins/rust.txt` exactly as stored, including the newline a text file ends with.
const RUST_FILE: &str = include_str!("../../../pins/rust.txt");

/// The pinned pi version, as `version` and later `doctor` report it.
pub fn pi_version() -> &'static str {
    PI_FILE.trim()
}

/// The pinned rust base's **whole reference** — the tag and the digest in one value — which every recipe with a rust
/// stage takes as `HOLOSMITH_RUST_IMAGE` (ADR-0118).
///
/// The whole reference rather than the tag alone, because Docker resolves `rust:tag@sha256:…` by the **digest** and
/// ignores the tag: a pin holding only a version could build from one image while claiming another, and nothing would
/// notice. One value in one file cannot drift from itself, and a bump is that one edit.
pub fn rust_image() -> &'static str {
    RUST_FILE.trim()
}
