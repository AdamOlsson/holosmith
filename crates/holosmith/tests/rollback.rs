//! `holosmith rollback`: the order, the target and the two endings (HS-079, spec item 10, ADR-0083).
//!
//! Acceptance bullet 1 is an order — "drains first, points `current` at the previous release, restarts, and verifies
//! health" — and only one of those steps is a child (`systemctl`), so the verb is driven here through
//! [`promote::Steps`], the seam the promotion's own pipeline is measured with: the trace proves the order, that the
//! switch happened **before** the restart, and that a rollback asks the machine *nothing* a promotion's first half asks
//! (no smoke run, no image build).
//!
//! The releases hold the shipped artifact (`dist/<triple>/holosmith`, the fixture `tests/images.rs` and
//! `tests/upgrade.rs` also use), because a rollback's target is read through its own `version --json`: a directory
//! holding a fake file cannot answer, and the schema refusal cannot be measured over one. Without the artifact every
//! test that switches prints one `SKIP` line — the gate's `case_rollback` measures the same arms end to end.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use holosmith::config::Config;
use holosmith::deploy::Target;
use holosmith::http::health::Answer;
use holosmith::images::{self, Log};
use holosmith::promote::{self, DrainAnswer, Steps};
use holosmith::release::Release;
use holosmith::rollback::{self, Options};

mod support;
use support::TempDir;

/// The window every test polls with: short enough that a poll which gives up does so now.
const WINDOW: Duration = Duration::from_millis(60);

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

/// One recorded call, in the order the verb made it.
type Trace = Vec<String>;

/// The machine, as a recorder: every question appends its name and every answer is the test's.
///
/// `restart` records **what `current` names at that moment**, which is how the acceptance's order becomes a
/// measurement rather than a print statement: a verb that restarted first and switched afterwards would show the old
/// target there.
struct Recorder {
    trace: Mutex<Trace>,
    /// The `current` symlink, read during the restart.
    link: PathBuf,
    /// What the drain answers.
    drain: DrainAnswer,
    /// What `/api/health` answers: `(version, uptime_s)`.
    health: (String, u64),
    /// What `systemctl restart` answers.
    restarted: Result<(), String>,
}

