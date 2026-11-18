//! `holosmith project add` and `holosmith project list`, driven the way the README tells an operator to run
//! them.
//!
//! The clone is a real `git clone` of a local repository, so what these tests assert is what the command does
//! on a machine with no network: the name it prints, the repository it left under the projects root, the
//! refusals a create would make, the removal that keeps a refused clone retryable, and what `list` says about a
//! repository it could not check.
//!
//! Two refused shapes are absent by necessity rather than by omission: `git clone` cannot produce a `.git` that
//! is not a directory, nor a clone carrying `extensions.worktreeConfig` (git writes the clone its own config and
//! does not copy the source's local one). Those two are asserted in `commands::project`'s unit test against the
//! same code and sentence the route answers, which is the layer that can fail for them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod support;
use support::TempDir;

/// A config naming a data root and a projects root under `directory`, both made: what `install` leaves behind.
fn config_file(directory: &TempDir) -> PathBuf {
    let data = directory.path().join("data");
    let projects = directory.path().join("projects");
    fs::create_dir_all(&data).expect("the data root is creatable");
    fs::create_dir_all(&projects).expect("the projects root is creatable");
    directory.write(
        "config.toml",
        &format!("data_root = {data:?}\nprojects_root = {projects:?}\nbackend = \"stub\"\n"),
    )
}

fn projects(directory: &TempDir) -> PathBuf {
    directory.path().join("projects")
}

/// The binary, with the machine's own git configuration kept out of the clone these tests are about.
///
/// The clone is this child's, so the ambient configuration is the one thing that could make the assertions
/// measure the host instead of the command: a global `clone.recurseSubmodules = true` turns the submodule case
/// into a failed clone rather than `submodules_present`, and `protocol.file.allow = never` — a hardening some
/// CI images set — kills the `file://` URLs. `GIT_CEILING_DIRECTORIES` keeps git's upward discovery inside the
/// temporary directory, because a `/tmp` that happened to sit inside a repository would let the unreadable
/// project find *that* repository and answer "no value" instead of "not a repository".
fn holosmith(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args(arguments)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
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

/// A repository at `at` with one commit, which is the least `git clone` can make a working copy of.
///
/// `support::repository` commits an **empty** tree, and a clone of an empty commit is a working copy with
/// nothing in it to assert; the identity still comes from `support::git`'s own `-c user.*`, so this needs no
/// configuration of the machine's.
fn repository(at: &Path) {
    fs::create_dir_all(at).expect("the repository directory is creatable");
    support::git(at, &["init", "--quiet"]);
    fs::write(at.join("README.md"), "a repository\n").expect("the file is writable");
    support::git(at, &["add", "."]);
    support::git(at, &["commit", "--quiet", "-m", "the first commit"]);
}

#[test]
fn add_clones_and_prints_the_name_the_session_form_takes() {
    let directory = TempDir::new("project-add");
    let config = config_file(&directory);
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

    let clone = projects(&directory).join("source");
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
    let directory = TempDir::new("project-derive");
    let config = config_file(&directory);
    let dotted = directory.file("remote/project.git");
    repository(&dotted);
    let slashed = directory.file("remote/other");
    repository(&slashed);

    let dotted_url = format!("file://{}", dotted.display());
    let added = holosmith(&["--config", text(&config), "project", "add", &dotted_url]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr_of(&added));
    assert_eq!(stdout_of(&added), "project\n");
    assert!(projects(&directory).join("project/.git").is_dir());

    let slashed_url = format!("file://{}/", slashed.display());
    let added = holosmith(&["--config", text(&config), "project", "add", &slashed_url]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr_of(&added));
    assert_eq!(stdout_of(&added), "other\n");
    assert!(projects(&directory).join("other/.git").is_dir());
}

/// A destination that is already there is refused by name, before git is asked to clone over it.
#[test]
fn an_existing_destination_is_refused_and_left_alone() {
    let directory = TempDir::new("project-existing");
    let config = config_file(&directory);
    let source = directory.file("source");
    repository(&source);
    let occupied = projects(&directory).join("source");
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
    let directory = TempDir::new("project-noname");
    let config = config_file(&directory);

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
        fs::read_dir(projects(&directory))
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
    let directory = TempDir::new("project-submodules");
    let config = config_file(&directory);
    let source = directory.file("source");
    repository(&source);
    fs::write(
        source.join(".gitmodules"),
        "[submodule \"inner\"]\n\tpath = inner\n\turl = https://example.invalid/inner\n",
    )
    .expect("the file is writable");
    support::git(&source, &["add", ".gitmodules"]);
    support::git(&source, &["commit", "--quiet", "-m", "declare a submodule"]);

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
        !projects(&directory).join("source").exists(),
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
    let directory = TempDir::new("project-failed");
    let config = config_file(&directory);
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
        !projects(&directory).join("missing").exists(),
        "a failed clone leaves no directory in the way of a retry"
    );
}

