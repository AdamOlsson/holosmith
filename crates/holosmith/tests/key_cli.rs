//! `holosmith key` and the files it owns (spec item 2).
//!
//! The CLI is driven as the operator runs it, so the stdin-only rule, the mode of `keys.toml` and the fact that
//! `key list` never prints a value are real assertions rather than a unit test of a helper.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A directory that removes itself.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = env::temp_dir().join(format!("holosmith-key-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temp directory is creatable");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    fn config(&self) -> PathBuf {
        let data = self.path.join("data");
        let projects = self.path.join("projects");
        fs::create_dir_all(&data).expect("the data root is creatable");
        fs::create_dir_all(&projects).expect("the projects root is creatable");
        let path = self.file("config.toml");
        fs::write(
            &path,
            format!(
                "data_root = {:?}\nprojects_root = {:?}\nbackend = \"stub\"\n",
                data, projects
            ),
        )
        .expect("the config is writable");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn holosmith(arguments: &[&str], stdin: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_holosmith"));
    command
        .args(arguments)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("the test binary runs");
    if let Some(text) = stdin {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(text.as_bytes())
            .expect("the key is written to stdin");
    }
    child.wait_with_output().expect("the command finishes")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn set_reads_stdin_writes_mode_0600_and_list_never_prints_the_value() {
    let directory = TempDir::new("set");
    let config = directory.config();
    let config = config.to_str().expect("UTF-8");
    let key = "sk-ant-this-is-a-test-key";
    let set = holosmith(
        &["--config", config, "key", "set", "anthropic"],
        Some(&format!("{key}\n")),
    );
    assert_eq!(set.status.code(), Some(0), "{}", stderr_of(&set));
    assert_eq!(stdout_of(&set), "stored anthropic\n");
    let file = directory.path().join("data/secrets/keys.toml");
    assert_eq!(
        fs::metadata(&file)
            .expect("the file exists")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the key file is 0600"
    );
    assert!(fs::read_to_string(&file).expect("readable").contains(key));

    let list = holosmith(&["--config", config, "key", "list"], None);
    assert_eq!(list.status.code(), Some(0), "{}", stderr_of(&list));
    assert_eq!(stdout_of(&list), "anthropic\n");
    assert!(!stdout_of(&list).contains(key), "list printed a key");
    assert!(!stderr_of(&list).contains(key), "list logged a key");

    let remove = holosmith(&["--config", config, "key", "remove", "anthropic"], None);
    assert_eq!(remove.status.code(), Some(0), "{}", stderr_of(&remove));
    assert_eq!(stdout_of(&remove), "removed anthropic\n");
    let list = holosmith(&["--config", config, "key", "list"], None);
    assert_eq!(stdout_of(&list), "", "the list is empty after remove");
}

#[test]
fn set_refuses_an_unknown_provider_and_an_empty_key() {
    let directory = TempDir::new("refuse");
    let config = directory.config();
    let config = config.to_str().expect("UTF-8");
    let unknown = holosmith(
        &["--config", config, "key", "set", "not-a-provider"],
        Some("sk-x\n"),
    );
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        stderr_of(&unknown).contains("not a provider this build knows"),
        "{}",
        stderr_of(&unknown)
    );
    let empty = holosmith(&["--config", config, "key", "set", "anthropic"], Some("\n"));
    assert_eq!(empty.status.code(), Some(1));
    assert!(
        stderr_of(&empty).contains("the key is empty"),
        "{}",
        stderr_of(&empty)
    );
}

#[test]
fn the_session_models_file_holds_the_dummy_key_and_never_the_real_one() {
    let directory = TempDir::new("models");
    let store = holosmith::key::Store::new(directory.path().join("secrets/keys.toml"));
    let real = "sk-ant-real-do-not-leak";
    store
        .set_provider("anthropic", real)
        .expect("the provider is stored");
    let dummy = store
        .dummy_for("0123456789abcdef")
        .expect("a dummy is minted");
    let secrets = store.load().expect("the store reads");
    let text = holosmith::key::models(&secrets, &dummy);
    assert!(text.contains("http://key-proxy:8080/anthropic"), "{text}");
    assert!(text.contains(&dummy), "{text}");
    assert!(
        !text.contains(real),
        "the models file leaked the real key: {text}"
    );
    assert!(!holosmith::key::contains_real_key(&secrets, &text));
}
