//! `holosmith update`: the gate the candidate tree carries, then the promotion that gate proves (ADR-0110).
//!
//! The command's two hard halves — the gate and the pipeline — are measured where each already is (the gate by
//! `scripts/verify.sh`'s container cases, the pipeline by `tests/upgrade.rs`). What is new, and what every test here is
//! about, is the **composition**: that the verification command is the candidate tree's own script rather than one
//! beside this binary; that its argv and environment are what ADR-0110 D5 decided; that a gate which did not verify
//! asks the machine nothing; that each of its exit codes keeps the meaning its wrapper's header gives it; and that a
//! green gate promotes the artifact that gate's own build phase wrote.
//!
//! **No test here runs a gate for real.** The fixture tree is a git repository on `master` whose
//! `scripts/verify-in-container.sh` records its argv, its environment and its working directory to a file and then
//! exits with the code the test names — which is exactly the seam the ticket asks for: the command reads the
//! candidate's own `scripts/`, so a fixture tree is a gate, and a test never spends forty minutes proving a tree.
//!
//! The promotion half needs a real artifact (`dist/<triple>/holosmith`, a static ELF of this machine with a
//! `version --json`), so the whole of it is skipped with one printed line on a tree that has not been built — the
//! convention `tests/images.rs` and `tests/upgrade.rs` established.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Duration;

use holosmith::commands::update;
use holosmith::config::Config;
use holosmith::deploy::Root;
use holosmith::http::health::Answer;
use holosmith::images::{self, Log};
use holosmith::pass::{self, Options};
use holosmith::promote::{self, DrainAnswer, Steps};
use holosmith::release::Release;
use holosmith::smoke;
use serde_json::Value;

mod support;
use support::{TempDir, git, repository_on};

/// One recorded call, in the order the pipeline made it.
type Trace = Vec<String>;

/// The machine, as a recorder: every question appends its name, and every answer is the test's.
///
/// A pass of the gate that never reaches the promotion must leave this trace **empty**, which is how "a failed gate
/// refuses before anything is asked of the machine" becomes a measurement rather than a claim about the order of two
/// `println!`s.
struct Recorder {
    trace: Mutex<Trace>,
    /// What `/api/health` answers: `(version, uptime_s)` — an uptime of 0 is a process younger than the restart.
    health: (String, u64),
}

