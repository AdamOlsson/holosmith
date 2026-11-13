//! `holosmith upgrade <binary>`: the pipeline's order and its refusals (spec item 10).
//!
//! Acceptance bullet 1 is a claim about **order** — "runs the spec's pipeline in order" — and the parts of that order a
//! gate case cannot record are the drain, the backup and the review acknowledgement: none of them is a child. So the
//! pipeline is driven here through [`promote::Steps`], the seam `promote::execute` asks the machine through, and the
//! answers are recorded: a refusal at one step has to leave every later step *never asked*, which is what makes the
//! order a measurement rather than a print.
//!
//! The candidate is the shipped artifact (`dist/<triple>/holosmith`, the fixture `tests/images.rs` also uses), because
//! a promotion's first two checks are about bytes: a static ELF for this machine, a `.sha256` beside it, and its own
//! `version --json` — none of which a fake file can satisfy. Without the artifact the whole order half is skipped with
//! one printed line (the daemon-free half of the gate's `case_upgrade` still measures the refusals), which is the
//! convention `tests/images.rs`'s `ready()` established for the same reason.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use holosmith::config::Config;
use holosmith::deploy::Root;
use holosmith::http::health::Answer;
use holosmith::images::{self, Expectation, Kind, Log};
use holosmith::promote::{self, DrainAnswer, Options, Steps};
use holosmith::release::Release;
use holosmith::smoke;
use holosmith::version::VersionInfo;

mod support;
use support::TempDir;

/// The window the refusals are measured with: short enough that a poll which gives up does so now.
const WINDOW: Duration = Duration::from_millis(60);

/// One recorded call, in the order the pipeline made it.
type Trace = Vec<String>;

/// The machine, as a recorder: every question appends its name, and every answer is the test's.
struct Recorder {
    trace: Mutex<Trace>,
    /// What the smoke run answers.
    smoke: smoke::Outcome,
    /// What the drain answers.
    drain: DrainAnswer,
    /// What `/api/health` answers: `(version, uptime_s)`.
    health: (String, u64),
    /// Which images the daemon has, by tag — empty is "nothing built yet".
    labels: BTreeMap<String, BTreeMap<String, String>>,
}

impl Recorder {
    fn new(version: &str) -> Self {
        Self {
            trace: Mutex::new(Vec::new()),
            smoke: smoke::Outcome::Passed,
            drain: DrainAnswer::Drained,
            health: (version.to_string(), 0),
            labels: BTreeMap::new(),
        }
    }

    fn record(&self, what: impl Into<String>) {
        self.trace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(what.into());
    }

    fn trace(&self) -> Trace {
        self.trace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Steps for Recorder {
    fn smoke(&self, _candidate: &Path, _root: &Path) -> smoke::Outcome {
        self.record("smoke");
        self.smoke.clone()
    }

    fn images(&self) -> &dyn images::Images {
        self
    }

    fn drain(&self, _admin_dir: &Path, _wait: Duration) -> DrainAnswer {
        self.record("drain");
        self.drain.clone()
    }

    fn restart(&self, _tools: &holosmith::deploy::Tools) -> Result<(), String> {
        self.record("restart");
        Ok(())
    }

    fn health(&self, _listen: SocketAddr) -> Result<Answer, String> {
        self.record(format!("health:{}", self.health.1));
        Ok(Answer {
            name: "holosmith".to_string(),
            version: self.health.0.clone(),
            role: holosmith::config::Role::Server,
            uptime_s: self.health.1,
            preview_origin: None,
        })
    }
}

impl images::Images for Recorder {
    fn daemon_arch(&self) -> Result<String, String> {
        Ok(if env!("HOLOSMITH_TARGET").starts_with("aarch64") {
            "arm64".to_string()
        } else {
            "amd64".to_string()
        })
    }

    fn labels(&self, image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        self.record(format!("labels:{image}"));
        Ok(self.labels.get(image).cloned())
    }

    fn build(&self, tag: &str, _archive: &[u8], _args: &[String]) -> Result<Log, String> {
        self.record(format!("build:{tag}"));
        Ok(Log {
            success: true,
            text: String::new(),
        })
    }
}

/// The shipped artifact, or `None` on a tree that has not been built.
fn artifact() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HOLOSMITH_IMAGE_BIN") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut targets = vec![env!("HOLOSMITH_TARGET")];
    targets.extend(
        ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]
            .into_iter()
            .filter(|triple| *triple != env!("HOLOSMITH_TARGET")),
    );
    targets
        .into_iter()
        .map(|triple| workspace.join(format!("dist/{triple}/holosmith")))
        .find(|path| path.is_file())
}