#[test]
fn list_prints_nothing_and_exits_zero_when_there_are_no_projects() {
    let directory = TempDir::new("project-empty");
    let config = config_file(&directory);
    let listed = holosmith(&["--config", text(&config), "project", "list"]);
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_of(&listed));
    assert_eq!(stdout_of(&listed), "");

    // The projects root itself is absent until something is added, and that is an empty list too.
    fs::remove_dir_all(projects(&directory)).expect("the projects root is removable");
    let listed = holosmith(&["--config", text(&config), "project", "list"]);
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_of(&listed));
    assert_eq!(stdout_of(&listed), "");
}

/// The list is the create's own filter: a directory a session cannot be created in is not a name the form
/// takes, so it is not printed.
#[test]
fn list_leaves_out_a_directory_a_create_would_refuse() {
    let directory = TempDir::new("project-filter");
    let config = config_file(&directory);
    let source = directory.file("source");
    repository(&source);
    let added = holosmith(&["--config", text(&config), "project", "add", text(&source)]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr_of(&added));

    let refused = projects(&directory).join("no-checkout");
    fs::create_dir_all(&refused).expect("the directory is creatable");
    fs::write(refused.join("README.md"), "not a repository\n").expect("the file is writable");

    let listed = holosmith(&["--config", text(&config), "project", "list"]);
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_of(&listed));
    assert_eq!(stdout_of(&listed), "source\n");
}

/// A repository `git` cannot be asked about is not a repository a create has refused: `list` keeps the name,
/// says on stderr what could not be checked, and exits 1 — `status`'s rule for a fact it could not read.
///
/// The shape is a `.git` that is a **directory** git does not recognise, which makes the one reading
/// `worktree::refuse` performs — `git config --local --type=bool --get extensions.worktreeConfig` — exit 128,
/// the status that is neither "a value" (0) nor "no value" (1). It is deliberate that this needs no root and no
/// permission bits: an unreadable `.git/config` in an otherwise real repository is **not** this arm, because
/// git warns and answers "no value" with exit 1, and a git missing from `PATH` would make every project
/// unreadable rather than one.
#[test]
fn list_prints_a_project_git_cannot_be_asked_about_and_exits_one() {
    let directory = TempDir::new("project-unreadable");
    let config = config_file(&directory);
    let source = directory.file("source");
    repository(&source);
    let added = holosmith(&["--config", text(&config), "project", "add", text(&source)]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr_of(&added));

    // What a half-deleted repository looks like: the directory and its `.git` are there, and git answers
    // nothing about them.
    let broken = projects(&directory).join("broken");
    fs::create_dir_all(broken.join(".git")).expect("the broken .git is creatable");

    let listed = holosmith(&["--config", text(&config), "project", "list"]);
    assert_eq!(listed.status.code(), Some(1), "a fact could not be read");
    // The name is still there: the form can use it, and the exit code and the stderr line are what say it is
    // in doubt.
    assert_eq!(stdout_of(&listed), "broken\nsource\n");
    let doubt = stderr_of(&listed);
    assert!(doubt.contains("holosmith: project list: broken"), "{doubt}");
    assert!(doubt.contains("could not be checked"), "{doubt}");
    assert!(
        doubt.contains("can only be used inside a git repository"),
        "git's own words, not a code: {doubt}"
    );
}
