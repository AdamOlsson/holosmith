//! What the binary reports about itself. Later stories reuse these names: `/api/health` takes
//! `version`, `serve` logs `version` and `target` on its startup line, `doctor` compares
//! `pi_version`, and `deploy --ssh` refuses on a `target` mismatch.

use serde::Serialize;

use crate::pins;
use crate::schema::SCHEMA_VERSION;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    pub version: String,
    pub git_tag: String,
    pub pi_version: String,
    pub schema_version: u32,
    pub target: String,
    pub sqlite: String,
}

impl VersionInfo {
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_tag: env!("HOLOSMITH_GIT_TAG").to_string(),
            pi_version: pins::pi_version().to_string(),
            schema_version: SCHEMA_VERSION,
            target: env!("HOLOSMITH_TARGET").to_string(),
            sqlite: rusqlite::version().to_string(),
        }
    }

    /// The four `key value` lines the CLI prints, one per line, in this order.
    pub fn render_human(&self) -> String {
        format!(
            "version {}\ngit_tag {}\npi_version {}\nschema_version {}\n",
            self.version, self.git_tag, self.pi_version, self.schema_version
        )
    }

    /// One line of JSON: the same four keys plus `target` and `sqlite`, no newline.
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
            pi_version: pins::pi_version().to_string(),
            schema_version: SCHEMA_VERSION,
            target: "x86_64-unknown-linux-musl".to_string(),
            sqlite: "3.50.2".to_string(),
        }
    }

    #[test]
    fn human_output_is_four_key_value_lines_in_order() {
        assert_eq!(
            sample().render_human(),
            format!(
                "version 0.1.0\ngit_tag a30e03f\npi_version {}\nschema_version {SCHEMA_VERSION}\n",
                pins::pi_version()
            )
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
                r#"{{"version":"0.1.0","git_tag":"a30e03f","pi_version":"{}","schema_version":{SCHEMA_VERSION},"target":"x86_64-unknown-linux-musl","sqlite":"3.50.2"}}"#,
                pins::pi_version()
            )
        );
        assert!(!json.contains('\n'), "JSON output must be a single line");
    }
}