/// The tree one test promotes in: a config, an install tree with `releases/old` live, a database, and the candidate.
struct World {
    dir: TempDir,
    config: Config,
    root: Root,
    candidate: PathBuf,
    info: VersionInfo,
}

fn world(label: &str) -> Option<World> {
    let artifact = artifact()?;
    let dir = TempDir::new(label);
    // The two files `holosmith build` writes beside each other: the artifact and its digest.
    let candidate = dir.file("holosmith");
    std::fs::copy(&artifact, &candidate).expect("the artifact is copyable");
    let digest = images::digest(&candidate).expect("the candidate is readable");
    std::fs::write(
        dir.file("holosmith.sha256"),
        format!("{digest}  holosmith\n"),
    )
    .expect("the digest file is writable");
    let info = promote::ask_version(&candidate).expect("the artifact answers `version --json`");

    let config = Config {
        data_root: dir.file("data"),
        projects_root: dir.file("projects"),
        control_listen: "127.0.0.1:1".parse().expect("a valid address"),
        ..Config::default()
    };
    std::fs::create_dir_all(&config.data_root).expect("the data root is creatable");
    std::fs::create_dir_all(&config.projects_root).expect("the projects root is creatable");
    let database = holosmith::db::Database::open(
        &config.data_root.join(holosmith::db::DATABASE_FILE),
        holosmith::db::OpenOptions::default(),
        holosmith::clock::unix_now(),
    )
    .expect("a fresh database opens");
    drop(database);

    let root = dir.file("root");
    let root = Root::resolve(Some(root), Some(dir.file("holosmith.toml")));
    let release = Release::of(&root);
    std::fs::create_dir_all(release.releases()).expect("the releases directory is creatable");
    let old = b"#!/bin/sh\nexit 0\n";
    let old_digest = {
        let path = dir.file("old-holosmith");
        std::fs::write(&path, old).expect("the fixture binary is writable");
        images::digest(&path).expect("the fixture binary is readable")
    };
    release
        .write("old", old, &old_digest)
        .expect("the previous release is writable");
    release
        .switch("old")
        .expect("the previous release is the live one");

    Some(World {
        dir,
        config,
        root,
        candidate,
        info,
    })
}

/// The options a test promotes with, acknowledging the review.
fn options(world: &World, acknowledge_review: bool) -> Options {
    Options {
        candidate: world.candidate.clone(),
        acknowledge_review,
        wait: Duration::from_millis(1),
        window: WINDOW,
    }
}

fn execute(world: &World, options: &Options, steps: &Recorder) -> Result<(), String> {
    promote::execute(
        &world.config,
        &world.root,
        options,
        steps,
        &holosmith::deploy::Tools::default(),
    )
}

