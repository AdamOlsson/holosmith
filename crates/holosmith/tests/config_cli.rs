//! The `config` subcommands as the operator runs them.
//!
//! Every test drives `CARGO_BIN_EXE_holosmith`, so the exit codes, the stdout/stderr split
//! and the file left on disk are the real ones.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The config path a root names, composed the way the binary composes it: `HOLOSMITH_ROOT`, else the default root.
fn default_config_path() -> String {
    let root = std::env::var_os("HOLOSMITH_ROOT").map_or_else(
        || PathBuf::from(holosmith::config::DEFAULT_ROOT),
        PathBuf::from,
    );
    root.join(holosmith::config::CONFIG_FILE)
        .display()
        .to_string()
}

/// A directory that removes itself, so the tests leave nothing in `/tmp`.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = env::temp_dir().join(format!("holosmith-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temp directory is creatable");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn text(&self) -> &str {
        self.path.to_str().expect("the temp path is UTF-8")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.file(name);
        fs::write(&path, contents).expect("the temp file is writable");
        path
    }

    /// A fake editor: a script that receives the config path as `$1`.
    fn editor(&self, name: &str, body: &str) -> PathBuf {
        let path = self.write(name, &format!("#!/bin/sh\n{body}\n"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("the script is executable");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn holosmith(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args(arguments)
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .output()
        .expect("the test binary path from CARGO_BIN_EXE_holosmith runs")
}

fn holosmith_with_env(arguments: &[&str], variables: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_holosmith"));
    command
        .args(arguments)
        .env_remove("VISUAL")
        .env_remove("EDITOR");
    for (name, value) in variables {
        command.env(name, value);
    }
    command
        .output()
        .expect("the test binary path from CARGO_BIN_EXE_holosmith runs")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A `config show` body: everything after the two header comments.
fn body_of(shown: &str) -> String {
    shown.lines().skip(2).collect::<Vec<_>>().join("\n") + "\n"
}

#[test]
fn validate_accepts_a_clean_file() {
    let directory = TempDir::new("validate-clean");
    let config = directory.write("config.toml", "role = \"hub\"\n");
    let output = holosmith(&[
        "config",
        "validate",
        "--config",
        config.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert_eq!(
        stdout_of(&output),
        format!("config valid: {}\n", config.display())
    );
    assert!(output.stderr.is_empty(), "{}", stderr_of(&output));
}

#[test]
fn validate_reports_every_problem_at_once() {
    let directory = TempDir::new("validate-many");
    let config = directory.write(
        "config.toml",
        "turn_timeout_mni = 45\nmax_live_sessions = 0\n\n[egress]\nalow = \"x\"\n",
    );
    let output = holosmith(&[
        "config",
        "validate",
        "--config",
        config.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "{}", stdout_of(&output));
    assert_eq!(
        stderr_of(&output),
        format!(
            "{path}:1:1: turn_timeout_mni: unknown key\n\
             {path}:2:1: max_live_sessions: must be at least 1\n\
             {path}:5:1: egress.alow: unknown key\n\
             3 problems in {path}\n",
            path = config.display()
        )
    );
}

#[test]
fn validate_refuses_a_missing_file_the_operator_named() {
    let directory = TempDir::new("validate-missing");
    let config = directory.file("config.toml");
    let output = holosmith(&[
        "config",
        "validate",
        "--config",
        config.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        stderr_of(&output),
        format!(
            "holosmith: config validate: no such file: {}\n",
            config.display()
        )
    );
}

#[test]
fn validate_reports_a_path_it_cannot_read() {
    let directory = TempDir::new("validate-directory");
    let output = holosmith(&["config", "validate", "--config", directory.text()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr_of(&output).starts_with(&format!(
            "holosmith: config validate: cannot read {}: ",
            directory.path().display()
        )),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn validate_uses_the_default_path_when_none_is_given() {
    let default = default_config_path();
    // `exists()` is not the question this test asks. It answers `false` for a path under a directory the user may
    // not read — which is what a root the service user owns is, 0700 and `holosmith` (ADR-0086 D1) — so the guard
    // would not fire on the one machine where the run below cannot work. `metadata` separates "this machine has no
    // config" from "this machine's config is not mine to read", and only the first is a case this test can drive.
    match std::fs::metadata(&default) {
        Ok(_) => {
            eprintln!("SKIP: this machine has {default}");
            return;
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            eprintln!("SKIP: {default} cannot be examined from this user ({error})");
            return;
        }
        Err(_) => {}
    }
    let output = holosmith(&["config", "validate"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert_eq!(
        stdout_of(&output),
        format!("config valid: {default} (no file, defaults)\n")
    );
}

#[test]
fn show_prints_every_resolved_default() {
    let directory = TempDir::new("show-defaults");
    let config = directory.write("config.toml", "");
    let output = holosmith(&[
        "config",
        "show",
        "--config",
        config.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert!(output.stderr.is_empty());

    let shown = stdout_of(&output);
    let mut lines = shown.lines();
    assert_eq!(
        lines.next(),
        Some(format!("# config_path: {}", config.display()).as_str())
    );
    assert_eq!(lines.next(), Some("# config_source: file"));

    let body = body_of(&shown);
    // The golden is the independent copy of the key set, compared byte for byte: a key the
    // walk or the renderer drops fails here, so a second hand-kept list of key names on top
    // of it could only ever say "you forgot to edit me too".
    assert_eq!(body, include_str!("golden/config-default.toml"));
}

#[test]
fn show_accepts_the_global_flag_before_the_subcommand() {
    let directory = TempDir::new("show-global-flag");
    let config = directory.write("config.toml", "");
    let output = holosmith(&[
        "--config",
        config.to_str().expect("UTF-8"),
        "config",
        "show",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert_eq!(
        body_of(&stdout_of(&output)),
        include_str!("golden/config-default.toml")
    );
}

#[test]
fn show_prints_nothing_on_stdout_when_the_file_is_broken() {
    let directory = TempDir::new("show-broken");
    let config = directory.write("config.toml", "turn_timeout_mni = 45\n");
    let output = holosmith(&[
        "config",
        "show",
        "--config",
        config.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "{}", stdout_of(&output));
    assert_eq!(
        stderr_of(&output),
        format!(
            "{path}:1:1: turn_timeout_mni: unknown key\n1 problem in {path}\n",
            path = config.display()
        )
    );
}

#[test]
fn show_output_revalidates_clean_and_round_trips() {
    let directory = TempDir::new("show-round-trip");
    let source = directory.write("config.toml", "role = \"hub\"\nmax_live_sessions = 5\n");
    let output = holosmith(&[
        "config",
        "show",
        "--config",
        source.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    let shown = stdout_of(&output);
    assert!(shown.contains("role = \"hub\"\n"), "{shown}");
    assert!(shown.contains("max_live_sessions = 5\n"), "{shown}");

    let round_tripped = directory.write("round-trip.toml", &shown);
    let validated = holosmith(&[
        "config",
        "validate",
        "--config",
        round_tripped.to_str().expect("UTF-8"),
    ]);
    assert_eq!(
        validated.status.code(),
        Some(0),
        "the shown config must re-validate: {}",
        stderr_of(&validated)
    );

    let again = holosmith(&[
        "config",
        "show",
        "--config",
        round_tripped.to_str().expect("UTF-8"),
    ]);
    assert_eq!(again.status.code(), Some(0), "{}", stderr_of(&again));
    assert_eq!(body_of(&stdout_of(&again)), body_of(&shown));
}

#[test]
fn edit_creates_the_file_from_the_resolved_defaults() {
    let directory = TempDir::new("edit-create");
    let config = directory.file("config.toml");
    let editor = directory.editor("editor.sh", "exit 0");
    let output = holosmith_with_env(
        &[
            "config",
            "edit",
            "--config",
            config.to_str().expect("UTF-8"),
        ],
        &[("EDITOR", &editor)],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert_eq!(
        stdout_of(&output),
        format!("config valid: {}\n", config.display())
    );

    let created = fs::read_to_string(&config).expect("the editor was given a file to edit");
    assert!(
        created.starts_with(&format!(
            "# config_path: {}\n# config_source: defaults\n",
            config.display()
        )),
        "{created}"
    );
    let validated = holosmith(&[
        "config",
        "validate",
        "--config",
        config.to_str().expect("UTF-8"),
    ]);
    assert_eq!(
        validated.status.code(),
        Some(0),
        "{}",
        stderr_of(&validated)
    );
}

#[test]
fn edit_leaves_an_invalid_save_byte_identical() {
    let directory = TempDir::new("edit-invalid");
    let config = directory.write("config.toml", "role = \"hub\"\n");
    let editor = directory.editor(
        "editor.sh",
        "printf 'role   =  \"hub\"\\nmax_live_sessions = 0\\n' > \"$1\"",
    );
    let output = holosmith_with_env(
        &[
            "config",
            "edit",
            "--config",
            config.to_str().expect("UTF-8"),
        ],
        &[("EDITOR", &editor)],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "{}", stdout_of(&output));
    assert_eq!(
        stderr_of(&output),
        format!(
            "{path}:2:1: max_live_sessions: must be at least 1\n1 problem in {path}\n",
            path = config.display()
        )
    );
    // The save is reported as it stands: nothing is reformatted, reverted or appended.
    assert_eq!(
        fs::read_to_string(&config).expect("the file is still there"),
        "role   =  \"hub\"\nmax_live_sessions = 0\n"
    );
}

#[test]
fn validate_folds_dot_segments_before_comparing_the_roots() {
    let directory = TempDir::new("validate-roots-folded");
    let nested = directory.write(
        "nested.toml",
        "data_root = \"/var/lib/holosmith\"\nprojects_root = \"/srv/../var/lib/holosmith/projects\"\n",
    );
    let output = holosmith(&[
        "config",
        "validate",
        "--config",
        nested.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr_of(&output),
        format!(
            "{path}:2:1: projects_root: must not be inside data_root: deleting data must \
             never touch a repository\n1 problem in {path}\n",
            path = nested.display()
        )
    );

    let outside = directory.write(
        "outside.toml",
        "projects_root = \"/var/lib/holosmith/../projects\"\n",
    );
    let output = holosmith(&[
        "config",
        "validate",
        "--config",
        outside.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
}

#[test]
fn show_revalidates_when_the_config_path_holds_a_newline() {
    let directory = TempDir::new("show-newline-path");
    let config = directory.write("a\nb.toml", "role = \"hub\"\n");
    let output = holosmith(&[
        "config",
        "show",
        "--config",
        config.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    let shown = stdout_of(&output);
    assert_eq!(
        shown.lines().next(),
        Some(format!("# config_path: {}/a\\nb.toml", directory.path().display()).as_str())
    );

    let round_tripped = directory.write("round-trip.toml", &shown);
    let validated = holosmith(&[
        "config",
        "validate",
        "--config",
        round_tripped.to_str().expect("UTF-8"),
    ]);
    assert_eq!(
        validated.status.code(),
        Some(0),
        "the header must not break the document: {}",
        stderr_of(&validated)
    );
}

#[test]
fn validate_reads_a_section_written_as_an_inline_table() {
    let directory = TempDir::new("validate-inline-table");
    let config = directory.write("config.toml", "merge = { targets = [\"trunk\"] }\n");
    let output = holosmith(&[
        "config",
        "validate",
        "--config",
        config.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
}

#[test]
fn edit_fails_clearly_with_neither_variable_set() {
    let directory = TempDir::new("edit-no-editor");
    let config = directory.file("config.toml");
    let output = holosmith(&[
        "config",
        "edit",
        "--config",
        config.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr_of(&output),
        "holosmith: config edit: neither $VISUAL nor $EDITOR is set\n"
    );
    assert!(
        !config.exists(),
        "no file is written when there is no editor to open it"
    );
}

#[test]
fn edit_reports_a_non_zero_editor_exit_as_its_own_failure() {
    let directory = TempDir::new("edit-exit-code");
    let config = directory.write("config.toml", "role = \"hub\"\n");
    for code in [2, 7] {
        let editor = directory.editor("editor.sh", &format!("exit {code}"));
        let output = holosmith_with_env(
            &[
                "config",
                "edit",
                "--config",
                config.to_str().expect("UTF-8"),
            ],
            &[("EDITOR", &editor)],
        );
        // `2` is clap's usage code and `vim` exits it for a failed save, so every editor
        // failure is this command's own `1`; the editor's number is in the message.
        assert_eq!(output.status.code(), Some(1), "editor exited {code}");
        assert_eq!(
            stderr_of(&output),
            format!("holosmith: config edit: editor exited with status {code}\n")
        );
        assert_eq!(
            fs::read_to_string(&config).expect("the file is still there"),
            "role = \"hub\"\n"
        );
    }
}

#[test]
fn edit_refuses_a_missing_directory() {
    let directory = TempDir::new("edit-missing-directory");
    let config = directory.file("absent/config.toml");
    let editor = directory.editor("editor.sh", "exit 0");
    let output = holosmith_with_env(
        &[
            "config",
            "edit",
            "--config",
            config.to_str().expect("UTF-8"),
        ],
        &[("EDITOR", &editor)],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr_of(&output),
        format!(
            "holosmith: config edit: no such directory: {}\n",
            directory.file("absent").display()
        )
    );
}

#[test]
fn edit_refuses_an_editor_command_it_cannot_split() {
    let directory = TempDir::new("edit-unparsable");
    let config = directory.file("config.toml");
    let mut command = Command::new(env!("CARGO_BIN_EXE_holosmith"));
    let output = command
        .args([
            "config",
            "edit",
            "--config",
            config.to_str().expect("UTF-8"),
        ])
        .env_remove("EDITOR")
        .env("VISUAL", "code --wait 'unclosed")
        .output()
        .expect("the test binary runs");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr_of(&output),
        "holosmith: config edit: $VISUAL is not a shell-splittable command: code --wait 'unclosed\n"
    );
}

#[test]
fn edit_reports_an_editor_it_cannot_run() {
    let directory = TempDir::new("edit-not-executable");
    let config = directory.file("config.toml");
    let missing = directory.file("no-such-editor");
    let output = holosmith_with_env(
        &[
            "config",
            "edit",
            "--config",
            config.to_str().expect("UTF-8"),
        ],
        &[("EDITOR", &missing)],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr_of(&output),
        format!(
            "holosmith: config edit: cannot run {}: No such file or directory (os error 2)\n",
            missing.display()
        )
    );
}

#[test]
fn edit_prefers_visual_and_passes_its_arguments_through() {
    let directory = TempDir::new("edit-visual");
    let config = directory.write("config.toml", "role = \"hub\"\n");
    // Two words plus the path: the script sees its flag as $1 and the config as $2.
    let visual = directory.editor("visual.sh", "printf 'role = \"server\"\\n' > \"$2\"");
    let editor = directory.editor("editor.sh", "printf 'role = \"both\"\\n' > \"$1\"");
    let output = holosmith_with_env(
        &[
            "config",
            "edit",
            "--config",
            config.to_str().expect("UTF-8"),
        ],
        &[
            (
                "VISUAL",
                &PathBuf::from(format!("{} --wait", visual.display())),
            ),
            ("EDITOR", &editor),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert_eq!(
        fs::read_to_string(&config).expect("the file is still there"),
        "role = \"server\"\n"
    );
}