impl Recorder {
    fn new(version: &str) -> Self {
        Self {
            trace: Mutex::new(Vec::new()),
            health: (version.to_string(), 0),
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
        smoke::Outcome::Passed
    }

    fn images(&self) -> &dyn images::Images {
        self
    }

    fn drain(&self, _admin_dir: &Path, _wait: Duration) -> DrainAnswer {
        self.record("drain");
        DrainAnswer::Drained
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
            draining: false,
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
        // Nothing built yet, so both images are outstanding and both are built from the candidate's bytes.
        Ok(None)
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

/// One machine and one candidate tree: a clean checkout on `master`, its own gate, a data root with a database, and an
/// install tree whose `releases/old` is live.
struct World {
    /// Kept for its drop: the scratch tree, its checkout and its report all live under it.
    _dir: TempDir,
    config: Config,
    root: Root,
    /// `[update] source`: the fixture checkout.
    source: PathBuf,
    /// Where the fixture gate records what it was given.
    log: PathBuf,
    /// The file `--report` names.
    report: PathBuf,
}

/// A world whose gate exits `code` and prints two `SKIP ` lines.
fn world(label: &str, code: i32) -> World {
    let dir = TempDir::new(label);
    let source = dir.file("source");
    repository_on(&source, "master");
    // The gate's own writes and the artifact are ignored, so a tree the gate has run in is still clean — which is
    // ADR-0110 D3's precondition read from the other side.
    std::fs::write(source.join(".gitignore"), "dist/\ntarget/\n")
        .expect("the ignore file is writable");
    let log = dir.file("gate.log");
    let script = source.join("scripts").join("verify-in-container.sh");
    std::fs::create_dir_all(source.join("scripts")).expect("the scripts directory is creatable");
    std::fs::write(&script, wrapper(&log, code)).expect("the fixture gate is writable");
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("the fixture gate is executable");
    git(&source, &["add", "-A"]);
    git(
        &source,
        &["commit", "--quiet", "-m", "the tree and its gate"],
    );

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
    let mut config = config;
    config.update.source = Some(source.clone());

    let root = Root::resolve(Some(dir.file("root")), Some(dir.file("holosmith.toml")));
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

    let report = dir.file("report.json");
    World {
        _dir: dir,
        config,
        root,
        source,
        log,
        report,
    }
}

/// The fixture gate: it records its argv, the image it was given and its working directory, prints a line of its own
/// and two `SKIP `s, and exits with `code`.
///
/// The `SKIP ` lines are `scripts/verify.sh`'s shape at column 0, which is the count a pass of the gate reports.
fn wrapper(log: &Path, code: i32) -> String {
    format!(
        "#!/bin/sh\n\
         printf 'argc %s\\n' \"$#\" > '{log}'\n\
         printf 'args %s\\n' \"$*\" >> '{log}'\n\
         printf 'image %s\\n' \"${{HOLOSMITH_VERIFY_IMAGE-<unset>}}\" >> '{log}'\n\
         printf 'cwd %s\\n' \"$PWD\" >> '{log}'\n\
         printf 'case_thing: the gate says a line of its own\\n'\n\
         printf 'SKIP case_a: no Docker daemon answers\\n'\n\
         printf 'SKIP case_b: no image pi-agent:local (HOLOSMITH_TEST_IMAGE names another)\\n'\n\
         exit {code}\n",
        log = log.display()
    )
}

impl World {
    /// Run one pass with `steps` and read its report back: the report is written for every ending, so a test that
    /// wants the sentence a refusal carries reads the file a job's caller would.
    fn run(&self, steps: &Recorder) -> (ExitCode, Value) {
        let code = pass::run(
            &self.config,
            &self.root,
            &Options {
                report: Some(self.report.clone()),
                wait: Duration::from_millis(1),
            },
            steps,
            &holosmith::deploy::Tools::default(),
        );
        let text = std::fs::read_to_string(&self.report)
            .expect("a run with --report writes one for every ending");
        (
            code,
            serde_json::from_str(&text).expect("the report is one JSON object"),
        )
    }

    /// What the fixture gate recorded about its own invocation.
    fn recorded(&self) -> String {
        std::fs::read_to_string(&self.log)
            .expect("the fixture gate recorded its argv and environment")
    }
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

/// The sentence the report's `detail` carries, for a run that did not end live.
fn detail(report: &Value) -> &str {
    report["detail"]
        .as_str()
        .expect("an ending that is not live carries a sentence")
}

/// The report's `steps` as `"<step> <ending>"`, which is the pair a caller reads to see how far a run got and what
/// stopped it.
fn step_list(report: &Value) -> Vec<String> {
    report["steps"]
        .as_array()
        .expect("a report's steps are a list")
        .iter()
        .map(|step| {
            format!(
                "{} {}",
                step["step"].as_str().expect("a step has a name"),
                step["ending"].as_str().expect("a step has an ending")
            )
        })
        .collect()
}

#[test]
fn the_gate_is_the_candidate_trees_own_script_in_the_running_releases_image() {
    let world = world("update-gate", 0);
    let steps = Recorder::new("0.1.0");
    let (code, report) = world.run(&steps);

    // The argv, the environment and the directory: the wrapper is the candidate's, it is given no argument at all
    // (a full gate, never `--no-build`), and the image is the one the running release deployed.
    let recorded = world.recorded();
    assert!(
        recorded.contains("argc 0\n"),
        "the gate was given arguments it does not take: {recorded}"
    );
    assert!(
        recorded.contains(&format!("image {}\n", images::agent_image())),
        "the gate did not run in the running release's image: {recorded}"
    );
    assert!(
        recorded.contains(&format!("cwd {}\n", world.source.display())),
        "the gate did not run in the candidate tree: {recorded}"
    );

    // A green gate that left no artifact is a refusal naming the two files and the command that writes them — and
    // the machine was asked nothing, because there was nothing to promote.
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(report["state"], "refused");
    assert_eq!(report["gate"]["exit_code"], 0);
    assert_eq!(
        report["gate"]["command"],
        script(&world).display().to_string()
    );
    assert_eq!(
        report["gate"]["skips"], 2,
        "the report does not carry the SKIP count"
    );
    assert!(report["artifact"].is_null());
    assert_eq!(
        step_list(&report),
        ["candidate resolved", "gate verified", "artifact refused"],
        "the report's steps do not say what the green gate left undone"
    );
    let said = detail(&report);
    assert!(said.contains("no artifact"), "{said}");
    assert!(said.contains("scripts/build.sh"), "{said}");
    assert!(said.contains("holosmith.sha256"), "{said}");
    assert!(
        steps.trace().is_empty(),
        "the promotion ran without an artifact: {:?}",
        steps.trace()
    );
}

#[test]
fn a_red_gate_asks_the_machine_nothing_at_all() {
    let world = world("update-red", 1);
    let steps = Recorder::new("0.1.0");
    let (code, report) = world.run(&steps);

    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(report["state"], "refused");
    assert_eq!(report["gate"]["exit_code"], 1);
    assert_eq!(
        step_list(&report),
        ["candidate resolved", "gate refused"],
        "the report's steps do not say where the run stopped"
    );
    let said = detail(&report);
    assert!(said.contains("failed it"), "{said}");
    // The gate's own last lines travel with the refusal, which is where its `FAIL` line is.
    assert!(
        said.contains("SKIP case_b"),
        "the gate's own output is not in the refusal: {said}"
    );

    // The whole point of the order: no smoke run, no drain, no backup, no switch.
    assert!(
        steps.trace().is_empty(),
        "a red gate asked the machine something: {:?}",
        steps.trace()
    );
    assert!(backups(&world).is_empty(), "a red gate wrote a backup");
    assert_eq!(
        Release::of(&world.root).link(),
        Some(Path::new("releases/old").to_path_buf()),
        "a red gate switched `current`"
    );
}

#[test]
fn every_gate_exit_code_keeps_the_meaning_its_wrappers_header_gives_it() {
    // The header's table: 1 is the tree's verdict, and 2, 3, 125, 126, 127 and 137 are not verdicts at all. The gate
    // step's own ending names the code's meaning — 3 says `stale crate set` where the run's state says
    // `could-not-verify`, because the image is the subject and not the tree.
    for (code, state, needle, ending) in [
        (1, "refused", "failed it", "refused"),
        (
            2,
            "could-not-verify",
            "refused to start",
            "could not verify",
        ),
        (3, "could-not-verify", "`Cargo.lock`", "stale crate set"),
        (
            125,
            "could-not-verify",
            "`docker run` itself failed",
            "could not verify",
        ),
        (
            126,
            "could-not-verify",
            "not executable",
            "could not verify",
        ),
        (127, "could-not-verify", "not there", "could not verify"),
        (137, "could-not-verify", "kernel killed", "could not verify"),
    ] {
        let world = world(&format!("update-exit-{code}"), code);
        let steps = Recorder::new("0.1.0");
        let (status, report) = world.run(&steps);

        assert_eq!(
            status,
            ExitCode::FAILURE,
            "exit {code} is not a live candidate"
        );
        assert_eq!(
            report["state"], state,
            "exit {code} reads as {}",
            report["state"]
        );
        assert_eq!(report["gate"]["exit_code"], code);
        assert_eq!(
            step_list(&report),
            vec!["candidate resolved".to_string(), format!("gate {ending}")],
            "exit {code}: the report's steps do not carry the gate's ending"
        );
        let said = detail(&report);
        assert!(said.contains(needle), "exit {code}: {said}");
        assert!(
            said.contains("no smoke run"),
            "exit {code} does not say the machine was asked nothing: {said}"
        );
        assert!(
            steps.trace().is_empty(),
            "exit {code} reached the machine: {:?}",
            steps.trace()
        );
    }

    // Exit 3 is a **host act** and not a broken registry: a candidate whose `Cargo.lock` moves the crate set cannot be
    // verified in the running release's image, and the repair is to build and deploy that image from the candidate.
    let world = world("update-exit-3-host-act", 3);
    let (_, report) = world.run(&Recorder::new("0.1.0"));
    let said = detail(&report);
    assert!(said.contains("host act"), "{said}");
    assert!(said.contains("cargo fetch --locked"), "{said}");
    assert!(
        said.contains("build and deploy"),
        "the refusal does not name the repair: {said}"
    );
    assert!(
        !said.contains("not found in the registry index"),
        "the refusal repeats the misreading it exists to prevent: {said}"
    );
}

/// `--report` carries the endings that happen before a pass begins, too. A config that cannot be loaded is exit 1 and
/// no step at all, and a caller polling the file cannot tell that apart from a job that crashed.
#[test]
fn a_config_that_cannot_be_loaded_still_writes_its_refusal_to_the_report() {
    let dir = TempDir::new("update-bad-config");
    let report = dir.file("report.json");
    let invalid = dir.file("invalid.toml");
    std::fs::write(&invalid, "owner = \"two words\"\n").expect("the fixture config is writable");
    let absent = dir.file("absent.toml");

    // Both arms of the load: a file that is there and refuses, and one that is not there at all.
    for (config, needle) in [(invalid, "owner"), (absent, "no such file")] {
        let code = update::run(update::UpdateArgs {
            config: Some(config),
            report: Some(report.clone()),
            wait: Duration::from_millis(1),
            root: Some(dir.file("root")),
        });

        assert_eq!(
            code,
            ExitCode::FAILURE,
            "a config that cannot be loaded is never a live candidate"
        );
        let value: Value = serde_json::from_str(
            &std::fs::read_to_string(&report).expect("--report is written for every ending"),
        )
        .expect("the report is one JSON object");
        assert_eq!(value["state"], "refused");
        assert!(
            step_list(&value).is_empty(),
            "a run that reached no step reported one: {value}"
        );
        assert!(value["candidate"].is_null(), "{value}");
        assert!(detail(&value).contains(needle), "{}", detail(&value));
    }
}

#[test]
fn a_machine_with_no_candidate_never_reaches_the_gate() {
    let mut world = world("update-no-candidate", 0);
    world.config.update.source = None;
    let steps = Recorder::new("0.1.0");
    let (code, report) = world.run(&steps);

    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(report["state"], "refused");
    assert!(
        detail(&report).contains("has no candidate"),
        "{}",
        detail(&report)
    );
    assert!(report["gate"].is_null());
    assert_eq!(
        step_list(&report),
        ["candidate refused"],
        "the report does not say the candidate was what refused"
    );
    assert!(
        !world.log.exists(),
        "the gate ran for a machine with no candidate"
    );
    assert!(steps.trace().is_empty());
}

#[test]
fn a_checkout_with_no_gate_of_its_own_is_refused_rather_than_verified_elsewhere() {
    let world = world("update-no-wrapper", 0);
    std::fs::remove_file(script(&world)).expect("the fixture gate is removable");
    git(&world.source, &["add", "-A"]);
    git(
        &world.source,
        &["commit", "--quiet", "-m", "the gate is gone"],
    );

    let (code, report) = world.run(&Recorder::new("0.1.0"));
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(report["state"], "refused");
    assert_eq!(
        step_list(&report),
        ["candidate resolved", "candidate refused"],
        "the report does not say the tree was resolved and then refused for its wrapper"
    );
    let said = detail(&report);
    assert!(said.contains("verify-in-container.sh"), "{said}");
    assert!(
        said.contains("checkout of this repository"),
        "the refusal does not say what the tree has to be: {said}"
    );
}

/// A green gate promotes the artifact its own build phase wrote — and that artifact is the shipped one, because a
/// promotion's first checks are about bytes: a static ELF of this machine, a `.sha256` beside it, and its own
/// `version --json`.
#[test]
fn a_green_gate_promotes_the_artifact_its_build_phase_wrote() {
    let Some(artifact) = artifact() else {
        eprintln!(
            "SKIP tests/update.rs: no dist/<triple>/holosmith to promote (scripts/build.sh produces it; the gate's \
             case_upgrade and case_smoke measure the same pipeline end to end)"
        );
        return;
    };
    let info = promote::ask_version(&artifact).expect("the artifact answers `version --json`");
    let world = world("update-promote", 0);
    // What the gate's build phase leaves: the artifact and its digest, in `dist/<this machine's triple>/`.
    let triple = holosmith::builder::default_target().expect("this binary is a release target");
    let directory = world.source.join("dist").join(triple);
    std::fs::create_dir_all(&directory).expect("the dist directory is creatable");
    let candidate = directory.join(holosmith::builder::BINARY);
    std::fs::copy(&artifact, &candidate).expect("the artifact is copyable");
    let digest = images::digest(&candidate).expect("the candidate is readable");
    std::fs::write(
        directory.join(holosmith::builder::DIGEST),
        format!("{digest}  holosmith\n"),
    )
    .expect("the digest file is writable");

    // The tree is still the candidate's: `dist/` is ignored, so the build the gate ran left it clean.
    assert_eq!(
        holosmith::candidate::resolve(&world.config)
            .expect("a clean master is a candidate")
            .source,
        world.source,
        "the fixture is not the candidate it was made to be"
    );

    let steps = Recorder::new(&info.version);
    let (code, report) = world.run(&steps);

    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "the promotion did not end live: {report}"
    );
    assert_eq!(report["state"], "live");
    assert_eq!(report["artifact"]["path"], candidate.display().to_string());
    assert_eq!(report["artifact"]["digest"], digest);
    // The candidate is the **tree's** commit and tag, not the artifact's: `scripts/build.sh` bakes its own
    // `git describe` into the binary, and this fixture checkout's commit is not the one the shipped artifact was
    // built from. They are two different facts and the report says each in its own place.
    assert_eq!(
        report["candidate"]["tag"],
        holosmith::candidate::resolve(&world.config)
            .expect("a clean master is a candidate")
            .tag,
        "the report's candidate is not the checkout `[update] source` names"
    );
    assert_eq!(report["gate"]["skips"], 2);
    assert_eq!(
        step_list(&report),
        ["candidate resolved", "gate verified", "promotion live"],
        "the report's steps do not say how the run got to live"
    );

    // The pipeline ran in the spec's order, on the artifact the gate left.
    let trace = steps.trace();
    assert_eq!(
        trace,
        [
            "smoke".to_string(),
            format!("labels:holosmith-proxy:{}", info.version),
            format!("labels:agent-image:{}", info.version),
            format!("build:holosmith-proxy:{}", info.version),
            format!("build:agent-image:{}", info.version),
            "drain".to_string(),
            "restart".to_string(),
            "health:0".to_string(),
        ],
        "the promotion did not run in the spec's order: {trace:?}"
    );
    // And the machine's state followed: the release directory exists, the backup was taken, `current` names it.
    let release = Release::of(&world.root);
    assert_eq!(
        release.link(),
        Some(Path::new(holosmith::release::RELEASES).join(&info.git_tag)),
        "`current` does not point at the candidate's release"
    );
    assert!(release.binary(&info.git_tag).is_file());
    assert_eq!(backups(&world).len(), 1, "{:?}", backups(&world));
}

/// The gate's own path, as the report and a refusal name it.
fn script(world: &World) -> PathBuf {
    world.source.join("scripts").join("verify-in-container.sh")
}