/// Every `app-*.db` the data root's backups directory holds.
fn backups(world: &World) -> Vec<String> {
    let backups = world.config.data_root.join(holosmith::backup::BACKUPS);
    match std::fs::read_dir(&backups) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("app-"))
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn a_promotion_asks_the_machine_in_the_specs_order() {
    let Some(world) = world("upgrade-order") else {
        eprintln!(
            "SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote (scripts/build.sh produces it; the gate's \
             case_upgrade measures the same order end to end)"
        );
        return;
    };
    let steps = Recorder::new(&world.info.version);
    execute(&world, &options(&world, true), &steps).expect("the promotion should succeed");

    // The order, as the machine saw it: the smoke test, then the two images (read, then built), then the drain, then
    // the restart and the health poll. The backup, the release write and the switch are this process's own writes and
    // are asserted by their effects below.
    let trace = steps.trace();
    let proxy = format!("holosmith-proxy:{}", world.info.version);
    let agent = format!("agent-image:{}", world.info.version);
    assert_eq!(
        trace,
        [
            "smoke".to_string(),
            format!("labels:{proxy}"),
            format!("labels:{agent}"),
            format!("build:{proxy}"),
            format!("build:{agent}"),
            "drain".to_string(),
            "restart".to_string(),
            "health:0".to_string(),
        ],
        "the pipeline did not run in the spec's order: {trace:?}"
    );

    // The backup exists, and it is a database — taken after the drain and before the switch, which is the position the
    // trace above and this file's own existence put it in.
    let backups = backups(&world);
    assert_eq!(backups.len(), 1, "{backups:?}");
    let snapshot = world
        .config
        .data_root
        .join(holosmith::backup::BACKUPS)
        .join(&backups[0]);
    assert!(
        holosmith::db::recorded_without_migrating(&snapshot)
            .expect("the snapshot opens read-only")
            .is_some(),
        "the backup is not a database"
    );

    // The candidate's release directory, and `current` pointing at it.
    let release = Release::of(&world.root);
    assert_eq!(
        release.link(),
        Some(Path::new(holosmith::release::RELEASES).join(&world.info.git_tag)),
        "`current` does not point at the candidate's release"
    );
    assert!(
        release.binary(&world.info.git_tag).is_file(),
        "the candidate's release directory holds no binary"
    );
    // The unit runs through `current`, which is what makes the restart start the candidate.
    let unit = std::fs::read_to_string(world.root.unit()).expect("the unit is readable");
    let live = world
        .root
        .current()
        .join(holosmith::release::BINARY)
        .display()
        .to_string();
    assert!(
        unit.contains(&format!("ExecStart={live} serve")),
        "the unit does not run through `current`:\n{unit}"
    );
}

/// One more release in the tree, with an explicit mtime: the order the `history` reads is the *test's* then.
///
/// `File::open(dir).set_modified` is how a directory's own mtime is set (no `touch` child, no `filetime` crate), and
/// it has to be opened **after** the release's files are written, because writing into a directory refreshes it.
fn seed(world: &World, tag: &str, seconds: u64) {
    let release = Release::of(&world.root);
    let bytes = b"#!/bin/sh\nexit 0\n";
    let path = world.dir.file(&format!("seed-{tag}"));
    std::fs::write(&path, bytes).expect("the fixture binary is writable");
    let digest = images::digest(&path).expect("the fixture binary is readable");
    release
        .write(tag, bytes, &digest)
        .expect("the release is writable");
    let dir = release.dir_of(tag);
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
    std::fs::File::open(&dir)
        .expect("the release directory opens")
        .set_modified(when)
        .expect("the release's mtime is set");
}

#[test]
fn a_promotion_that_succeeds_prunes_the_tree_and_keeps_the_rollback_target() {
    let Some(mut world) = world("upgrade-prune") else {
        eprintln!("SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote");
        return;
    };
    // Five releases after this promotion: the candidate, `old`, and three older ones. `keep_releases = 4` therefore
    // removes exactly one — and it is the oldest, not the live release and not the one behind it.
    seed(&world, "ancient1", 1_000_000_000);
    seed(&world, "ancient2", 1_100_000_000);
    seed(&world, "ancient3", 1_200_000_000);
    world.config.keep_releases = 4;
    let release = Release::of(&world.root);
    assert_eq!(
        release.history(),
        vec![
            "old".to_string(),
            "ancient3".to_string(),
            "ancient2".to_string(),
            "ancient1".to_string()
        ],
        "the fixture's order is not the test's"
    );

    let steps = Recorder::new(&world.info.version);
    execute(&world, &options(&world, true), &steps).expect("the promotion should succeed");

    assert!(
        !release.dir_of("ancient1").exists(),
        "the oldest release survived"
    );
    for kept in [
        world.info.git_tag.clone(),
        "old".to_string(),
        "ancient3".to_string(),
        "ancient2".to_string(),
    ] {
        assert!(
            release.dir_of(&kept).is_dir(),
            "the prune removed releases/{kept}, which it had to keep: {:?}",
            release.history()
        );
    }
    // The live one and the rollback target, which is what the acceptance names.
    assert_eq!(
        release.link(),
        Some(Path::new(holosmith::release::RELEASES).join(&world.info.git_tag))
    );
    assert_eq!(
        release.older_than(&world.info.git_tag).as_deref(),
        Some("old")
    );
    assert!(
        release.live().is_some(),
        "the tree is not live after the prune"
    );
}

