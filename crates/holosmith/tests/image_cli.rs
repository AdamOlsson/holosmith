//! `holosmith image build` and `holosmith image run`, driven the way the README tells an operator to run them.
//!
//! The shapes asserted here are the ones a machine with no Docker daemon can still measure: the usage the verb refuses
//! before it reads anything, a project name no create would take, a name that is not under the projects root, a recipe
//! that cannot become an image — refused before the first build, so no daemon measures it — a work directory
//! that is not there, and a daemon that does not answer.
//!
//! **What is not here is a container.** A tag that exists, a second call that does not rebuild it, a `RUN` that fails
//! and one `image run` against a real image are facts about a real daemon, and they are `scripts/verify.sh`'s cases
//! (`case_project_image`, `case_project_image_failure`, `case_base_image_lazy`, `case_image_run`): a Rust test that
//! ran a container would be a test that needs a daemon the suite does not have.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

mod support;
use support::TempDir;

/// A config naming a data root and a projects root under `directory`, both made: what `install` leaves behind.
///
/// `base_image` is left at its default, so the verb's sentences name the base this binary would build rather than an
/// image the test invented.
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

/// The same file, with a `docker_host` that cannot answer: a socket under the temp directory this test made.
///
/// The default would be `/var/run/docker.sock`, which answers on the machine this suite sometimes runs on — and a
/// refusal named by a machine's own daemon is a test that measures the machine rather than the verb.
fn config_file_without_a_daemon(directory: &TempDir) -> PathBuf {
    let config = config_file(directory);
    let path = directory.file("config.toml");
    let mut text = fs::read_to_string(&path).expect("the config this test wrote is readable");
    text.push_str(&format!(
        "docker_host = \"unix://{}\"\n",
        directory.file("no-daemon.sock").display()
    ));
    fs::write(&path, text).expect("the config is writable");
    config
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn text(path: &std::path::Path) -> &str {
    path.to_str().expect("the path is UTF-8")
}

#[test]
fn the_verb_names_one_of_a_project_and_the_base_and_refuses_neither_or_both() {
    let directory = TempDir::new("image-usage");
    let config = config_file(&directory);
    // Neither: clap's own refusal, before a config is read, which is where a missing argument belongs.
    let neither = holosmith(&["--config", text(&config), "image", "build"]);
    assert_eq!(
        neither.status.code(),
        Some(2),
        "`image build` with no subject exited {:?}: {}",
        neither.status.code(),
        stderr_of(&neither)
    );
    assert!(
        stderr_of(&neither).contains("PROJECT"),
        "the refusal does not name the argument that is missing: {}",
        stderr_of(&neither)
    );
    // Both: the two are one subject, and a call that named both would have to choose silently.
    let both = holosmith(&[
        "--config",
        text(&config),
        "image",
        "build",
        "holosmith",
        "--base",
    ]);
    assert_eq!(
        both.status.code(),
        Some(2),
        "`image build <project> --base` was accepted: {}",
        stdout_of(&both)
    );
    assert!(stdout_of(&neither).is_empty() && stdout_of(&both).is_empty());
}

#[test]
fn a_project_that_is_not_under_the_projects_root_is_refused_naming_where_it_looked() {
    let directory = TempDir::new("image-missing-project");
    let config = config_file(&directory);
    let missing = holosmith(&["--config", text(&config), "image", "build", "nope"]);
    assert_eq!(missing.status.code(), Some(1), "{}", stdout_of(&missing));
    let said = stderr_of(&missing);
    assert!(
        said.contains("`nope`") && said.contains("projects"),
        "the refusal does not name the project and the root it looked in: {said}"
    );
    // The answer is one sentence on stderr and nothing on stdout, so a script's `$(…)` stays empty.
    assert!(stdout_of(&missing).is_empty(), "{:?}", stdout_of(&missing));
}

#[test]
fn a_recipe_that_cannot_become_an_image_is_refused_before_anything_is_built() {
    let directory = TempDir::new("image-unusable-recipe");
    let config = config_file(&directory);
    // A directory where the recipe should be: `projectimage::resolve`'s `NotAFile`, which the create refuses too, and
    // which is answered here before the base is asked about — no daemon, and no build of anything.
    let project = directory.path().join("projects").join("holosmith");
    fs::create_dir_all(project.join("docker").join("Dockerfile"))
        .expect("the recipe's directory is creatable");
    let refused = holosmith(&["--config", text(&config), "image", "build", "holosmith"]);
    assert_eq!(refused.status.code(), Some(1), "{}", stdout_of(&refused));
    let said = stderr_of(&refused);
    assert!(
        said.contains(&project.join("docker/Dockerfile").display().to_string()),
        "the refusal does not name the recipe: {said}"
    );
    assert!(
        said.contains("not a regular file"),
        "the refusal is not the recipe's own sentence: {said}"
    );
    assert!(stdout_of(&refused).is_empty());
}

/// `image run` with no command after `--`: a refusal of its own, exit 2, one sentence — not clap's usage line.
///
/// The command has to be given after the separator so a flag meant for it is the command's, and a call that named
/// none is the one an author makes while typing; the sentence says what to write rather than what a placeholder is
/// missing.
#[test]
fn the_run_verb_refuses_a_call_that_named_no_command_after_the_separator() {
    let directory = TempDir::new("image-run-no-command");
    let config = config_file(&directory);
    fs::create_dir_all(directory.path().join("projects").join("holosmith"))
        .expect("the project is creatable");
    for arguments in [
        vec!["--config", text(&config), "image", "run", "holosmith"],
        vec!["--config", text(&config), "image", "run", "holosmith", "--"],
    ] {
        let refused = holosmith(&arguments);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "`image run` with no command exited {:?}: {}",
            refused.status.code(),
            stderr_of(&refused)
        );
        let said = stderr_of(&refused);
        assert!(
            said.contains("holosmith: image run:") && said.contains("`--`"),
            "the refusal does not say what to write: {said}"
        );
        assert!(
            said.lines().count() == 1,
            "the refusal is not one sentence: {said}"
        );
        assert!(stdout_of(&refused).is_empty(), "{:?}", stdout_of(&refused));
    }
}