impl Recorder {
    fn new(link: &Path, version: &str) -> Self {
        Self {
            trace: Mutex::new(Vec::new()),
            link: link.to_path_buf(),
            drain: DrainAnswer::Drained,
            health: (version.to_string(), 0),
            restarted: Ok(()),
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
    fn smoke(&self, _candidate: &Path, _root: &Path) -> holosmith::smoke::Outcome {
        // A rollback must never ask this: it is the promotion's go/no-go, and a rollback has no candidate.
        self.record("smoke");
        holosmith::smoke::Outcome::Passed
    }

    fn images(&self) -> &dyn images::Images {
        self
    }

    fn drain(&self, _admin_dir: &Path, _wait: Duration) -> DrainAnswer {
        self.record("drain");
        self.drain.clone()
    }

    fn restart(&self, _systemctl: &Path) -> Result<(), String> {
        let target = std::fs::read_link(&self.link)
            .map(|target| target.display().to_string())
            .unwrap_or_else(|_| "no current".to_string());
        self.record(format!("restart -> {target}"));
        self.restarted.clone()
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
        // A rollback builds nothing, so this answer is only here to satisfy the seam.
        Ok("amd64".to_string())
    }

    fn labels(&self, _image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        Ok(None)
    }

    fn build(&self, _image: &str, _archive: &[u8], _arguments: &[String]) -> Result<Log, String> {
        self.record("build");
        Err("a rollback builds no image".to_string())
    }
}

/// The tree one test rolls back in: a config, a database, three releases and a unit.
struct World {
    /// Held for its `Drop`: the tree under it is this test's, and removing it at the end is the cleanup.
    _dir: TempDir,
    config: Config,
    target: Target,
    info: holosmith::version::VersionInfo,
    /// The release tags, **oldest first**: `one`, `two`, `three`, with `current` on the newest.
    tags: Vec<String>,
}

/// The three releases a rollback chooses between, named for their place rather than for a version: a release
/// directory's name is a tag, and every copy of one artifact reports the same `git_tag` — which is exactly the
/// ambiguity `--to` and the tree's own order are for.
const TAGS: [&str; 3] = ["one", "two", "three"];

fn world(label: &str) -> Option<World> {
    let artifact = artifact()?;
    let dir = TempDir::new(label);
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

    let target = Target::resolve(Some(dir.file("root")));
    let release = Release::of(&target);
    let info = promote::ask_version(&artifact).expect("the artifact answers `version --json`");
    let digest = images::digest(&artifact).expect("the artifact is readable");
    // Oldest first, each written one second after the one before it, so the tree's order is the test's rather than the
    // clock's — and `current` on the last of them.
    for (at, tag) in TAGS.iter().enumerate() {
        release
            .write(
                tag,
                &std::fs::read(&artifact).expect("the artifact is readable"),
                &digest,
            )
            .expect("the release is writable");
        let when = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + at as u64);
        std::fs::File::open(release.dir_of(tag))
            .expect("the release directory is open")
            .set_modified(when)
            .expect("the release's mtime is set");
    }
    release.switch("three").expect("the newest is live");
    // The unit a deploy writes, through `current`: `deploy::unit_text` is the one writer of these bytes.
    let unit = target.unit_path();
    std::fs::create_dir_all(unit.parent().expect("the unit has a directory"))
        .expect("the unit's directory is creatable");
    std::fs::write(
        &unit,
        holosmith::deploy::unit_text(
            &release.current().join(holosmith::release::BINARY),
            target.privileged(),
            config.role,
        ),
    )
    .expect("the unit is writable");

    Some(World {
        _dir: dir,
        config,
        target,
        info,
        tags: TAGS.iter().map(|tag| tag.to_string()).collect(),
    })
}

fn options(to: Option<&str>) -> Options {
    Options {
        to: to.map(str::to_string),
        wait: Duration::from_millis(1),
        window: WINDOW,
    }
}

fn execute(world: &World, options: &Options, steps: &Recorder) -> Result<(), String> {
    rollback::execute(
        &world.config,
        &world.target,
        options,
        steps,
        &holosmith::deploy::Tools::default(),
    )
}

fn link(world: &World) -> Option<PathBuf> {
    Release::of(&world.target).link()
}

/// A test's recorder, pointed at the tree it rolls back in.
fn recorder(world: &World) -> Recorder {
    Recorder::new(
        &world.target.opt_dir().join(holosmith::release::CURRENT),
        &world.info.version,
    )
}

/// The one `SKIP` line a tree without an artifact prints, instead of failing for the wrong reason.
macro_rules! world_or_skip {
    ($label:expr) => {
        match world($label) {
            Some(world) => world,
            None => {
                eprintln!(
                    "SKIP tests/rollback.rs: no dist/<triple>/holosmith to roll back to (scripts/build.sh produces \
                     it; the gate's case_rollback measures the same arms end to end)"
                );
                return;
            }
        }
    };
}

#[test]
fn a_rollback_drains_switches_restarts_and_polls_in_that_order() {
    let world = world_or_skip!("rollback-order");
    let steps = recorder(&world);
    execute(&world, &options(None), &steps).expect("the rollback of a healthy release succeeds");
    // The order, and the switch **before** the restart: the acceptance's first bullet, measured.
    assert_eq!(
        steps.trace(),
        vec![
            "drain".to_string(),
            "restart -> releases/two".to_string(),
            "health:0".to_string()
        ],
        "a rollback must drain, switch, then restart, and must ask nothing a promotion's first half asks"
    );
    assert_eq!(link(&world), Some(PathBuf::from("releases/two")));
    // The release it came from is still there: a rollback moves the link, it deletes nothing.
    assert!(Release::of(&world.target).binary("three").is_file());
    // The unit already went through `current`, so nothing was rewritten: the deploy's bytes are the rollback's.
    let unit = std::fs::read_to_string(world.target.unit_path()).expect("the unit is readable");
    assert_eq!(
        holosmith::release::unit_executable(&unit),
        Some(
            Release::of(&world.target)
                .current()
                .join(holosmith::release::BINARY)
        )
    );
}

#[test]
fn the_default_target_is_the_release_behind_the_live_one_and_to_names_another() {
    let world = world_or_skip!("rollback-choice");
    // The tree's own order: three is live, so two is one step back — not `one`, and not by name.
    let steps = recorder(&world);
    execute(&world, &options(None), &steps).expect("the default target is a release in the tree");
    assert_eq!(link(&world), Some(PathBuf::from("releases/two")));

    // `--to` names any other release: here, two steps back from where the tree started.
    let steps = recorder(&world);
    execute(&world, &options(Some("one")), &steps).expect("a named release is switched to");
    assert_eq!(link(&world), Some(PathBuf::from("releases/one")));
    // …and forward again is the same switch, which is the way back from a mistaken rollback.
    let steps = recorder(&world);
    execute(&world, &options(Some("three")), &steps)
        .expect("a newer release is also a legal target");
    assert_eq!(link(&world), Some(PathBuf::from("releases/three")));
}

#[test]
fn a_tree_with_one_release_is_refused_and_asks_the_machine_nothing() {
    let world = world_or_skip!("rollback-only");
    // Remove the two releases behind the live one, so `current` names the oldest — and the only — release.
    for tag in ["one", "two"] {
        std::fs::remove_dir_all(Release::of(&world.target).dir_of(tag))
            .expect("a release is removable");
    }
    let steps = recorder(&world);
    let error =
        execute(&world, &options(None), &steps).expect_err("one release has no previous release");
    assert!(error.contains("oldest release this tree holds"), "{error}");
    assert!(
        steps.trace().is_empty(),
        "the machine was asked something before the tree was read: {:?}",
        steps.trace()
    );
    assert_eq!(link(&world), Some(PathBuf::from("releases/three")));
}

#[test]
fn a_target_older_than_the_database_is_refused_before_the_drain() {
    let world = world_or_skip!("rollback-schema");
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

    let steps = recorder(&world);
    let error = execute(&world, &options(None), &steps)
        .expect_err("an older release cannot read this database");
    assert!(error.contains("older than the database"), "{error}");
    // The acceptance's "with a clear explanation": the pre-line that would refuse it later, and the rule that makes a
    // rollback safe in the first place.
    assert!(error.contains("doctor --quick"), "{error}");
    assert!(error.contains("Nothing was changed"), "{error}");
    assert!(
        steps.trace().is_empty(),
        "the machine was asked something before the schema was compared: {:?}",
        steps.trace()
    );
    assert_eq!(
        link(&world),
        Some(PathBuf::from("releases/three")),
        "a refused rollback moved the link"
    );
}

#[test]
fn a_drain_that_expires_switches_nothing_and_says_the_machine_is_draining() {
    let world = world_or_skip!("rollback-expired");
    let mut steps = recorder(&world);
    steps.drain = DrainAnswer::Expired;
    let error =
        execute(&world, &options(None), &steps).expect_err("an expired drain refuses the rollback");
    assert!(error.contains("draining until it is restarted"), "{error}");
    assert!(error.contains("releases/three is still live"), "{error}");
    assert_eq!(steps.trace(), vec!["drain".to_string()]);
    assert_eq!(link(&world), Some(PathBuf::from("releases/three")));
}

#[test]
fn a_rollback_joins_a_drain_somebody_else_started() {
    let world = world_or_skip!("rollback-joining");
    // A promotion refuses this answer; a rollback must not — a machine left draining is the machine it exists for.
    let mut steps = recorder(&world);
    steps.drain = DrainAnswer::Already;
    execute(&world, &options(None), &steps).expect("a rollback joins a drain in progress");
    assert_eq!(
        steps.trace(),
        vec![
            "drain".to_string(),
            "restart -> releases/two".to_string(),
            "health:0".to_string()
        ],
        "a joined drain must not stop the rollback"
    );
    assert_eq!(link(&world), Some(PathBuf::from("releases/two")));
}

#[test]
fn a_rollback_that_fails_its_own_health_check_is_loud_and_names_the_next_step() {
    let world = world_or_skip!("rollback-unanswered");
    let mut steps = recorder(&world);
    // An answer from a process older than the restart: the version is right and the uptime is not.
    steps.health = (world.info.version.clone(), 3_600);
    let error =
        execute(&world, &options(None), &steps).expect_err("an unanswered rollback is not live");
    // The four things the acceptance's fourth bullet asks for.
    assert!(error.contains("systemctl status"), "{error}");
    assert!(error.contains("journalctl -u"), "{error}");
    assert!(
        error.contains("`current` still names it"),
        "the sentence does not say which release the link is on: {error}"
    );
    assert!(error.contains("--to three"), "no way back forward: {error}");
    assert!(error.contains("doctor --quick"), "{error}");
    // The link is **not** put back: a rollback that guesses twice is one nobody can reason about.
    assert_eq!(link(&world), Some(PathBuf::from("releases/two")));
    // The sequence, and nothing else: the poll asked as often as its window allowed, and the machine was asked no
    // question a promotion would ask.
    let trace = steps.trace();
    assert_eq!(
        &trace[..2],
        &["drain".to_string(), "restart -> releases/two".to_string()]
    );
    assert!(
        trace[2..].iter().all(|call| call == "health:3600"),
        "a poll that never answers asked something else: {trace:?}"
    );
}

#[test]
fn a_restart_that_fails_is_named_first_and_polls_nothing() {
    let world = world_or_skip!("rollback-restart");
    let mut steps = recorder(&world);
    steps.restarted = Err("`systemctl restart holosmith.service` answered 1 (nope)".to_string());
    let error = execute(&world, &options(None), &steps).expect_err("a failed restart is not live");
    assert!(error.contains("The restart itself failed first"), "{error}");
    assert!(error.contains("answered 1 (nope)"), "{error}");
    assert_eq!(
        steps.trace(),
        vec!["drain".to_string(), "restart -> releases/two".to_string()],
        "a restart that never happened must not be polled for"
    );
}

#[test]
fn a_unit_that_does_not_go_through_current_is_rewritten_and_a_missing_one_is_refused() {
    let world = world_or_skip!("rollback-unit");
    // A unit an operator edited by hand (or a tree copied onto a machine): it names a raw binary, so the switch would
    // not reach the restart. The rollback fixes it with the same bytes `deploy`'s step 5 renders.
    std::fs::write(
        world.target.unit_path(),
        "[Unit]\nDescription=Holosmith server\n\n[Service]\nExecStart=/usr/local/bin/holosmith serve\n",
    )
    .expect("the unit is writable");
    let steps = recorder(&world);
    execute(&world, &options(None), &steps).expect("the rollback still takes effect");
    let unit = std::fs::read_to_string(world.target.unit_path()).expect("the unit is readable");
    assert_eq!(
        holosmith::release::unit_executable(&unit),
        Some(
            Release::of(&world.target)
                .current()
                .join(holosmith::release::BINARY)
        ),
        "the unit was not made to go through `current`: {unit}"
    );

    // No unit at all: a machine that has not been deployed, refused before the drain.
    std::fs::remove_file(world.target.unit_path()).expect("the unit is removable");
    let steps = recorder(&world);
    let error =
        execute(&world, &options(None), &steps).expect_err("a machine with no unit is refused");
    assert!(error.contains("has to have been deployed"), "{error}");
    assert!(steps.trace().is_empty(), "{:?}", steps.trace());
}

#[test]
fn the_target_directory_has_to_hold_a_binary_this_machine_can_run() {
    let world = world_or_skip!("rollback-binary");
    // A release directory whose binary is not a program: `version --json` cannot be asked, so the rollback refuses
    // before it drains anything — and the `[tags]` list is not a substitute for asking the binary.
    std::fs::write(
        Release::of(&world.target).binary("two"),
        b"#!/bin/sh\nexit 9\n",
    )
    .expect("the release's binary is replaceable");
    let steps = recorder(&world);
    let error = execute(&world, &options(None), &steps)
        .expect_err("a release that cannot answer is refused");
    assert!(error.contains("has to hold a `holosmith`"), "{error}");
    assert!(steps.trace().is_empty(), "{:?}", steps.trace());
    assert_eq!(link(&world), Some(PathBuf::from("releases/three")));
    // Nothing in the tree changed either: the directory is still there for the operator to look at.
    assert!(Release::of(&world.target).dir_of("two").is_dir());
    // And the fixture really was a release the tree counted before it was broken.
    assert_eq!(
        world.tags,
        TAGS.iter().map(|tag| tag.to_string()).collect::<Vec<_>>()
    );
}
