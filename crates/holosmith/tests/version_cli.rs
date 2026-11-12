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
fn version_prints_five_key_value_lines_and_exits_zero() {
    let stdout = stdout_of(&holosmith(&["version"]));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 5, "expected five lines, got {stdout:?}");
    assert_eq!(lines[0], format!("version {}", env!("CARGO_PKG_VERSION")));
    assert!(
        lines[1]
            .strip_prefix("git_tag ")
            .is_some_and(|tag| !tag.is_empty()),
        "expected a non-empty git_tag line, got {:?}",
        lines[1]
    );
    // The fifth fact is the **release directory the running file is in** (HS-080), and this binary is in
    // `target/*/holosmith`, not under `releases/`: the line is always printed, and `-` is that answer.
    assert_eq!(
        lines[2], "release -",
        "a test binary is not in a release directory: {stdout:?}"
    );
    assert_eq!(lines[3], format!("pi_version {}", pin_file().trim()));
    assert_eq!(
        lines[4],
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
            "release",
            "schema_version",
            "sqlite",
            "target",
            "version"
        ]
    );
    assert_eq!(
        object["release"],
        serde_json::Value::Null,
        "a binary outside a release directory reports no release, and the key is still there"
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
fn json_schema_version_is_the_highest_embedded_migration() {
    // Guard 2 of ADR-0009, from the outside: the constant the binary reports without opening a
    // database is the highest migration it embeds, and it writes nothing on the way.
    let dir = std::env::temp_dir().join(format!("holosmith-version-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp directory is creatable");
    let output = Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args(["version", "--json"])
        .current_dir(&dir)
        .output()
        .expect("the binary runs");
    let stdout = stdout_of(&output);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is valid JSON");
    let highest = holosmith::schema::embedded()
        .expect("the embedded migrations are well formed")
        .last()
        .map(|migration| migration.number)
        .unwrap_or_default();
    assert_eq!(value["schema_version"], serde_json::json!(highest));
    let left: Vec<String> = fs::read_dir(&dir)
        .expect("the directory is readable")
        .map(|entry| {
            entry
                .expect("a readable entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    let _ = fs::remove_dir_all(&dir);
    assert!(
        left.is_empty(),
        "version opened or created something: {left:?}"
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
