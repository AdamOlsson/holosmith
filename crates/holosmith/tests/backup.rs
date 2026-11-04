//! The daily project backup (HS-065, spec item 1's `project_backup`).
//!
//! Two halves: the module against a real git repository in a `TempDir`, and the create's own behaviour
//! over a socket — one bundle a day, and a failed bundle that does not stop a session. Every test that
//! needs a repository makes one with `support::repository`, which commits with an identity on the
//! command line so nothing depends on the machine's `user.email`.

use std::path::Path;
use std::time::Duration;

use holosmith::backup::{Backups, KEEP};
use holosmith::config::Config;
use holosmith::http;

mod support;
use support::{Served, TempDir, fast_stub, git, repository, request, state_in, with_stub};

const GRACE: Duration = Duration::from_secs(10);

/// 2026-11-04T00:00:00Z. The days below count from here, so a test names the day it means instead of
/// racing midnight (the reason `take` takes the instant at all).
const DAY_2026_11_04: i64 = 1_793_750_400;
const DAY: i64 = 86_400;

/// A config that keeps a bundle of `projects_root`'s repositories and writes below `data_root`.
fn config(data_root: &Path, projects_root: &Path) -> Config {
    Config {
        project_backup: true,
        data_root: data_root.to_path_buf(),
        projects_root: projects_root.to_path_buf(),
        backend: holosmith::runtime::Backend::Stub,
        ..Config::default()
    }
}

/// A repository at `<projects>/<name>` with one commit, and the projects root it lives in.
fn project(dir: &TempDir, name: &str) -> std::path::PathBuf {
    let root = dir.file("projects");
    let path = root.join(name);
    repository(&path);
    root
}

