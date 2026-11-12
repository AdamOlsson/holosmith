//! What the binary reports about itself. Later stories reuse these names: `/api/health` takes
//! `version`, `serve` logs `version` and `target` on its startup line, `doctor` compares
//! `pi_version`, and `deploy --ssh` refuses on a `target` mismatch.
//!
//! **One shape, two directions**: `holosmith version --json` writes it, and a promotion reads it back out of
//! a *candidate* binary it has not installed yet (`holosmith upgrade` runs `<binary> version --json`), because a
//! release's version, git tag, pin and schema are facts only that binary can answer. Deserialising the same struct
//! rather than a second reader struct is what keeps the two ends of one line from drifting.

use serde::{Deserialize, Serialize};

use crate::pins;
use crate::schema::SCHEMA_VERSION;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub version: String,
    pub git_tag: String,
    /// The release directory this file was run from, or `None` when it is not in one.
    ///
    /// A **different fact** from `git_tag`: the tag is what the build was made from, and this is the
    /// `releases/<tag>/` directory the running file sits in. They agree on a machine a promotion owns, and a
    /// hand-seeded directory (a copy of any binary) is exactly where they do not — which is why the release tree's
    /// name, not the build's, is the one a switch, `current` and a rollback all speak in.
    ///
    /// `Option` with a **default**, because the other direction of this one line is a promotion: `promote::ask_version`
    /// deserialises a *candidate's* output, and a candidate built before this key existed has no `release` in it. A
    /// missing key is `None`; a required one would make every older binary unpromotable.
    #[serde(default)]
    pub release: Option<String>,
    pub pi_version: String,
    pub schema_version: u32,
    pub target: String,
    pub sqlite: String,
}

impl VersionInfo {
    pub fn current() -> Self {
        let exe = std::env::current_exe().ok();
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_tag: env!("HOLOSMITH_GIT_TAG").to_string(),
            release: exe.as_deref().and_then(crate::release::running_tag),
            pi_version: pins::pi_version().to_string(),
            schema_version: SCHEMA_VERSION,
            target: env!("HOLOSMITH_TARGET").to_string(),
            sqlite: rusqlite::version().to_string(),
        }
    }

    /// The five `key value` lines the CLI prints, one per line, in this order.
    ///
    /// `release` is `-` when the running file is not in a release directory (a build in `dist/`, a test binary): the
    /// line is always there, so a script reads the same shape whether or not this machine has promoted.
    pub fn render_human(&self) -> String {
        format!(
            "version {}\ngit_tag {}\nrelease {}\npi_version {}\nschema_version {}\n",
            self.version,
            self.git_tag,
            self.release.as_deref().unwrap_or("-"),
            self.pi_version,
            self.schema_version
        )
    }

    /// One line of JSON: the same five keys plus `target` and `sqlite`, no newline.
    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> VersionInfo {
        VersionInfo {
            version: "0.1.0".to_string(),
            git_tag: "a30e03f".to_string(),
            release: Some("v0.1.0".to_string()),
            pi_version: pins::pi_version().to_string(),
            schema_version: SCHEMA_VERSION,
            target: "x86_64-unknown-linux-musl".to_string(),
            sqlite: "3.50.2".to_string(),
        }
    }

    #[test]
    fn human_output_is_five_key_value_lines_in_order() {
        assert_eq!(
            sample().render_human(),
            format!(
                "version 0.1.0\ngit_tag a30e03f\nrelease v0.1.0\npi_version {}\nschema_version {SCHEMA_VERSION}\n",
                pins::pi_version()
            )
        );
        // A file that is not in a release directory says so on the same line, so a script never counts lines.
        let loose = VersionInfo {
            release: None,
            ..sample()
        };
        assert!(
            loose.render_human().contains("\nrelease -\n"),
            "{}",
            loose.render_human()
        );
    }

    #[test]
    fn human_output_has_no_trailing_whitespace() {
        let rendered = sample().render_human();
        for line in rendered.lines() {
            assert_eq!(
                line.trim_end(),
                line,
                "line {line:?} has trailing whitespace"
            );
        }
    }

    #[test]
    fn json_output_has_the_contract_keys_in_order() {
        let json = sample()
            .render_json()
            .expect("string and integer fields always serialise");
        assert_eq!(
            json,
            format!(
                r#"{{"version":"0.1.0","git_tag":"a30e03f","release":"v0.1.0","pi_version":"{}","schema_version":{SCHEMA_VERSION},"target":"x86_64-unknown-linux-musl","sqlite":"3.50.2"}}"#,
                pins::pi_version()
            )
        );
        assert!(!json.contains('\n'), "JSON output must be a single line");
    }
}
