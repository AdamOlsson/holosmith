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
    let refusal = stderr_of(&unknown);
    assert!(
        refusal.contains("not a provider this build knows"),
        "{refusal}"
    );
    // The whole table, in the table's own order, and the default provider opens it (ADR-0093): the operator is told
    // which id to store first, not the ids in whatever order a map happened to hold.
    assert!(
        refusal.contains(&holosmith::key::provider_ids().join(", ")),
        "the refusal does not name every provider: {refusal}"
    );
    assert!(
        refusal.contains(&format!("knows: {}, ", holosmith::key::default_provider())),
        "the refusal does not open with the default provider: {refusal}"
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
fn an_unknown_provider_is_refused_before_stdin_is_read() {
    // The bug this pins: the refusal used to happen after `read_to_string`, so a typo'd id sat on a blank screen
    // until a Ctrl-D that the operator had no reason to press. Stdin here is a pipe that is **never written to and
    // never closed**, so a command that reads first can only hang — the wait below is what turns that into a
    // failure instead of a test that hangs the suite.
    let directory = TempDir::new("refuse-first");
    let config = directory.config();
    let mut child = Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args([
            "--config",
            config.to_str().expect("UTF-8"),
            "key",
            "set",
            "not-a-provider",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the test binary runs");
    let open_stdin = child.stdin.take().expect("stdin is piped");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("the child is waitable") {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the refusal is waiting on stdin: an unknown provider must be refused at once"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    drop(open_stdin);
    let output = child.wait_with_output().expect("the output is collectable");
    assert_eq!(status.code(), Some(1), "{}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("not a provider this build knows"),
        "{}",
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output), "", "a refusal writes nothing to stdout");
}

#[test]
fn set_linear_stores_a_tool_key_under_tools_in_a_0600_file_and_never_prints_it() {
    let directory = TempDir::new("tool-set");
    let config = directory.config();
    let config = config.to_str().expect("UTF-8");
    let key = "lin_api_this-is-a-test-key";

    let set = holosmith(
        &["--config", config, "key", "set", "linear"],
        Some(&format!("{key}\n")),
    );
    assert_eq!(set.status.code(), Some(0), "{}", stderr_of(&set));
    assert_eq!(stdout_of(&set), "stored linear\n");
    assert!(!stdout_of(&set).contains(key), "set printed the key");
    assert!(!stderr_of(&set).contains(key), "set logged the key");

    let secrets = directory.path().join("data/secrets");
    assert_eq!(
        fs::metadata(&secrets)
            .expect("the secrets directory exists")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "the secrets directory is 0700"
    );
    let file = secrets.join("keys.toml");
    assert_eq!(
        fs::metadata(&file)
            .expect("the file exists")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the key file is 0600"
    );
    assert_eq!(
        fs::read_to_string(&file).expect("the file is readable"),
        format!("[providers]\n[tools]\nlinear = \"{key}\"\n[sessions]\n"),
        "the key is stored under [tools] and nowhere else"
    );
}

#[test]
fn list_names_a_tool_distinguishably_and_setting_it_twice_leaves_one_value() {
    let directory = TempDir::new("tool-list");
    let config = directory.config();
    let config = config.to_str().expect("UTF-8");
    let first = "lin_api_first-value";
    let second = "lin_api_second-value";
    for key in [first, second] {
        let set = holosmith(
            &["--config", config, "key", "set", "linear"],
            Some(&format!("{key}\n")),
        );
        assert_eq!(set.status.code(), Some(0), "{}", stderr_of(&set));
        assert_eq!(stdout_of(&set), "stored linear\n");
    }

    let list = holosmith(&["--config", config, "key", "list"], None);
    assert_eq!(list.status.code(), Some(0), "{}", stderr_of(&list));
    assert_eq!(
        stdout_of(&list),
        "linear (tool)\n",
        "a tool is listed once, and distinguishably from a provider id"
    );
    assert!(
        !stdout_of(&list).contains(first),
        "list printed the old key"
    );
    assert!(!stdout_of(&list).contains(second), "list printed the key");
    assert!(!stderr_of(&list).contains(second), "list logged the key");

    let store = holosmith::key::Store::from_data_root(&directory.path().join("data"));
    assert_eq!(
        store.tools().expect("the store reads"),
        vec!["linear"],
        "setting the same tool twice left two rows"
    );
    assert_eq!(
        holosmith::key::Store::tool_key(&store.load().expect("the store reads"), "linear"),
        Some(second),
        "the second value did not win"
    );
}

#[test]
fn revoke_is_idempotent_and_the_value_is_gone_after_the_first() {
    let directory = TempDir::new("tool-revoke");
    let config = directory.config();
    let config = config.to_str().expect("UTF-8");
    let key = "lin_api_will-be-revoked";
    let set = holosmith(
        &["--config", config, "key", "set", "linear"],
        Some(&format!("{key}\n")),
    );
    assert_eq!(set.status.code(), Some(0), "{}", stderr_of(&set));
    let file = directory.path().join("data/secrets/keys.toml");
    assert!(fs::read_to_string(&file).expect("readable").contains(key));

    let first = holosmith(&["--config", config, "key", "revoke", "linear"], None);
    assert_eq!(first.status.code(), Some(0), "{}", stderr_of(&first));
    assert_eq!(stdout_of(&first), "revoked linear\n");
    assert!(
        !fs::read_to_string(&file).expect("readable").contains(key),
        "the revoked key is still in the file"
    );

    let again = holosmith(&["--config", config, "key", "revoke", "linear"], None);
    assert_eq!(
        again.status.code(),
        Some(0),
        "a second revoke is not an error: {}",
        stderr_of(&again)
    );
    assert_eq!(stderr_of(&again), "", "a second revoke complained");
    assert_eq!(
        stdout_of(&again).lines().count(),
        1,
        "a second revoke printed {} lines",
        stdout_of(&again).lines().count()
    );

    let list = holosmith(&["--config", config, "key", "list"], None);
    assert_eq!(list.status.code(), Some(0), "{}", stderr_of(&list));
    assert_eq!(stdout_of(&list), "", "the list is empty after revoke");
}

#[test]
fn an_unknown_id_names_both_tables_and_each_command_names_the_other() {
    let directory = TempDir::new("tool-cross-table");
    let config = directory.config();
    let config = config.to_str().expect("UTF-8");

    let unknown = holosmith(
        &["--config", config, "key", "set", "not-a-key-id"],
        Some("sk-x\n"),
    );
    assert_eq!(unknown.status.code(), Some(1));
    let refusal = stderr_of(&unknown);
    assert!(
        refusal.contains(&holosmith::key::provider_ids().join(", ")),
        "the refusal does not name every provider: {refusal}"
    );
    assert!(
        refusal.contains(&holosmith::key::tool_ids().join(", ")),
        "the refusal does not name every tool: {refusal}"
    );
    assert!(
        refusal.contains("not a provider this build knows")
            && refusal.contains("not a tool this build knows"),
        "the refusal does not name both tables: {refusal}"
    );

    let revoke = holosmith(&["--config", config, "key", "revoke", "anthropic"], None);
    assert_eq!(revoke.status.code(), Some(1));
    assert!(
        stderr_of(&revoke).contains("use `holosmith key remove anthropic`"),
        "a provider id given to revoke does not name `key remove`: {}",
        stderr_of(&revoke)
    );

    let remove = holosmith(&["--config", config, "key", "remove", "linear"], None);
    assert_eq!(remove.status.code(), Some(1));
    assert!(
        stderr_of(&remove).contains("use `holosmith key revoke linear`"),
        "a tool id given to remove does not name `key revoke`: {}",
        stderr_of(&remove)
    );

    // A typo is not the other table's id: `remove` keeps its own answer, which is the one a script reads as "nothing
    // was stored under that name".
    let typo = holosmith(&["--config", config, "key", "remove", "not-a-key-id"], None);
    assert_eq!(typo.status.code(), Some(1));
    assert_eq!(
        stderr_of(&typo),
        "holosmith: key remove: not-a-key-id has no key\n",
        "a typo'd provider id is no longer `remove`'s own sentence"
    );
}

#[test]
fn a_tool_key_is_never_a_model_provider() {
    let directory = TempDir::new("tool-not-provider");
    let config = directory.config();
    let config = config.to_str().expect("UTF-8");
    let set = holosmith(
        &["--config", config, "key", "set", "linear"],
        Some("lin_api_not-a-model-key\n"),
    );
    assert_eq!(set.status.code(), Some(0), "{}", stderr_of(&set));

    let store = holosmith::key::Store::from_data_root(&directory.path().join("data"));
    let secrets = store.load().expect("the store reads");
    assert!(
        secrets.providers.is_empty(),
        "a tool key landed in the provider table: {:?}",
        secrets.providers
    );
    assert!(holosmith::key::provider("linear").is_none());
    assert!(!holosmith::key::provider_ids().contains(&"linear"));
    let models = holosmith::key::models(&secrets, "hs-dummy-test");
    assert!(
        !models.contains("linear"),
        "models.json named a tool: {models}"
    );
    assert!(!models.contains("lin_api_not-a-model-key"));
}

/// Rotation is why the key is in the store and read per request: a writer in **another process** (`key set`, twice)
/// rewrites the file, and the reader a forwarded request uses — `Store::load` and then `Store::tool_key` on that one
/// read — sees the new value on its next read: no restart, no cached copy, and nothing else in the secrets directory.
///
/// The wire-level proof — two real forwarded requests answered with two different keys — belongs to a real MCP call
/// through the MCP proxy, which needs a key and the internet (ADR-0099's unmeasured half). What this pins is the half
/// that would go wrong silently, a cached copy in the store.
#[test]
fn a_tool_key_rotation_is_seen_by_the_next_read_of_the_file() {
    let directory = TempDir::new("tool-rotation");
    let config = directory.config();
    let config = config.to_str().expect("UTF-8");
    let store = holosmith::key::Store::from_data_root(&directory.path().join("data"));

    let first = holosmith(
        &["--config", config, "key", "set", "linear"],
        Some("lin_api_before-the-rotation\n"),
    );
    assert_eq!(first.status.code(), Some(0), "{}", stderr_of(&first));
    assert_eq!(stdout_of(&first), "stored linear\n");
    assert_eq!(
        holosmith::key::Store::tool_key(&store.load().expect("the store reads"), "linear"),
        Some("lin_api_before-the-rotation")
    );
    let before = fs::read_to_string(store.path()).expect("the file is readable");

    let second = holosmith(
        &["--config", config, "key", "set", "linear"],
        Some("lin_api_after-the-rotation\n"),
    );
    assert_eq!(second.status.code(), Some(0), "{}", stderr_of(&second));
    assert_eq!(stdout_of(&second), "stored linear\n");
    assert_eq!(
        holosmith::key::Store::tool_key(&store.load().expect("the store reads"), "linear"),
        Some("lin_api_after-the-rotation"),
        "the second read saw the first key: the store cached it"
    );

    let after = fs::read_to_string(store.path()).expect("the file is readable");
    assert_ne!(before, after, "the rotation did not reach the file");
    assert!(after.contains("lin_api_after-the-rotation"));
    assert!(
        !after.contains("lin_api_before-the-rotation"),
        "the old key is still in the file"
    );
    let mut names: Vec<String> = fs::read_dir(directory.path().join("data/secrets"))
        .expect("the secrets directory is readable")
        .map(|entry| {
            entry
                .expect("the entry is readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["keys.toml"],
        "a write left a second file in the secrets directory"
    );
}

/// The file an earlier release left: a tool key written under `[providers]`, because there was one table. It is
/// listed as a tool, revoked as one, and the write that removes it leaves the file in this build's shape.
#[test]
fn a_legacy_providers_row_is_listed_as_a_tool_and_revoked_as_one() {
    let directory = TempDir::new("tool-legacy");
    let config = directory.config();
    let config = config.to_str().expect("UTF-8");
    let secrets = directory.path().join("data/secrets");
    fs::create_dir_all(&secrets).expect("the secrets directory is creatable");
    let file = secrets.join("keys.toml");
    fs::write(
        &file,
        "[providers]\nlinear = \"lin_api_legacy\"\n[sessions]\n",
    )
    .expect("the legacy file is writable");

    let list = holosmith(&["--config", config, "key", "list"], None);
    assert_eq!(list.status.code(), Some(0), "{}", stderr_of(&list));
    assert_eq!(stdout_of(&list), "linear (tool)\n");
    assert!(!stdout_of(&list).contains("lin_api_legacy"));

    let revoke = holosmith(&["--config", config, "key", "revoke", "linear"], None);
    assert_eq!(revoke.status.code(), Some(0), "{}", stderr_of(&revoke));
    assert_eq!(stdout_of(&revoke), "revoked linear\n");
    assert_eq!(
        fs::read_to_string(&file).expect("the file is readable"),
        "[providers]\n[sessions]\n",
        "the rewritten file still carries the legacy row, or a [tools] table with nothing in it"
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