/// The project name is the create's own refusal, so it is `image build`'s sentence with this verb's exit code.
#[test]
fn the_run_verb_refuses_a_project_that_is_not_under_the_projects_root() {
    let directory = TempDir::new("image-run-missing-project");
    let config = config_file(&directory);
    let missing = holosmith(&[
        "--config",
        text(&config),
        "image",
        "run",
        "nope",
        "--",
        "true",
    ]);
    assert_eq!(missing.status.code(), Some(2), "{}", stdout_of(&missing));
    let said = stderr_of(&missing);
    assert!(
        said.contains("`nope`") && said.contains("projects"),
        "the refusal does not name the project and the root it looked in: {said}"
    );
    assert!(stdout_of(&missing).is_empty(), "{:?}", stdout_of(&missing));
}

/// A recipe that cannot become an image is refused before the daemon question, with the create's own sentence.
#[test]
fn the_run_verb_refuses_a_recipe_that_cannot_become_an_image() {
    let directory = TempDir::new("image-run-unusable-recipe");
    let config = config_file(&directory);
    let project = directory.path().join("projects").join("holosmith");
    fs::create_dir_all(project.join("docker").join("Dockerfile"))
        .expect("the recipe's directory is creatable");
    let refused = holosmith(&[
        "--config",
        text(&config),
        "image",
        "run",
        "holosmith",
        "--",
        "true",
    ]);
    assert_eq!(refused.status.code(), Some(2), "{}", stdout_of(&refused));
    let said = stderr_of(&refused);
    assert!(
        said.contains(&project.join("docker/Dockerfile").display().to_string())
            && said.contains("not a regular file"),
        "the refusal is not the recipe's own sentence: {said}"
    );
    assert!(stdout_of(&refused).is_empty());
}

