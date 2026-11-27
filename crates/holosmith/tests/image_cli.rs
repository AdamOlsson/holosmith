//! `holosmith image build`, driven the way the README tells an operator to run it.
//!
//! Three shapes are asserted here, and they are the three a machine with no Docker daemon can still measure: the
//! usage the verb refuses before it reads anything, a project name that is not under the projects root, and a recipe
//! that cannot become an image — which is refused before the first build, so a daemon would have measured nothing.
//!
//! **What is not here is the build.** A tag that exists, a second call that does not rebuild it and a `RUN` that fails
//! are facts about a real daemon, and they are `scripts/verify.sh`'s cases (`case_project_image`,
//! `case_project_image_failure`, `case_base_image_lazy`): a Rust test that built an image would be a test that takes
//! minutes and needs a daemon the suite does not have.

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
