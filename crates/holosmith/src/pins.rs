//! The one place the pinned pi version lives: `pins/pi.txt`, embedded at compile time.

/// `pins/pi.txt` exactly as stored, including the newline a text file ends with.
const PI_FILE: &str = include_str!("../../../pins/pi.txt");

/// The pinned pi version, as `version` and later `doctor` report it.
pub fn pi_version() -> &'static str {
    PI_FILE.trim()
}