#[test]
fn a_promotion_whose_candidate_never_answers_prunes_nothing() {
    // The prune's place is the *success* ending: a promotion that rolled back leaves the tree an operator reads while
    // diagnosing it — and the rollback target in it — and a tree pruned before the health poll would take releases out
    // from under a promotion that then failed.
    let Some(mut world) = world("upgrade-prune-failed") else {
        eprintln!("SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote");
        return;
    };
    seed(&world, "ancient1", 1_000_000_000);
    seed(&world, "ancient2", 1_100_000_000);
    world.config.keep_releases = 2;
    let release = Release::of(&world.root);

    let mut steps = Recorder::new(&world.info.version);
    // An answer from a process older than the restart: the version is right and the uptime is not, so the candidate
    // is not live and the promotion rolls back.
    steps.health = (world.info.version.clone(), 3_600);
    execute(&world, &options(&world, true), &steps).expect_err("the candidate never became live");

    assert_eq!(
        release.history(),
        vec![
            world.info.git_tag.clone(),
            "old".to_string(),
            "ancient2".to_string(),
            "ancient1".to_string()
        ],
        "a failed promotion pruned the tree: {:?}",
        release.history()
    );
}

#[test]
fn a_promotion_that_skipped_the_review_is_refused_before_anything_is_asked() {
    let Some(world) = world("upgrade-unacknowledged") else {
        eprintln!("SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote");
        return;
    };
    let steps = Recorder::new(&world.info.version);
    let error = execute(&world, &options(&world, false), &steps)
        .expect_err("a promotion without the acknowledgement is refused");
    assert!(error.contains("review gate"), "{error}");
    assert!(
        steps.trace().is_empty(),
        "the machine was asked something before the gate was acknowledged: {:?}",
        steps.trace()
    );
    assert!(
        backups(&world).is_empty(),
        "a refused promotion wrote a backup"
    );
    assert_eq!(
        Release::of(&world.root).link(),
        Some(Path::new("releases/old").to_path_buf()),
        "a refused promotion switched `current`"
    );
}

#[test]
fn a_failed_smoke_run_stops_the_promotion_before_the_drain() {
    let Some(world) = world("upgrade-smoke") else {
        eprintln!("SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote");
        return;
    };
    let mut steps = Recorder::new(&world.info.version);
    steps.smoke = smoke::Outcome::Failed("the recorded turn held no tool call".to_string());
    let error = execute(&world, &options(&world, true), &steps)
        .expect_err("a failed smoke run is a refused promotion");
    assert!(error.contains("failed its smoke test"), "{error}");
    assert_eq!(
        steps.trace(),
        ["smoke"],
        "the drain ran after a failed smoke test"
    );
    assert!(backups(&world).is_empty());
    assert_eq!(
        Release::of(&world.root).link(),
        Some(Path::new("releases/old").to_path_buf())
    );

    // The other non-pass: a machine that cannot judge the candidate says so, and is not a verdict about it.
    let mut steps = Recorder::new(&world.info.version);
    steps.smoke = smoke::Outcome::CouldNotRun("no Docker daemon answers".to_string());
    let error = execute(&world, &options(&world, true), &steps)
        .expect_err("a machine that cannot judge it refuses too");
    assert!(error.contains("cannot judge the candidate"), "{error}");
    assert_eq!(steps.trace(), ["smoke"]);
}

