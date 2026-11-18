//! `holosmith project add` and `holosmith project list`, driven the way the README tells an operator to run
//! them.
//!
//! The clone is a real `git clone` of a local repository, so what these tests assert is what the command does
//! on a machine with no network: the name it prints, the repository it left under the projects root, the
//! refusals a create would make, and the removal that keeps a refused clone retryable.
//!
//! Two refused shapes are absent by necessity rather than by omission: `git clone` cannot produce a `.git`
//! that is not a directory, nor a clone carrying `extensions.worktreeConfig` (git writes the clone its own
//! config and does not copy the source's local one). Those two are asserted in
//! `commands::project`'s unit test against the same code and sentence the route answers, which is the layer
//! that can fail for them.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A directory that removes itself.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path =
            env::temp_dir().join(format!("holosmith-project-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temp directory is creatable");
        Self { path }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// A config naming a data root and a projects root under this directory, both made.
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

    fn projects(&self) -> PathBuf {
        self.path.join("projects")
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
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the test binary runs")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn text(path: &Path) -> &str {
    path.to_str().expect("the path is UTF-8")
}

/// `git` in `at`, with the sandbox's own configuration out of the way: whether this machine has a commit
/// identity, a template directory or a default branch is not what any of these tests are about.
fn git(at: &Path, arguments: &[&str]) -> Output {
    let output = Command::new("git")
        .arg("-C")
        .arg(at)
        .args(arguments)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Holosmith")
        .env("GIT_AUTHOR_EMAIL", "holosmith@example.invalid")
        .env("GIT_COMMITTER_NAME", "Holosmith")
        .env("GIT_COMMITTER_EMAIL", "holosmith@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// A repository at `at` with one commit, which is the least `git clone` can make a working copy of.
fn repository(at: &Path) {
    fs::create_dir_all(at).expect("the repository directory is creatable");
    git(at, &["init", "-q"]);
    fs::write(at.join("README.md"), "a repository\n").expect("the file is writable");
    git(at, &["add", "."]);
    git(at, &["commit", "-q", "-m", "the first commit"]);
}

#[test]
fn add_clones_and_prints_the_name_the_session_form_takes() {
    let directory = TempDir::new("add");
    let config = directory.config();
    let source = directory.file("source");
    repository(&source);

    let added = holosmith(&["--config", text(&config), "project", "add", text(&source)]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr_of(&added));
    // The name alone: this is what `name=$(holosmith project add …)` captures.
    assert_eq!(stdout_of(&added), "source\n");
    assert!(
        stderr_of(&added).contains("cloning"),
        "{}",
        stderr_of(&added)
    );

    let clone = directory.projects().join("source");
    assert!(clone.join(".git").is_dir(), "the clone is a repository");
    assert_eq!(
        fs::read_to_string(clone.join("README.md")).expect("the clone has the file"),
        "a repository\n"
    );

    let listed = holosmith(&["--config", text(&config), "project", "list"]);
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_of(&listed));
    assert_eq!(stdout_of(&listed), "source\n");
}

/// The name comes from the address, so a `.git` suffix and a trailing `/` are the same project.
#[test]
fn the_name_is_the_last_segment_less_a_trailing_slash_and_dot_git() {
    let directory = TempDir::new("derive");
    let config = directory.config();
    let dotted = directory.file("remote/project.git");
    repository(&dotted);
    let slashed = directory.file("remote/other");
    repository(&slashed);

    let dotted_url = format!("file://{}", dotted.display());
    let added = holosmith(&["--config", text(&config), "project", "add", &dotted_url]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr_of(&added));
    assert_eq!(stdout_of(&added), "project\n");
    assert!(directory.projects().join("project/.git").is_dir());

    let slashed_url = format!("file://{}/", slashed.display());
    let added = holosmith(&["--config", text(&config), "project", "add", &slashed_url]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr_of(&added));
    assert_eq!(stdout_of(&added), "other\n");
    assert!(directory.projects().join("other/.git").is_dir());
}

/// A destination that is already there is refused by name, before git is asked to clone over it.
#[test]
fn an_existing_destination_is_refused_and_left_alone() {
    let directory = TempDir::new("existing");
    let config = directory.config();
    let source = directory.file("source");
    repository(&source);
    let occupied = directory.projects().join("source");
    fs::create_dir_all(&occupied).expect("the directory is creatable");
    fs::write(occupied.join("notes.txt"), "not a repository\n").expect("the file is writable");

    let added = holosmith(&["--config", text(&config), "project", "add", text(&source)]);
    assert_eq!(added.status.code(), Some(1));
    assert_eq!(stdout_of(&added), "", "a refusal prints nothing on stdout");
    let refusal = stderr_of(&added);
    assert!(refusal.contains(text(&occupied)), "{refusal}");
    assert!(refusal.contains("already exists"), "{refusal}");
    assert_eq!(
        fs::read_to_string(occupied.join("notes.txt")).expect("the file is still there"),
        "not a repository\n"
    );
}

/// A URL with no usable last segment is refused with a sentence, and nothing is cloned.
#[test]
fn a_url_that_names_no_project_is_refused() {
    let directory = TempDir::new("noname");
    let config = directory.config();

    let added = holosmith(&[
        "--config",
        text(&config),
        "project",
        "add",
        "https://github.com/you/..",
    ]);
    assert_eq!(added.status.code(), Some(1));
    assert_eq!(stdout_of(&added), "");
    let refusal = stderr_of(&added);
    assert!(refusal.contains("names no project"), "{refusal}");
    assert_eq!(
        fs::read_dir(directory.projects())
            .expect("the projects root is readable")
            .count(),
        0,
        "nothing was cloned"
    );
}

/// A clone a create would refuse is refused here, removed, and so the corrected URL can be added under the
/// same name: without the removal the second run would answer `already exists`.
#[test]
fn a_clone_with_submodules_is_refused_removed_and_retryable() {
    let directory = TempDir::new("submodules");
    let config = directory.config();
    let source = directory.file("source");
    repository(&source);
    fs::write(
        source.join(".gitmodules"),
        "[submodule \"inner\"]\n\tpath = inner\n\turl = https://example.invalid/inner\n",
    )
    .expect("the file is writable");
    git(&source, &["add", ".gitmodules"]);
    git(&source, &["commit", "-q", "-m", "declare a submodule"]);

    let added = holosmith(&["--config", text(&config), "project", "add", text(&source)]);
    assert_eq!(added.status.code(), Some(1));
    assert_eq!(stdout_of(&added), "");
    let refusal = stderr_of(&added);
    assert!(
        refusal.contains("submodules_present: the project has submodules"),
        "the create's own code and sentence: {refusal}"
    );
    assert!(refusal.contains("was removed"), "{refusal}");
    assert!(
        !directory.projects().join("source").exists(),
        "the refused clone is gone, so the name is free"
    );

    // The retry reaches the same refusal rather than the destination check: the removal is what makes this
    // the same answer twice instead of `already exists`.
    let again = holosmith(&["--config", text(&config), "project", "add", text(&source)]);
    assert_eq!(again.status.code(), Some(1));
    let refusal = stderr_of(&again);
    assert!(refusal.contains("submodules_present"), "{refusal}");
    assert!(!refusal.contains("already exists"), "{refusal}");
}

/// A clone that fails is one sentence on stderr naming git's own reason, and it leaves nothing behind.
#[test]
fn a_failed_clone_says_why_in_one_line_and_leaves_nothing() {
    let directory = TempDir::new("failed");
    let config = directory.config();
    let missing = directory.file("missing");

    let added = holosmith(&["--config", text(&config), "project", "add", text(&missing)]);
    assert_eq!(added.status.code(), Some(1));
    assert_eq!(stdout_of(&added), "");
    let refusal = stderr_of(&added);
    let verdict = refusal.lines().last().expect("there is a failure line");
    assert!(verdict.contains("git clone"), "{verdict}");
    assert!(
        verdict.contains("does not exist") || verdict.contains("not a git repository"),
        "git's own reason: {verdict}"
    );
    assert!(
        !directory.projects().join("missing").exists(),
        "a failed clone leaves no directory in the way of a retry"
    );
}

#[test]
fn list_prints_nothing_and_exits_zero_when_there_are_no_projects() {
    let directory = TempDir::new("empty");
    let config = directory.config();
    let listed = holosmith(&["--config", text(&config), "project", "list"]);
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_of(&listed));
    assert_eq!(stdout_of(&listed), "");

    // The projects root itself is absent until something is added, and that is an empty list too.
    fs::remove_dir_all(directory.projects()).expect("the projects root is removable");
    let listed = holosmith(&["--config", text(&config), "project", "list"]);
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_of(&listed));
    assert_eq!(stdout_of(&listed), "");
}

/// The list is the create's own filter: a directory a session cannot be created in is not a name the form
/// takes, so it is not printed.
#[test]
fn list_leaves_out_a_directory_a_create_would_refuse() {
    let directory = TempDir::new("filter");
    let config = directory.config();
    let source = directory.file("source");
    repository(&source);
    let added = holosmith(&["--config", text(&config), "project", "add", text(&source)]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr_of(&added));

    let refused = directory.projects().join("no-checkout");
    fs::create_dir_all(&refused).expect("the directory is creatable");
    fs::write(refused.join("README.md"), "not a repository\n").expect("the file is writable");

    let listed = holosmith(&["--config", text(&config), "project", "list"]);
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_of(&listed));
    assert_eq!(stdout_of(&listed), "source\n");
}
