//! `config show`: the resolved config as TOML, driven by the same key table as the walk.
//!
//! Unset optionals are a comment, never an omission: TOML has no null, and a key that
//! disappears is the silent fallback this story exists to prevent (ADR-0006).

use std::fmt;

use serde_json::Value;

use super::schema::KNOWN_KEYS;
use super::{Config, LoadedConfig};

/// Why the resolved config could not be written as TOML.
///
/// The one value `Config` holds that TOML cannot spell is a path that is not UTF-8, and no
/// config file can produce one (TOML is UTF-8). the `--data-dir` takes a `PathBuf`, so
/// the day it exists the answer has to be an error and not a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    NotUtf8Path,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a path in the config is not UTF-8, which TOML cannot carry")
    }
}

/// The resolved config as TOML, one key per line, flat keys first.
pub fn render(config: &Config) -> Result<String, RenderError> {
    let resolved = serde_json::to_value(config).map_err(|_| RenderError::NotUtf8Path)?;
    let mut out = String::new();
    let mut section: Option<&str> = None;
    for spec in KNOWN_KEYS {
        if spec.section != section {
            if let Some(name) = spec.section {
                out.push('\n');
                out.push_str(&format!("[{name}]\n"));
            }
            section = spec.section;
        }
        let key = field(spec.path);
        match lookup(&resolved, spec.path).and_then(toml_value) {
            Some(text) => out.push_str(&format!("{key} = {text}\n")),
            None => out.push_str(&format!("# {key} unset\n")),
        }
    }
    Ok(out)
}

/// `config show`: the path and the source, then the resolved config. The comments keep the
/// output valid TOML, so a `config show` body re-reads as the same value.
pub fn render_show(loaded: &LoadedConfig) -> Result<String, RenderError> {
    Ok(format!(
        "# config_path: {}\n# config_source: {}\n{}",
        escaped(&loaded.path.display().to_string()),
        loaded.source.as_str(),
        render(&loaded.config)?
    ))
}

fn field(path: &str) -> &str {
    match path.rsplit_once('.') {
        Some((_, field)) => field,
        None => path,
    }
}

/// The resolved value at a dotted path, if it is in `value`.
fn lookup<'value>(value: &'value Value, path: &str) -> Option<&'value Value> {
    path.split('.')
        .try_fold(value, |value, part| value.get(part))
}

/// The TOML text for one resolved value. `None` is a key with no value: unset, or a shape
/// this renderer cannot spell, which the agreement test refuses to let exist.
fn toml_value(value: &Value) -> Option<String> {
    Some(match value {
        Value::String(text) => quoted(text),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Array(items) => {
            let items: Option<Vec<String>> = items.iter().map(toml_value).collect();
            format!("[{}]", items?.join(", "))
        }
        Value::Object(_) | Value::Null => return None,
    })
}

/// A basic TOML string: quotes, backslashes and the control characters TOML escapes.
fn quoted(text: &str) -> String {
    format!("\"{}\"", escaped(text))
}

/// `text` with every character that could end the value or the comment line it sits in
/// written as an escape.
///
/// A TOML comment ends at a newline and may not hold control characters, so a `--config`
/// path with a newline in it must not be pasted into the header unescaped: the output would
/// stop being TOML.
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                out.push_str(&format!("\\u{:04X}", u32::from(character)));
            }
            character => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::config::validate_text;

    fn rendered(config: &Config) -> String {
        render(config).expect("a config read from a file renders")
    }

    #[test]
    fn the_defaults_render_byte_identically_to_the_golden_file() {
        assert_eq!(
            rendered(&Config::default()),
            include_str!("../../tests/golden/config-default.toml")
        );
    }

    #[test]
    fn show_puts_the_path_and_the_source_above_the_config() {
        let loaded = LoadedConfig::defaults(Path::new("/etc/holosmith/config.toml"));
        let shown = render_show(&loaded).expect("a UTF-8 path renders");
        assert_eq!(
            shown.lines().take(2).collect::<Vec<_>>(),
            [
                "# config_path: /etc/holosmith/config.toml",
                "# config_source: defaults"
            ]
        );
        assert!(shown.ends_with(&rendered(&Config::default())));
    }

    #[test]
    fn a_path_with_a_newline_in_the_header_keeps_the_output_valid_toml() {
        let loaded = LoadedConfig::defaults(Path::new("/tmp/a\r\nb.toml"));
        let shown = render_show(&loaded).expect("a UTF-8 path renders");
        assert_eq!(
            shown.lines().next(),
            Some("# config_path: /tmp/a\\r\\nb.toml")
        );
        validate_text(Path::new("shown.toml"), &shown)
            .expect("a path with a newline in it must not break the document");
    }

    #[test]
    fn a_path_that_is_not_utf8_is_an_error_and_not_a_panic() {
        let config = Config {
            data_root: PathBuf::from(OsString::from_vec(b"/var/\xff".to_vec())),
            ..Config::default()
        };
        assert_eq!(render(&config), Err(RenderError::NotUtf8Path));
    }

    #[test]
    fn the_rendered_config_reads_back_as_the_same_value() {
        let mut config = Config::default();
        config.role = crate::config::Role::Hub;
        config.max_live_sessions = 5;
        config.owner = Some("me@example.com".to_string());
        config.abort_grace = crate::config::Duration::from_millis(1500);
        config.merge.targets = vec!["develop".to_string()];
        config.egress.allow = vec!["crates.io".to_string()];
        // Every flat key comes before the first table header, or TOML puts it in the table.
        let rendered = rendered(&config);
        let reparsed = validate_text(Path::new("rendered.toml"), &rendered)
            .expect("the rendered config is valid TOML and a valid config");
        assert_eq!(reparsed, config);
    }

    #[test]
    fn every_unset_optional_is_a_comment_and_not_an_omission() {
        let rendered = rendered(&Config::default());
        for key in ["owner", "docker_host", "hosts_seed", "max_session_tokens"] {
            assert!(
                rendered.contains(&format!("# {key} unset\n")),
                "{key} is missing from:\n{rendered}"
            );
        }
    }

    #[test]
    fn a_set_optional_renders_as_a_value() {
        let config = Config {
            max_session_tokens: Some(1000),
            ..Config::default()
        };
        let rendered = rendered(&config);
        assert!(
            rendered.contains("max_session_tokens = 1000\n"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("# max_session_tokens unset"),
            "{rendered}"
        );
    }

    #[test]
    fn strings_are_escaped_so_the_output_stays_valid_toml() {
        assert_eq!(quoted("plain"), "\"plain\"");
        assert_eq!(quoted("a\"b"), "\"a\\\"b\"");
        assert_eq!(quoted("a\\b"), "\"a\\\\b\"");
        assert_eq!(quoted("a\tb"), "\"a\\tb\"");
        assert_eq!(quoted("a\u{7}b"), "\"a\\u0007b\"");
        assert_eq!(quoted("plain\nline"), "\"plain\\nline\"");
    }
}