/// The project name is checked against the create's own rule before it is joined, in both verbs.
///
/// `..` and `.` are names no create would accept, and `<projects_root>/..` is the machine's whole root — which `image
/// run` would mount writable at `/work` if the name reached `repository_of` unchecked. The refusal comes before any
/// lookup, recipe or build, so the config here has no daemon that answers: a verb that asked the daemon first would
/// fail on its sentence rather than on this one.
#[test]
fn neither_verb_takes_a_project_name_that_no_create_would() {
    let directory = TempDir::new("image-name-rule");
    let config = config_file_without_a_daemon(&directory);
    for name in [".", ".."] {
        let built = holosmith(&["--config", text(&config), "image", "build", name]);
        let ran = holosmith(&[
            "--config",
            text(&config),
            "image",
            "run",
            name,
            "--",
            "true",
        ]);
        // Each verb keeps its own code for a refusal: `build`'s failures are its build's, and a call `run` cannot
        // carry out is its 2.
        assert_eq!(
            built.status.code(),
            Some(1),
            "`image build {name}` exited {:?}: {}",
            built.status.code(),
            stderr_of(&built)
        );
        assert_eq!(
            ran.status.code(),
            Some(2),
            "`image run {name}` exited {:?}: {}",
            ran.status.code(),
            stderr_of(&ran)
        );
        for refused in [&built, &ran] {
            let said = stderr_of(refused);
            assert!(
                said.contains(&format!("`{name}`")) && said.contains("not a project name"),
                "the refusal is not the name rule's about `{name}`: {said}"
            );
            assert!(
                said.lines().count() == 1,
                "the refusal is not one sentence: {said}"
            );
            assert!(stdout_of(refused).is_empty(), "{:?}", stdout_of(refused));
        }
    }
}

/// A `--work` that is not a directory is refused, because `docker run -v` would create one instead of mounting it
/// (ADR-0035) — and it is refused before the daemon question, so a machine without one still answers it.
#[test]
fn the_run_verb_refuses_a_work_directory_that_is_not_there() {
    let directory = TempDir::new("image-run-missing-work");
    let config = config_file_without_a_daemon(&directory);
    let project = directory.path().join("projects").join("holosmith");
    fs::create_dir_all(&project).expect("the project is creatable");
    // A file where a directory belongs: the arm `Mounts::check` and this verb share, and the one a typo makes.
    let work = directory.write("not-a-directory", "");
    let refused = holosmith(&[
        "--config",
        text(&config),
        "image",
        "run",
        "holosmith",
        "--work",
        text(&work),
        "--",
        "true",
    ]);
    assert_eq!(refused.status.code(), Some(2), "{}", stdout_of(&refused));
    let said = stderr_of(&refused);
    assert!(
        said.contains(&work.display().to_string()) && said.contains("empty root-owned directory"),
        "the refusal does not name the path and the fact ADR-0035 records: {said}"
    );
    assert!(stdout_of(&refused).is_empty());
}

/// A daemon that does not answer is one sentence and exit 2, from the preflight's own read rather than a second probe.
#[test]
fn the_run_verb_refuses_a_machine_whose_daemon_does_not_answer() {
    let directory = TempDir::new("image-run-no-daemon");
    let config = config_file_without_a_daemon(&directory);
    fs::create_dir_all(directory.path().join("projects").join("holosmith"))
        .expect("the project is creatable");
    let refused = holosmith(&[
        "--config",
        text(&config),
        "image",
        "run",
        "holosmith",
        "--",
        "true",
    ]);
    assert_eq!(refused.status.code(), Some(2), "{}", stdout_of(&refused));
    let said = stderr_of(&refused);
    assert!(
        said.contains("did not answer") && said.contains("no-daemon.sock"),
        "the refusal is not the preflight's sentence about this daemon: {said}"
    );
    assert!(stdout_of(&refused).is_empty());
}
