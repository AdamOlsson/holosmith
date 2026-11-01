//! The binary's `version` output contract: four human lines, or one line of JSON.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn holosmith(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args(args)
        .output()
        .expect("the test binary path from CARGO_BIN_EXE_holosmith runs")
}

fn stdout_of(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit status {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn pin_file() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../pins/pi.txt");
    fs::read_to_string(&path).expect("pins/pi.txt is readable")
}

#[test]
fn version_prints_four_key_value_lines_and_exits_zero() {
    let stdout = stdout_of(&holosmith(&["version"]));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 4, "expected four lines, got {stdout:?}");
    assert_eq!(lines[0], format!("version {}", env!("CARGO_PKG_VERSION")));
    assert!(
        lines[1]
            .strip_prefix("git_tag ")
            .is_some_and(|tag| !tag.is_empty()),
        "expected a non-empty git_tag line, got {:?}",
        lines[1]
    );
    assert_eq!(lines[2], format!("pi_version {}", pin_file().trim()));
    assert_eq!(
        lines[3],
        format!("schema_version {}", holosmith::schema::SCHEMA_VERSION)
    );
}

#[test]
fn json_prints_one_line_with_the_contract_keys() {
    let stdout = stdout_of(&holosmith(&["version", "--json"]));
    assert_eq!(
        stdout.lines().count(),
        1,
        "expected one line, got {stdout:?}"
    );
    assert!(stdout.ends_with('\n'), "expected a trailing newline");

    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is valid JSON");
    let object = value.as_object().expect("a JSON object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "git_tag",
            "pi_version",
            "schema_version",
            "sqlite",
            "target",
            "version"
        ]
    );
    assert_eq!(
        object["version"],
        serde_json::json!(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(object["pi_version"], serde_json::json!(pin_file().trim()));
    assert_eq!(
        object["schema_version"],
        serde_json::json!(holosmith::schema::SCHEMA_VERSION)
    );
    assert!(
        object["target"].as_str().is_some_and(|t| !t.is_empty()),
        "target must be a non-empty string"
    );
    assert!(
        object["sqlite"]
            .as_str()
            .is_some_and(|v| v.split('.').count() == 3),
        "sqlite must be a version string like 3.50.2"
    );
}

#[test]
fn unknown_subcommand_fails() {
    let output = holosmith(&["nonsense"]);
    assert!(
        !output.status.success(),
        "an unknown command must exit non-zero"
    );
}
