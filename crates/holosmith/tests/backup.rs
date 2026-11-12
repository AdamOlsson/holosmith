//! The daily project backup (spec item 1's `project_backup`).
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

/// Every name in `root`, sorted — the bundles, and any half-written one a failure left behind, which is
/// what "nothing was left behind" has to look at.
fn names_in(root: &Path) -> Vec<String> {
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
    assert_eq!(names_in(&backups.root), ["holosmith-2026-11-04.bundle"]);
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
    assert_eq!(names_in(&backups.root), Vec::<String>::new());
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
    let kept = names_in(&backups.root);
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
fn a_day_whose_bundle_is_already_there_still_applies_the_retention_rule() {
    let dir = TempDir::new("backup-repair");
    let projects = project(&dir, "holosmith");
    let backups = Backups::new(dir.path());
    let repository = projects.join("holosmith");

    // Today's bundle is there and eight older ones are too — the state a prune that failed yesterday
    // leaves. The ordinary second-session call must repair it rather than return early, or the rule would
    // stay broken until a new day arrived.
    std::fs::create_dir_all(&backups.root).expect("the backups directory is creatable");
    for name in (1..=8)
        .map(|day| format!("holosmith-2026-10-{day:02}.bundle"))
        .chain(std::iter::once("holosmith-2026-11-04.bundle".to_string()))
    {
        std::fs::write(backups.root.join(name), b"").expect("a planted bundle is writable");
    }
    assert_eq!(
        backups
            .take(&repository, "holosmith", DAY_2026_11_04)
            .expect("the day is bundled"),
        None
    );
    let kept = names_in(&backups.root);
    assert_eq!(kept.len(), KEEP, "the repair path kept {kept:?}");
    assert_eq!(
        kept.first().map(String::as_str),
        Some("holosmith-2026-10-03.bundle")
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
        names_in(&backups.root),
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
    let kept = names_in(&root);
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
    assert_eq!(names_in(&dir.path().join("backups")), Vec::<String>::new());
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

// ---- the promotion's database snapshots (spec item 10 step 4) -----------------------------------------

/// The instants the snapshot tests take, an hour apart, from 2026-11-04T00:00:00Z.
const SNAPSHOT_START: i64 = DAY_2026_11_04;

/// A snapshot's name is `app-<YYYYMMDDTHHMMSS>.db`, and the name is the sort key the rotation reads.
#[test]
fn a_snapshot_is_named_after_the_second_it_was_taken_and_sorts_by_it() {
    use holosmith::backup::Snapshots;
    let snapshots = Snapshots::new(Path::new("/var/lib/holosmith"));
    assert_eq!(
        snapshots.path(SNAPSHOT_START),
        Path::new("/var/lib/holosmith/backups/app-20261104T000000.db")
    );
    // The next second, and the newest one: text order is time order for a fixed-width UTC stamp.
    assert_eq!(
        snapshots.path(SNAPSHOT_START + 1),
        Path::new("/var/lib/holosmith/backups/app-20261104T000001.db")
    );
    assert!(snapshots.path(SNAPSHOT_START) < snapshots.path(SNAPSHOT_START + 60));
}

/// Five kept, oldest first, and a file that is not a snapshot's name is neither counted nor removed.
#[test]
fn the_rotation_keeps_the_last_five_snapshots_and_leaves_anything_else_alone() {
    use holosmith::backup::{KEEP_SNAPSHOTS, Snapshots};
    let dir = TempDir::new("backup-rotation");
    let snapshots = Snapshots::new(dir.path());
    std::fs::create_dir_all(&snapshots.root).expect("the backups directory is creatable");
    // A project bundle of the *other* rotation, and an operator's own database: neither is this module's.
    std::fs::write(snapshots.root.join("holosmith-2026-11-04.bundle"), b"x")
        .expect("the fixture bundle is writable");
    std::fs::write(snapshots.root.join("app.db"), b"x").expect("the fixture database is writable");

    let mut taken = Vec::new();
    for step in 0..(KEEP_SNAPSHOTS + 2) {
        let path = snapshots
            .take(SNAPSHOT_START + step as i64 * 60, &|destination| {
                std::fs::write(destination, b"a database").map_err(|error| error.to_string())
            })
            .expect("a snapshot is taken");
        taken.push(path);
    }
    let mut present: Vec<String> = std::fs::read_dir(&snapshots.root)
        .expect("the directory reads")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("app-"))
        .collect();
    present.sort();
    let want: Vec<String> = taken
        .iter()
        .skip(2)
        .map(|path| {
            path.file_name()
                .expect("a snapshot has a name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(present, want, "the rotation did not keep the last five");
    assert!(
        dir.path()
            .join("backups/holosmith-2026-11-04.bundle")
            .exists(),
        "the rotation deleted a project bundle"
    );
    assert!(
        dir.path().join("backups/app.db").exists(),
        "the rotation deleted the live database named app.db"
    );
    // No `.tmp` file outlives a take: the name that exists under `app-<ts>.db` is always a whole snapshot.
    let leftovers: Vec<String> = std::fs::read_dir(&snapshots.root)
        .expect("the directory reads")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a partial snapshot was kept: {leftovers:?}"
    );
}

/// The real thing: `VACUUM INTO` over a **read-only** connection produces a database the engine can open, with the
/// rows the machine had — and a second snapshot of the same second replaces the first rather than colliding.
#[test]
fn a_snapshot_is_a_readable_database_and_a_second_one_in_the_same_second_replaces_it() {
    use holosmith::backup::Snapshots;
    use holosmith::db::{self, Reader};
    let dir = TempDir::new("backup-vacuum");
    let database = support::open(&dir);
    let path = dir.database();
    // One row, so "the snapshot holds what the machine had" is a fact rather than a schema.
    let session = database
        .sessions()
        .create(
            &holosmith::sessions::NewSession {
                project: "holosmith".to_string(),
                model: "claude-sonnet-4.5".to_string(),
                thinking: "high".to_string(),
                network: false,
                client_key: None,
                base_commit: None,
            },
            NOW,
        )
        .expect("a session is created");
    drop(database);

    let snapshots = Snapshots::new(dir.path());
    let vacuum = |destination: &Path| {
        Reader::open(&path)
            .expect("the database opens read-only")
            .expect("the database exists")
            .snapshot(destination)
            .map_err(|error| error.to_string())
    };
    let first = snapshots
        .take(NOW, &vacuum)
        .expect("the first snapshot is taken");
    let again = snapshots
        .take(NOW, &vacuum)
        .expect("a second snapshot of the same second is taken");
    assert_eq!(first, again, "one second has one snapshot's name");
    let names: Vec<String> = std::fs::read_dir(&snapshots.root)
        .expect("the directory reads")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("app-"))
        .collect();
    assert_eq!(
        names.len(),
        1,
        "the same second produced two snapshots: {names:?}"
    );

    // The snapshot is a database, at the schema this build embeds, with the row that existed.
    let reader = Reader::open(&first)
        .expect("the snapshot opens")
        .expect("the snapshot exists");
    assert_eq!(
        reader
            .schema_version(&first)
            .expect("the snapshot records a schema"),
        holosmith::schema::SCHEMA_VERSION
    );
    let rows = reader
        .sessions()
        .list(None)
        .expect("the snapshot's rows read");
    assert!(
        rows.iter().any(|row| row.id == session.id),
        "the snapshot holds no row for the session: {rows:?}"
    );
    // And it is read-only openable while the source is still there, which is what a restore reads first.
    assert!(
        db::recorded_without_migrating(&first)
            .expect("the snapshot records a schema")
            .is_some()
    );
}

/// The instant the snapshot tests create their row at.
const NOW: i64 = SNAPSHOT_START + 3_600;