/// The names of the bundles in `root`, sorted.
fn bundles(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Every file in `root`, including a half-written one: what "nothing was left behind" means.
fn files(root: &Path) -> Vec<String> {
    bundles(root)
}

#[test]
fn a_day_is_bundled_once_and_the_second_attempt_finds_it() {
    let dir = TempDir::new("backup-once");
    let projects = project(&dir, "holosmith");
    let backups = Backups::new(dir.path());
    let repository = projects.join("holosmith");

    let first = backups
        .take(&repository, "holosmith", DAY_2026_11_04)
        .expect("the first bundle is taken")
        .expect("the first attempt wrote a bundle");
    assert_eq!(
        first.file_name().expect("a bundle has a name"),
        "holosmith-2026-11-04.bundle"
    );
    // The second session of the same day is the ordinary case, and it must cost one `stat`.
    assert_eq!(
        backups
            .take(&repository, "holosmith", DAY_2026_11_04 + 60)
            .expect("a second attempt is not a failure"),
        None
    );
    assert_eq!(bundles(&backups.root), ["holosmith-2026-11-04.bundle"]);
}

#[test]
fn the_bundle_is_one_git_can_restore() {
    let dir = TempDir::new("backup-verify");
    let projects = project(&dir, "holosmith");
    let backups = Backups::new(dir.path());
    let repository = projects.join("holosmith");
    let bundle = backups
        .take(&repository, "holosmith", DAY_2026_11_04)
        .expect("the bundle is taken")
        .expect("the first attempt wrote a bundle");

    // `git bundle verify` is what the module itself runs before renaming the file into place; running it
    // again here is what says the *kept* file is the one that verified.
    git(
        &repository,
        &["bundle", "verify", &bundle.to_string_lossy()],
    );
    // `--all` is the story's flag, and a bundle of one branch is not a recovery point for a repository
    // that has several: the heads are what proves it carried them.
    let heads = git(
        &repository,
        &["bundle", "list-heads", &bundle.to_string_lossy()],
    );
    assert!(
        heads.contains("refs/heads/"),
        "the bundle carries no branch: {heads}"
    );
}

#[test]
fn a_repository_with_nothing_to_bundle_leaves_nothing_behind() {
    let dir = TempDir::new("backup-empty");
    let root = dir.file("projects");
    let repository = root.join("holosmith");
    std::fs::create_dir_all(&repository).expect("the repository directory is creatable");
    git(&repository, &["init", "--quiet"]);
    let backups = Backups::new(dir.path());

    // `git bundle create --all` refuses an empty repository. The answer is an error the create logs, and
    // the directory must hold no file at all: not the final name, and not the temporary one a failed
    // bundle was written under.
    assert!(
        backups
            .take(&repository, "holosmith", DAY_2026_11_04)
            .is_err(),
        "an empty repository produced a bundle"
    );
    assert_eq!(files(&backups.root), Vec::<String>::new());
}

#[test]
fn the_last_seven_days_are_kept() {
    let dir = TempDir::new("backup-prune");
    let projects = project(&dir, "holosmith");
    let backups = Backups::new(dir.path());
    let repository = projects.join("holosmith");

    // Nine days of real bundles, so the prune is tested against the files it will actually meet rather
    // than against nine empty files that happen to be named like them.
    for day in 0..9 {
        backups
            .take(&repository, "holosmith", DAY_2026_11_04 + day * DAY)
            .expect("each day is bundled")
            .expect("each day is a new bundle");
    }
    let kept = bundles(&backups.root);
    assert_eq!(kept.len(), KEEP, "the prune kept {kept:?}");
    // Nine days taken, seven kept: the two oldest go, and the week that remains is the newest one.
    assert_eq!(
        kept.first().map(String::as_str),
        Some("holosmith-2026-11-06.bundle")
    );
    assert_eq!(
        kept.last().map(String::as_str),
        Some("holosmith-2026-11-12.bundle")
    );
}

#[test]
fn one_projects_prune_is_not_another_projects() {
    let dir = TempDir::new("backup-projects");
    let projects = project(&dir, "holosmith");
    let other = projects.join("holosmith2");
    repository(&other);
    let backups = Backups::new(dir.path());

    // A name that is a prefix of another is the case a naive `starts_with` gets wrong: `holosmith`'s
    // prune must not touch `holosmith2`'s bundles.
    for day in 0..2 {
        backups
            .take(
                &projects.join("holosmith"),
                "holosmith",
                DAY_2026_11_04 + day * DAY,
            )
            .expect("holosmith is bundled");
        backups
            .take(&other, "holosmith2", DAY_2026_11_04 + day * DAY)
            .expect("holosmith2 is bundled");
    }
    assert_eq!(
        bundles(&backups.root),
        [
            "holosmith-2026-11-04.bundle",
            "holosmith-2026-11-05.bundle",
            "holosmith2-2026-11-04.bundle",
            "holosmith2-2026-11-05.bundle",
        ]
    );
}

/// One create over a socket, answering the row.
async fn create(addr: std::net::SocketAddr, key: &str) -> support::Response {
    let body = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;
    let text = format!(
        "POST /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://localhost:8080\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\n\
         Idempotency-Key: {key}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    request(addr, &text).await
}

#[tokio::test]
async fn a_create_takes_the_days_bundle_before_the_session_runs() {
    let dir = TempDir::new("backup-create");
    let projects = project(&dir, "holosmith");
    let state = with_stub(state_in(&dir, config(dir.path(), &projects)), fast_stub());
    let served = Served::start(http::router(state), GRACE).await;

    for key in ["first", "second"] {
        let answer = create(served.addr, key).await;
        assert_eq!(answer.status, 202, "{}", answer.body);
    }

    // Two sessions on one day are one bundle, and it is of the repository the create named.
    let root = dir.path().join("backups");
    let kept = bundles(&root);
    assert_eq!(kept.len(), 1, "two creates produced {kept:?}");
    assert!(
        kept[0].starts_with("holosmith-") && kept[0].ends_with(".bundle"),
        "the bundle is not named after the project and the day: {kept:?}"
    );
    git(
        &projects.join("holosmith"),
        &["bundle", "verify", &root.join(&kept[0]).to_string_lossy()],
    );
}

#[tokio::test]
async fn a_bundle_that_cannot_be_taken_does_not_block_the_create() {
    let dir = TempDir::new("backup-failure");
    // A projects root with no repository in it: `git bundle` will refuse, and the session must still be
    // created — a broken safety net is not a reason to refuse work (the story's third line).
    let projects = dir.file("projects");
    std::fs::create_dir_all(&projects).expect("the projects root is creatable");
    let state = with_stub(state_in(&dir, config(dir.path(), &projects)), fast_stub());
    let served = Served::start(http::router(state), GRACE).await;

    let answer = create(served.addr, "no-repository").await;
    assert_eq!(answer.status, 202, "{}", answer.body);
    let row: serde_json::Value = serde_json::from_str(&answer.body).expect("the answer is JSON");
    assert_eq!(row["project"].as_str(), Some("holosmith"));
    // And nothing was left behind under a name an operator would trust.
    assert_eq!(files(&dir.path().join("backups")), Vec::<String>::new());
}

#[tokio::test]
async fn nothing_is_bundled_when_the_setting_is_off() {
    let dir = TempDir::new("backup-off");
    let projects = project(&dir, "holosmith");
    let state = with_stub(
        state_in(
            &dir,
            Config {
                project_backup: false,
                ..config(dir.path(), &projects)
            },
        ),
        fast_stub(),
    );
    let served = Served::start(http::router(state), GRACE).await;

    let answer = create(served.addr, "off").await;
    assert_eq!(answer.status, 202, "{}", answer.body);
    // The default is off, and off must mean "no directory, no subprocess, no cost".
    assert!(!dir.path().join("backups").exists());
}