#[test]
fn an_expired_or_already_running_drain_is_refused_before_the_backup() {
    let Some(world) = world("upgrade-drain") else {
        eprintln!("SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote");
        return;
    };
    for (answer, fact) in [
        (DrainAnswer::Expired, "ran out"),
        (DrainAnswer::Already, "already draining"),
        (DrainAnswer::Failed("no socket".to_string()), "no socket"),
    ] {
        let mut steps = Recorder::new(&world.info.version);
        steps.drain = answer;
        let error = execute(&world, &options(&world, true), &steps)
            .expect_err("a drain that did not finish is a refused promotion");
        assert!(error.contains(fact), "{fact}: {error}");
        // The two drain endings name the one thing that clears a drain — the restart it exists for — because
        // finishing does not clear it (`Control` has no other writer).
        if fact != "no socket" {
            assert!(
                error.contains(holosmith::deploy::UNIT_NAME),
                "the refusal does not name the restart that clears a drain: {error}"
            );
        }
        assert_eq!(
            steps.trace().last().map(String::as_str),
            Some("drain"),
            "the promotion went past the drain: {:?}",
            steps.trace()
        );
        assert!(
            backups(&world).is_empty(),
            "a refused drain wrote a backup: {:?}",
            backups(&world)
        );
        assert_eq!(
            Release::of(&world.root).link(),
            Some(Path::new("releases/old").to_path_buf()),
            "a refused drain switched `current`"
        );
    }
}

#[test]
fn a_backup_that_cannot_be_written_leaves_the_previous_release_live() {
    let Some(world) = world("upgrade-backup") else {
        eprintln!("SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote");
        return;
    };
    // A *file* where the backups directory goes: `create_dir_all` fails, so `VACUUM INTO` is never reached and the
    // promotion stops at the step that failed.
    std::fs::write(
        world.config.data_root.join(holosmith::backup::BACKUPS),
        b"not a directory",
    )
    .expect("the fixture is writable");
    let steps = Recorder::new(&world.info.version);
    let error = execute(&world, &options(&world, true), &steps)
        .expect_err("a promotion that cannot back up does not proceed");
    assert!(error.contains("backup failed"), "{error}");
    // A refusal after the drain owes the operator the state the drain left: one clause, and every later refusal has
    // it (`promote::draining`), because the machine keeps answering `503 draining` to creates until it is restarted.
    assert!(
        error.contains("draining until it is restarted"),
        "a refusal after the drain does not say the machine is draining: {error}"
    );
    assert_eq!(
        Release::of(&world.root).link(),
        Some(Path::new("releases/old").to_path_buf()),
        "a promotion that could not back up switched `current`"
    );
    assert!(
        !steps.trace().iter().any(|call| call == "restart"),
        "a promotion that could not back up restarted the service: {:?}",
        steps.trace()
    );
}

#[test]
fn a_candidate_that_never_answers_is_switched_back_and_says_which_ending_it_was() {
    let Some(world) = world("upgrade-rollback") else {
        eprintln!("SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote");
        return;
    };
    // The old release is a shell script, so its own `version --json` cannot be read — the rollback's second poll is
    // therefore the "not confirmed" ending, which is the loudest one and the one this arm is about.
    let mut steps = Recorder::new(&world.info.version);
    // An answer from a process older than the restart: the version is right and the uptime is not.
    steps.health = (world.info.version.clone(), 3_600);
    let error = execute(&world, &options(&world, true), &steps)
        .expect_err("a candidate that never answers is not live");
    assert!(
        error.contains("rolled back") || error.contains("confirmed"),
        "{error}"
    );
    assert_eq!(
        Release::of(&world.root).link(),
        Some(Path::new("releases/old").to_path_buf()),
        "the rollback did not put `current` back"
    );
    let trace = steps.trace();
    assert_eq!(
        trace
            .iter()
            .filter(|call| call.as_str() == "restart")
            .count(),
        2,
        "the rollback did not restart the service: {trace:?}"
    );
    // The candidate's release directory survives the rollback (it is the next attempt's), and the backup is kept.
    assert!(
        Release::of(&world.root)
            .binary(&world.info.git_tag)
            .is_file()
    );
    assert_eq!(backups(&world).len(), 1, "{:?}", backups(&world));
    // The previous release's *version* cannot be read from a shell script, so the ending is the unconfirmed one, and
    // the sentence names the unit and the backup — the two things an operator needs.
    assert!(error.contains(holosmith::deploy::UNIT_NAME), "{error}");
    let snapshot = world
        .config
        .data_root
        .join(holosmith::backup::BACKUPS)
        .join(&backups(&world)[0]);
    assert!(error.contains(&snapshot.display().to_string()), "{error}");
}

#[test]
fn a_candidate_whose_schema_is_older_than_the_databases_is_refused() {
    let Some(world) = world("upgrade-schema") else {
        eprintln!("SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote");
        return;
    };
    // A database from a newer release: its `schema_version` table records one more than this build's.
    let path = world.config.data_root.join(holosmith::db::DATABASE_FILE);
    let connection = rusqlite::Connection::open(&path).expect("the database opens");
    connection
        .execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, 'a later release', 0)",
            [holosmith::schema::SCHEMA_VERSION + 1],
        )
        .expect("a schema row is writable");
    drop(connection);

    let steps = Recorder::new(&world.info.version);
    let error = execute(&world, &options(&world, true), &steps)
        .expect_err("a candidate older than the database is refused");
    assert!(error.contains("older than the database"), "{error}");
    assert!(
        steps.trace().is_empty(),
        "the machine was asked something before the schema was compared: {:?}",
        steps.trace()
    );
    assert!(backups(&world).is_empty());
}

#[test]
fn a_candidate_with_the_right_schema_is_accepted_and_one_missing_its_digest_is_not() {
    let Some(world) = world("upgrade-digest") else {
        eprintln!("SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote");
        return;
    };
    // The digest file is the build's claim about its own bytes: a candidate whose file says something else is not the
    // build that was hashed, and that is refused before anything is asked.
    let steps = Recorder::new(&world.info.version);
    execute(&world, &options(&world, true), &steps).expect("the ordinary candidate is accepted");

    // Now the same candidate with a digest file that names another build.
    std::fs::write(
        world.dir.file("holosmith.sha256"),
        format!("{}  holosmith\n", "0".repeat(64)),
    )
    .expect("the digest file is writable");
    let steps = Recorder::new(&world.info.version);
    let error = execute(&world, &options(&world, true), &steps)
        .expect_err("a candidate whose digest does not match its file is refused");
    assert!(error.contains("not the build"), "{error}");
    assert!(steps.trace().is_empty());
}

/// [`Expectation`] is what an image's labels are compared with, and a promotion must compare the *candidate's*: this
/// asserts the one field a `version --json` reader could get wrong.
#[test]
fn the_expectation_a_promotion_builds_carries_the_candidates_values() {
    let Some(world) = world("upgrade-expectation") else {
        eprintln!("SKIP tests/upgrade.rs: no dist/<triple>/holosmith to promote");
        return;
    };
    let digest = images::digest(&world.candidate).expect("the candidate is readable");
    let expectation = Expectation::of_release(Kind::Proxy, &world.config, &world.info, &digest);
    assert_eq!(expectation.version, world.info.version);
    assert_eq!(expectation.git_tag, world.info.git_tag);
    assert_eq!(expectation.pi_version, world.info.pi_version);
    assert_eq!(expectation.binary, digest);
    assert_eq!(
        expectation.tag(),
        format!("holosmith-proxy:{}", world.info.version)
    );
}
