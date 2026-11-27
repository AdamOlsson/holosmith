//! `POST /api/update` and `GET /api/update` over a real route table, and the job that stands between them (ADR-0110).
//!
//! `tests/update.rs` measures the **command**: the candidate, the gate's argv and environment, the exit-code
//! translation and the report each ending writes. `http/update.rs`'s unit tests measure the refusals in process. What
//! neither can show is the wire — that the route table answers the two endpoints ADR-0010's guards and ADR-0008's
//! envelopes describe, that the `POST` starts a transient unit whose argv a stand-in can read, and that the report a
//! **job** writes is the object a `GET` answers back. Those are facts about a socket and two child processes, so they
//! live here, in the one binary that drives both.
//!
//! # What the stand-ins are, and what they are not
//!
//! `systemd-run` is a script that records its argv and exits 0: this crate denies `unsafe`, a test cannot `set_var` a
//! `PATH`, and `deploy::Tools` exists so that the program a start runs is a parameter ([`World::tools`]). So the
//! launcher's argv is a file this test reads, word for word, and every property ADR-0110 D4 rests on — the unit's
//! name, `--collect`, both `append:` streams under the data root, the `UMask=0077` the manager creates that file under,
//! and `WorkingDirectory=` naming the candidate tree — is an assertion about bytes rather than about a sentence.
//!
//! **The job itself is run in process**, through [`pass::run`], with the `--report` path the recorded argv names. The
//! program in that argv is `std::env::current_exe()`, which in a test binary is the test harness and not a CLI, so
//! spawning it would measure the harness. Handing `pass::run` the path the launcher would have passed is the same job
//! over the same file, and it is what makes the report the `GET` answers one a **job** wrote rather than one this
//! test typed.
//!
//! # Why there is no artifact here, and no daemon
//!
//! The gate is the candidate tree's own `scripts/verify-in-container.sh`, so a fixture tree carrying one exits with the
//! code the test chooses and no gate runs for forty minutes. Every arm below stops before the promotion: the gate
//! answers `3` (a crate set the running image does not vendor) or `0` with no artifact behind it, and both are endings
//! `holosmith update` reaches with nothing asked of the machine — which is why [`Nothing`] panics if a step is reached,
//! and why none of this wants Docker, a built artifact or a database a session ever touches.
//!
//! # The restart
//!
//! A promotion's last step replaces the process that served the request, so "the `GET` still answers afterwards" has
//! to be measured over a second app. The last test stops the first one and serves a **second** [`AppState`] over the
//! same install tree and the same data root — a second state because one database path is opened once per process
//! (ADR-0009), and over the same files because the route reads a report and a log out of the data root and keeps no
//! run in memory. What it answers there is the object the first one answered, and that equality is the claim.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use holosmith::config::{Config, Update};
use holosmith::deploy::{Root, Tools};
use holosmith::http::AppState;
use holosmith::http::error::{
    BAD_CLIENT, BAD_ORIGIN, INTERNAL, NO_CANDIDATE, STALE_CANDIDATE, SYSTEMD_TOO_OLD,
    UPDATE_DISABLED, UPDATE_RUNNING,
};
use holosmith::http::{self, Guards};
use holosmith::images::{self, Log};
use holosmith::pass::{self, Options};
use holosmith::promote::{DrainAnswer, Steps};
use holosmith::smoke;
use serde_json::{Value, json};

mod support;
use support::{Response, Served, TempDir, fetch, git, head, repository_on, request, state_in};

/// The control origin [`Config::default`] carries, which is therefore the one `Guards::from_config` accepts when a
/// test changes nothing about either.
const ORIGIN: &str = "http://localhost:8080";

/// How long a served app waits for its sessions when a test stops it: no test here creates one.
const GRACE: Duration = Duration::from_millis(100);

/// The `commit` a request carries when the test's subject is not the commit: any string that is not `master`'s.
const ELSEWHERE: &str = "0000000000000000000000000000000000000000";

/// One machine and one candidate tree: a clean checkout on `master`, its own gate, a data root, an install tree whose
/// `releases/` is a directory, and the programs a start runs.
struct World {
    dir: TempDir,
    /// A second scratch directory for another state over these same files: two `Database::open` calls on one path in
    /// one process is ADR-0009's lock, and both the restarted app and a second arm of a test need a state of their own.
    second: TempDir,
    config: Config,
    root: Root,
    /// `[update] source`: the fixture checkout.
    source: PathBuf,
    /// Where the fixture gate records the image, the arguments and the directory it was given, one block per run.
    recorder: PathBuf,
    /// The exit code the fixture gate answers: `0` is a verified tree and `3` is a crate set the image cannot build.
    arm: PathBuf,
    /// `deploy::Tools::systemd_run`: the launcher a press is measured through.
    runner: PathBuf,
    /// The argv [`World::runner`] recorded, one word per line.
    ran: PathBuf,
    /// `deploy::Tools::systemctl`: the manager a refused start is asked about.
    controller: PathBuf,
}

impl World {
    /// The two programs a start runs, and nothing else of `Tools` changed: `df`, `install` and the rest are the names
    /// `PATH` resolves, and no arm here reaches them.
    fn tools(&self) -> Tools {
        Tools {
            systemd_run: self.runner.clone(),
            systemctl: self.controller.clone(),
            ..Tools::default()
        }
    }

    /// The same two programs, answering what a live unit looks like: `systemd-run` refuses the name and the manager
    /// reports the unit active.
    fn busy(&self) -> Tools {
        Tools {
            systemd_run: self.dir.file("bin/systemd-run-busy"),
            systemctl: self.dir.file("bin/systemctl-active"),
            ..Tools::default()
        }
    }

    /// And once more, with a manager that cannot answer at all (`is-active` exits 3 with nothing on stderr) — the
    /// machine's failure, which is not the conflict [`World::busy`] measures.
    fn unanswerable(&self) -> Tools {
        Tools {
            systemd_run: self.dir.file("bin/systemd-run-busy"),
            systemctl: self.dir.file("bin/systemctl-inactive"),
            ..Tools::default()
        }
    }

    /// The launcher these arms measure, with a manager that answers `--version` with `line` and nothing else: the floor
    /// is read from that one line, so the version a machine reports is the whole of what a floor arm needs to give it.
    fn manager_answering(&self, line: &str) -> Tools {
        let path = self.dir.file("bin/systemctl-version");
        answering(&path, line);
        Tools {
            systemd_run: self.runner.clone(),
            systemctl: path,
            ..Tools::default()
        }
    }

    /// The state `serve` wires for this machine: its config, its install tree, the data root the config names, and the
    /// programs `tools` names. Built in one place so every arm drives the shape `serve` gives.
    fn app(&self, config: &Config, tools: Tools) -> AppState {
        let mut state = state_in(&self.dir, config.clone());
        state.root = self.root.clone();
        state.deploy = tools;
        state
    }

    /// The same state in [`World::second`]: the app a **restart** leaves behind — the same config, the same install
    /// tree, the same data root, and a database of its own.
    fn restarted(&self) -> AppState {
        let mut state = state_in(&self.second, self.config.clone());
        state.root = self.root.clone();
        state.deploy = self.tools();
        state
    }

    /// A running app of `app`'s state, behind the guards its config produces.
    async fn serving(&self, app: AppState, config: &Config) -> Served {
        Served::start_with(http::router(app), Guards::from_config(config), GRACE).await
    }

    /// The gate's own path, as the report and the argv name it.
    fn script(&self) -> PathBuf {
        self.source.join("scripts").join("verify-in-container.sh")
    }

    /// What the fixture gate recorded about the runs it was given, one block per run.
    fn gates(&self) -> String {
        std::fs::read_to_string(&self.recorder).unwrap_or_else(|error| {
            panic!(
                "the fixture gate recorded nothing at {} ({error}): a green or stale-crate-set gate is still a \
                 script this machine ran",
                self.recorder.display()
            )
        })
    }
}

/// One machine: a fixture checkout on `master` whose gate answers [`World::arm`], a data root, an install tree and
/// the stand-in programs.
fn world(label: &str) -> World {
    let dir = TempDir::new(label);
    let second = TempDir::new(&format!("{label}-second"));
    let source = dir.file("source");
    repository_on(&source, "master");
    // The gate's own writes and the artifact a build would leave are ignored, so a tree the gate has run in is still
    // clean — ADR-0110 D3's precondition, read from the other side.
    std::fs::write(source.join(".gitignore"), "dist/\ntarget/\n")
        .expect("the ignore file is writable");
    let recorder = dir.file("gate.argv");
    let arm = dir.file("gate.exit");
    std::fs::write(&arm, "1\n").expect("the arm file is writable");
    let scripts = source.join("scripts");
    std::fs::create_dir_all(&scripts).expect("the scripts directory is creatable");
    std::fs::write(
        scripts.join("verify-in-container.sh"),
        wrapper(&recorder, &arm),
    )
    .expect("the fixture gate is writable");
    std::fs::set_permissions(
        scripts.join("verify-in-container.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
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
        update: Update {
            enabled: true,
            source: Some(source.clone()),
        },
        ..Config::default()
    };
    for path in [&config.data_root, &config.projects_root, &dir.file("bin")] {
        std::fs::create_dir_all(path).expect("a fixture directory is creatable");
    }
    let root = Root::resolve(Some(dir.file("root")), Some(dir.file("holosmith.toml")));
    // A machine that has deployed: the route refuses `409 not_deployed` only when there is no release tree, and every
    // arm here is about something later than that.
    std::fs::create_dir_all(root.releases()).expect("the release tree is creatable");

    let bin = dir.file("bin");
    let ran = bin.join("systemd-run.argv");
    stand_in(&bin.join("systemd-run"), &ran, 0, "");
    stand_in(&bin.join("systemctl"), &bin.join("systemctl.argv"), 0, "");
    stand_in(
        &bin.join("systemd-run-busy"),
        &bin.join("systemd-run-busy.argv"),
        1,
        "Unit holosmith-update.service already exists.",
    );
    stand_in(
        &bin.join("systemctl-active"),
        &bin.join("systemctl-active.argv"),
        0,
        "",
    );
    stand_in(
        &bin.join("systemctl-inactive"),
        &bin.join("systemctl-inactive.argv"),
        3,
        "",
    );

    World {
        dir,
        second,
        config,
        root,
        source,
        recorder,
        arm,
        runner: bin.join("systemd-run"),
        ran,
        controller: bin.join("systemctl"),
    }
}

/// The fixture gate: it records the image it was given, its arguments and its working directory, prints one `SKIP `
/// line and a line of its own, and exits with the code [`World::arm`] holds.
///
/// A copy of `tests/update.rs`'s wrapper with one difference and one reason: the exit code is a file rather than a
/// constant, so one fixture tree is both a green gate and a stale crate set and no arm has to commit a second script.
/// The recorder and the arm file are **outside** the checkout, so neither leaves it dirty for the `candidate::resolve`
/// the route runs before the press and the job runs again.
fn wrapper(recorder: &Path, arm: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         {{\n\
         printf 'image %s\\n' \"${{HOLOSMITH_VERIFY_IMAGE-<unset>}}\"\n\
         printf 'args %s\\n' \"$*\"\n\
         printf 'cwd %s\\n' \"$PWD\"\n\
         }} >> '{recorder}'\n\
         printf 'case_fixture: the gate this machine was given, not a real one\\n'\n\
         printf 'SKIP case_fixture: no Docker daemon answers, so the fixture gate measured nothing\\n'\n\
         exit \"$(cat '{arm}')\"\n",
        recorder = recorder.display(),
        arm = arm.display(),
    )
}

/// A stand-in program at `path`: it writes its argv to `ran`, one word per line, and exits `status` with `stderr` on
/// the error stream.
///
/// An executable rather than a recorder in process, because the question is what the **child** is given: every word
/// here is argv, and the one bug it can have is being spelled nowhere but in a printed line.
fn stand_in(path: &Path, ran: &Path, status: i32, stderr: &str) {
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\n{{ printf '%s\\n' \"$@\"; }} > \"{}\"\n{}exit {status}\n",
            ran.display(),
            if stderr.is_empty() {
                String::new()
            } else {
                format!("printf '%s\\n' \"{stderr}\" >&2\n")
            }
        ),
    )
    .expect("the stand-in is writable");
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("the stand-in is executable");
}

/// A stand-in that answers with one line on stdout: the `systemctl --version` a floor is read from, and nothing else — a
/// manager whose answers are a sentence rather than an argv record.
fn answering(path: &Path, line: &str) {
    std::fs::write(
        path,
        format!("#!/bin/sh\nprintf '%s\\n' '{line}'\nexit 0\n"),
    )
    .expect("the stand-in is writable");
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("the stand-in is executable");
}

/// The argv a stand-in recorded, one word per line.
fn recorded(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| {
            panic!(
                "{} recorded nothing ({error}): the route answered without asking the program it names",
                path.display()
            )
        })
        .lines()
        .map(str::to_string)
        .collect()
}

/// The word after `flag` in a recorded argv: what the launcher was told to pass this one option.
fn after<'a>(argv: &'a [String], flag: &str) -> &'a str {
    let at = argv
        .iter()
        .position(|word| word == flag)
        .unwrap_or_else(|| panic!("{flag} is not in the argv this machine ran: {argv:?}"));
    argv.get(at + 1)
        .map(String::as_str)
        .unwrap_or_else(|| panic!("{flag} is the last word of the argv: {argv:?}"))
}

/// One `POST /api/update` as the app sends it: a JSON body, the control origin, and the client header a Holosmith
/// page sends.
async fn press(addr: SocketAddr, body: &str) -> Response {
    press_with(
        addr,
        body,
        &[
            ("Origin", ORIGIN),
            ("Content-Type", "application/json"),
            ("X-Holosmith-Client", "1"),
        ],
    )
    .await
}

/// The same request with exactly the headers a caller names, so the guards can be asked what they think of a missing
/// or foreign one.
async fn press_with(addr: SocketAddr, body: &str, headers: &[(&str, &str)]) -> Response {
    let mut text = String::from("POST /api/update HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    request(addr, &text).await
}

/// The body a press carries: one `commit` and nothing else.
fn commit_body(commit: &str) -> String {
    json!({ "commit": commit }).to_string()
}

/// A refusal's `code`, which is the word a client switches on (ADR-0008).
fn code_of(body: &str) -> String {
    let value: Value = serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("a refusal is one JSON object, and this is not ({error}): {body}")
    });
    value["code"]
        .as_str()
        .expect("a refusal carries a code")
        .to_string()
}

/// The report's `steps` as `"<step> <ending>"`, which is the pair a caller reads to see how far a run got.
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

/// The report's `detail`: the sentence the ending carries.
fn detail(report: &Value) -> &str {
    report["detail"]
        .as_str()
        .expect("an ending that is not live carries a sentence")
}

/// The machine, for a pass that never reaches it: every question panics, so a run that asks one fails this test with
/// the step it reached rather than passing over a promotion the arm cannot do.
///
/// It is the second half of "the gate asked the machine nothing", which `tests/update.rs` measures with a trace. A
/// panic is the stronger spelling here: there is no arm in this file that has a promotion to run, so a question is not
/// an unexpected trace entry but a bug.
struct Nothing;

impl Steps for Nothing {
    fn smoke(&self, candidate: &Path, root: &Path) -> smoke::Outcome {
        panic!(
            "the gate did not verify the tree, so a smoke test of {} in {} is a promotion that should not have \
             started",
            candidate.display(),
            root.display()
        );
    }

    fn images(&self) -> &dyn images::Images {
        self
    }

    fn drain(&self, admin_dir: &Path, wait: Duration) -> DrainAnswer {
        panic!(
            "nothing was promoted, so the drain of {} (a {:?} wait) is a machine that was asked",
            admin_dir.display(),
            wait
        );
    }

    fn restart(&self, tools: &Tools) -> Result<(), String> {
        panic!(
            "nothing was promoted, so `{} --user restart` is a restart nobody asked for",
            tools.systemctl.display()
        );
    }

    fn health(&self, listen: SocketAddr) -> Result<holosmith::http::health::Answer, String> {
        panic!("nothing was promoted, so nothing should be polled at {listen}");
    }
}

impl images::Images for Nothing {
    fn daemon_arch(&self) -> Result<String, String> {
        panic!("nothing was promoted, so no daemon was asked for its architecture");
    }

    fn labels(
        &self,
        image: &str,
    ) -> Result<Option<std::collections::BTreeMap<String, String>>, String> {
        panic!("nothing was promoted, so the labels of {image} were not read");
    }

    fn build(
        &self,
        tag: &str,
        _input: images::Input<'_>,
        _args: &[String],
        _log: Option<&Path>,
    ) -> Result<Log, String> {
        panic!("nothing was promoted, so the build of {tag} is a build nobody asked for");
    }
}

// ---- the refusals, in the order the route asks them ---------------------------------------------

/// A machine whose `[update] enabled` is `false` — the kill switch (ADR-0112), which the file has to write — answers
/// `403 update_disabled` with the config key's sentence, before it reads the release tree or the tree a press names.
#[tokio::test]
async fn a_machine_whose_kill_switch_is_false_refuses_the_press_as_update_disabled() {
    let world = world("update-api-disabled");
    let mut config = world.config.clone();
    config.update.enabled = false;
    assert!(
        Config::default().update.enabled,
        "the switch is off by default, so this arm would be measuring the default rather than the operator's choice"
    );

    let served = world
        .serving(world.app(&config, world.tools()), &config)
        .await;
    let refused = press(served.addr, &commit_body(&head(&world.source))).await;
    assert_eq!(refused.status, 403, "{}", refused.body);
    assert_eq!(code_of(&refused.body), UPDATE_DISABLED);
    let body: Value = serde_json::from_str(&refused.body).expect("a refusal is one JSON object");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|said| said.contains("[update] enabled")),
        "the refusal does not say which key turns this machine on: {body}"
    );
    assert!(
        !world.ran.exists(),
        "a machine that may not promote started a unit: {:?}",
        recorded(&world.ran)
    );
    served.finish().await.expect("the app stops");
}

/// A machine whose systemd is older than a promotion's job needs is `409 systemd_too_old`, and nothing is started: a
/// machine under the floor cannot promote any tree, so it is told the version it reports rather than left to
/// `systemd-run` refusing the job's two `append:` properties later.
///
/// The refusal is the route's own, asked after the release tree and **before** the candidate's five `git` children:
/// this arm is what fails if the route's call site is deleted, which the in-process test of
/// [`holosmith::update::below_floor`] cannot show.
#[tokio::test]
async fn a_machine_below_the_systemd_floor_refuses_the_press_and_starts_nothing() {
    let world = world("update-api-floor");
    let tools = world.manager_answering("systemd 239 (239-1)");
    let served = world
        .serving(world.app(&world.config, tools.clone()), &world.config)
        .await;
    let refused = press(served.addr, &commit_body(&head(&world.source))).await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert_eq!(code_of(&refused.body), SYSTEMD_TOO_OLD);
    let body: Value = serde_json::from_str(&refused.body).expect("a refusal is one JSON object");
    let said = body["message"]
        .as_str()
        .expect("a refusal carries a sentence");
    assert_eq!(
        said,
        holosmith::update::below_floor(&tools).expect("a 239 is below the floor"),
        "the route worded the floor's sentence itself"
    );
    assert!(
        !world.ran.exists(),
        "a machine under the floor started a unit: {:?}",
        recorded(&world.ran)
    );
    served.finish().await.expect("the app stops");
}

/// A machine with a candidate is asked one more question before anything starts: is the commit the request carried
/// still `master`'s? It is not, and the answer names both commits.
#[tokio::test]
async fn a_commit_master_does_not_name_is_stale_and_starts_nothing() {
    let world = world("update-api-stale");
    let served = world
        .serving(world.app(&world.config, world.tools()), &world.config)
        .await;
    let refused = press(served.addr, &commit_body(ELSEWHERE)).await;
    assert_eq!(refused.status, 422, "{}", refused.body);
    assert_eq!(code_of(&refused.body), STALE_CANDIDATE);
    let body: Value = serde_json::from_str(&refused.body).expect("a refusal is one JSON object");
    let said = body["message"]
        .as_str()
        .expect("a refusal carries a sentence");
    assert!(
        said.contains(ELSEWHERE) && said.contains(&head(&world.source)),
        "the refusal does not name both commits, so an operator cannot see what moved: {said}"
    );
    assert!(
        !world.ran.exists(),
        "a stale press started a unit: {:?}",
        recorded(&world.ran)
    );
    served.finish().await.expect("the app stops");
}

/// The four shapes of "this machine has no candidate" a press can reach are the one `404 no_candidate`, and each
/// sentence names its own repair: one code because the client's move is one, four sentences because the operator's is
/// not. The two shapes this arm does not drive — an unset `[update] source` (`NoCandidate`) and a checkout whose `HEAD`
/// is another branch (`NotOnMaster`, which leaves nothing a press could compare against `master`) — are measured by
/// `tests/update.rs`'s candidate route and `tests/candidate.rs`.
#[tokio::test]
async fn the_four_shapes_of_no_candidate_are_the_same_404() {
    let world = world("update-api-no-candidate");

    // 1. `[update] source` names a path that is not there at all.
    let absent = TempDir::new("update-api-absent");
    let mut config = world.config.clone();
    config.update.source = Some(absent.file("checkout"));
    assert_no_candidate(&world, &absent, &config, "git clone").await;

    // 2. A directory that is there and is not a repository.
    let empty = TempDir::new("update-api-not-a-repository");
    let not_a_repository = empty.file("checkout");
    std::fs::create_dir_all(&not_a_repository).expect("the empty directory is creatable");
    let mut config = world.config.clone();
    config.update.source = Some(not_a_repository);
    assert_no_candidate(&world, &empty, &config, "git clone").await;

    // 3. A repository whose `master` has no commit behind it: there is nothing to promote.
    let unborn = TempDir::new("update-api-no-master");
    let no_master = unborn.file("checkout");
    std::fs::create_dir_all(&no_master).expect("the directory is creatable");
    git(&no_master, &["init", "--quiet", "-b", "master"]);
    let mut config = world.config.clone();
    config.update.source = Some(no_master.clone());
    assert_no_candidate(&world, &unborn, &config, "no commit on `master`").await;

    // 4. A tree holding work its commit does not: a build from it is one nobody could reproduce.
    let dirty = TempDir::new("update-api-dirty");
    let checkout = dirty.file("checkout");
    repository_on(&checkout, "master");
    std::fs::write(
        checkout.join("a-file-nobody-committed"),
        "work in progress\n",
    )
    .expect("the uncommitted file is writable");
    let mut config = world.config.clone();
    config.update.source = Some(checkout);
    assert_no_candidate(&world, &dirty, &config, "stash --include-untracked").await;
}

/// One press against a state that opened its own database in `dir`, asserting the `404` and the repair its sentence
/// names. The commit is not `master`'s, so a machine that got past the candidate would answer `422` instead — which is
/// how a `404` that arrived for the wrong reason would show up here.
async fn assert_no_candidate(world: &World, dir: &TempDir, config: &Config, repair: &str) {
    let mut state = state_in(dir, config.clone());
    state.root = world.root.clone();
    state.deploy = world.tools();
    let served = Served::start_with(http::router(state), Guards::from_config(config), GRACE).await;
    let refused = press(served.addr, &commit_body(ELSEWHERE)).await;
    assert_eq!(refused.status, 404, "{}", refused.body);
    assert_eq!(code_of(&refused.body), NO_CANDIDATE);
    let body: Value = serde_json::from_str(&refused.body).expect("a refusal is one JSON object");
    let said = body["message"]
        .as_str()
        .expect("a refusal carries a sentence");
    assert!(
        said.contains(repair),
        "the refusal does not name the repair `{repair}`: {said}"
    );
    served.finish().await.expect("the app stops");
}

/// The route is in the one table, so ADR-0010's checks apply to it — and a press a browser could make from another
/// origin, or without the client header, is refused before anything is read.
#[tokio::test]
async fn the_guards_refuse_a_press_with_no_client_header_and_one_from_a_foreign_origin() {
    let world = world("update-api-guards");
    let served = world
        .serving(world.app(&world.config, world.tools()), &world.config)
        .await;
    let body = commit_body(&head(&world.source));

    // No `X-Holosmith-Client`: the header a Holosmith page sends and a cross-site form cannot.
    let bare = press_with(
        served.addr,
        &body,
        &[("Origin", ORIGIN), ("Content-Type", "application/json")],
    )
    .await;
    assert_eq!(bare.status, 403, "{}", bare.body);
    assert_eq!(code_of(&bare.body), BAD_CLIENT);

    // The right headers from a page on another origin: refused as an origin, not as a client.
    let foreign = press_with(
        served.addr,
        &body,
        &[
            ("Origin", "https://evil.example"),
            ("Content-Type", "application/json"),
            ("X-Holosmith-Client", "1"),
        ],
    )
    .await;
    assert_eq!(foreign.status, 403, "{}", foreign.body);
    assert_eq!(code_of(&foreign.body), BAD_ORIGIN);

    // Neither answer carried a CORS header, and neither press reached the launcher.
    for answer in [&bare, &foreign] {
        assert!(
            answer
                .headers
                .iter()
                .all(|(name, _)| !name.to_ascii_lowercase().starts_with("access-control-")),
            "a refusal carried a CORS header: {:?}",
            answer.headers
        );
    }
    assert!(
        !world.ran.exists(),
        "a refused press started a unit: {:?}",
        recorded(&world.ran)
    );
    served.finish().await.expect("the app stops");
}

// ---- the press that starts a job -----------------------------------------------------------------

/// A good press is `202 {job, since}`, and the launcher is asked for exactly the transient unit ADR-0110 D4 describes:
/// this binary's `update`, both streams appended to the data root's log at the machine's `0600` rather than the user
/// manager's umask, and the candidate tree as the job's directory.
#[tokio::test]
async fn a_press_is_accepted_and_the_launcher_is_asked_for_one_transient_unit() {
    let world = world("update-api-start");
    let served = world
        .serving(world.app(&world.config, world.tools()), &world.config)
        .await;
    let commit = head(&world.source);
    let accepted = press(served.addr, &commit_body(&commit)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let body: Value =
        serde_json::from_str(&accepted.body).expect("the acceptance is one JSON object");
    assert_eq!(body["job"], holosmith::update::UNIT_NAME);
    assert!(
        body["since"].as_i64().is_some_and(|since| since > 0),
        "the acceptance carries no `since`, so a client cannot tell this run from the last one: {body}"
    );

    // The argv, word for word. The program is this process's own binary — `serve` starts the release it is running —
    // and the job's words after `--` are the verb, the machine's config, the install tree and the report path.
    let exe = std::env::current_exe().expect("the test binary names itself");
    let data = world.config.data_root.clone();
    let log = data.join(holosmith::update::LOG);
    let expected: Vec<String> = vec![
        "--user".to_string(),
        format!("--unit={}", holosmith::update::UNIT_NAME),
        "--collect".to_string(),
        format!("--property=StandardOutput=append:{}", log.display()),
        format!("--property=StandardError=append:{}", log.display()),
        format!("--property=WorkingDirectory={}", world.source.display()),
        "--property=UMask=0077".to_string(),
        "--".to_string(),
        exe.display().to_string(),
        "update".to_string(),
        "--config".to_string(),
        world.root.config_file().display().to_string(),
        "--root".to_string(),
        world.root.path().display().to_string(),
        "--report".to_string(),
        data.join(holosmith::http::update::REPORT)
            .display()
            .to_string(),
    ];
    assert_eq!(
        recorded(&world.ran),
        expected,
        "the launcher was not asked for the transient unit a promotion needs (ADR-0110 D4)"
    );
    served.finish().await.expect("the app stops");
}

/// A press while a unit under that name is live is `409 update_running`, and the body names the log so a client has
/// something to read while it waits. The manager's own confusion is a `500`, never a conflict.
#[tokio::test]
async fn a_press_while_the_unit_is_live_is_a_conflict_and_one_the_manager_cannot_answer_is_not() {
    let world = world("update-api-busy");
    let commit = head(&world.source);
    let log = world.config.data_root.join(holosmith::update::LOG);

    let served = world
        .serving(world.app(&world.config, world.busy()), &world.config)
        .await;
    let refused = press(served.addr, &commit_body(&commit)).await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert_eq!(code_of(&refused.body), UPDATE_RUNNING);
    let body: Value = serde_json::from_str(&refused.body).expect("a refusal is one JSON object");
    let said = body["message"]
        .as_str()
        .expect("a refusal carries a sentence");
    assert!(
        said.contains(holosmith::update::UNIT_NAME) && said.contains(&log.display().to_string()),
        "the conflict does not name the unit and the log a client can read while it waits: {said}"
    );
    served.finish().await.expect("the app stops");

    // `systemd-run` refused the name and the manager answered `inactive` with nothing on stderr, so nothing says the
    // unit is live: that is this machine's failure, and a `409` there would tell a client to wait for a unit nothing
    // holds. The second state is the second database ADR-0009 allows per path.
    let mut state = state_in(&world.second, world.config.clone());
    state.root = world.root.clone();
    state.deploy = world.unanswerable();
    let served = Served::start_with(
        http::router(state),
        Guards::from_config(&world.config),
        GRACE,
    )
    .await;
    let failed = press(served.addr, &commit_body(&commit)).await;
    assert_eq!(failed.status, 500, "{}", failed.body);
    assert_eq!(code_of(&failed.body), INTERNAL);
    served.finish().await.expect("the app stops");
}

// ---- the report, and the restart -----------------------------------------------------------------

/// The report a job writes is the object a `GET` answers — before and after the process that started the job is gone.
///
/// The gate answers `3`, which is `scripts/check-crate-set.sh`'s own answer: the candidate's `Cargo.lock` names crates
/// the running release's image does not vendor, so nothing was built and no case judged the tree. It has to reach the
/// route as `could-not-verify` with the **host act's** sentence, never as a pass — and the machine has to have been
/// asked nothing, which is what [`Nothing`] panics on.
#[tokio::test]
async fn the_report_a_job_writes_is_what_the_get_answers_before_and_after_the_restart() {
    let world = world("update-api-report");
    std::fs::write(&world.arm, "3\n").expect("the arm file is writable");
    let served = world
        .serving(world.app(&world.config, world.tools()), &world.config)
        .await;
    let commit = head(&world.source);
    let accepted = press(served.addr, &commit_body(&commit)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);

    // The job the launcher was asked for, run over the report path that argv names. Its program is the test harness,
    // so the same command is run in process (`pass::run` is what `holosmith update` calls) — and the path is the
    // launcher's, which is the fact that makes the file the `GET` reads the job's own.
    let argv = recorded(&world.ran);
    let report = PathBuf::from(after(&argv, "--report"));
    assert_eq!(
        after(&argv, "--report"),
        world
            .config
            .data_root
            .join(holosmith::http::update::REPORT)
            .display()
            .to_string()
    );
    let code = pass::run(
        &world.config,
        &world.root,
        &Options {
            report: Some(report),
            wait: Duration::from_millis(1),
        },
        &Nothing,
        &world.tools(),
    );
    assert_eq!(code, ExitCode::FAILURE, "exit 3 is never a live candidate");

    // The gate the job ran is the candidate tree's own script, in the image the **running release** deployed — the one
    // fact a real run would take three quarters of an hour to show.
    let gates = world.gates();
    assert!(
        gates.contains(&format!("image {}\n", images::agent_image())),
        "the gate did not run in the running release's image ({}): {gates}",
        images::agent_image()
    );
    assert!(
        gates.contains(&format!("cwd {}\n", world.source.display())),
        "the gate did not run in the candidate tree: {gates}"
    );
    assert!(
        gates.contains("args \n"),
        "the gate was given arguments, and a promotion runs the full gate and never `--no-build`: {gates}"
    );

    // The log the launcher appends both its streams to, so the `GET`'s `log` is the file the argv named rather than one
    // it invented.
    let log = world.config.data_root.join(holosmith::update::LOG);
    std::fs::write(&log, "update: gate: stale crate set (exit 3)\n")
        .expect("the job's log is writable");

    let answered = fetch(served.addr, "/api/update").await;
    assert_eq!(answered.status, 200, "{}", answered.body);
    let body: Value = serde_json::from_str(&answered.body).expect("the report is one JSON object");
    assert_eq!(
        body["state"], "could-not-verify",
        "a gate that answered 3 is the image's fact and not a tree's, so it is not a pass: {body}"
    );
    assert_eq!(
        step_list(&body),
        ["candidate resolved", "gate stale crate set"],
        "the report's steps do not say where the run stopped: {body}"
    );
    let said = detail(&body);
    for fact in ["host act", "cargo fetch --locked", "build and deploy"] {
        assert!(
            said.contains(fact),
            "the refusal is exit 3's and has to name `{fact}`: {said}"
        );
    }
    assert!(
        said.contains("Nothing was asked of the machine"),
        "the refusal does not say the machine was asked nothing: {said}"
    );
    assert_eq!(
        body["candidate"]["source"],
        world.source.display().to_string()
    );
    assert_eq!(body["candidate"]["commit"], commit);
    assert_eq!(
        body["candidate"]["tag"],
        git(&world.source, &["describe", "--tags", "--always"])
    );
    assert_eq!(
        body["gate"]["command"],
        world.script().display().to_string()
    );
    assert_eq!(body["gate"]["image"], images::agent_image());
    assert_eq!(body["gate"]["exit_code"], 3);
    assert_eq!(body["gate"]["ending"], "stale crate set");
    assert_eq!(
        body["gate"]["skips"], 1,
        "the gate printed one `SKIP ` line and the report does not carry the count: {body}"
    );
    assert!(
        body["artifact"].is_null(),
        "a gate that never verified the tree left an artifact: {body}"
    );
    assert_eq!(body["log"]["path"], log.display().to_string());
    assert_eq!(
        body["log"]["lines"],
        json!(["update: gate: stale crate set (exit 3)"]),
        "the route did not answer the lines of the log its own job appends to: {body}"
    );
    assert!(
        body["since"].as_i64().is_some_and(|since| since > 0),
        "the report carries no `since`, so a client cannot date the run: {body}"
    );

    // The restart: the first app stops, and a second one over the same install tree and data root answers. Both are
    // read out of files, so a route that kept a run in memory would answer something else here — or nothing.
    served.finish().await.expect("the first app stops");
    let after = world.serving(world.restarted(), &world.config).await;
    let again = fetch(after.addr, "/api/update").await;
    assert_eq!(
        again.status, 200,
        "the report did not survive the restart: {}",
        again.body
    );
    let restarted: Value =
        serde_json::from_str(&again.body).expect("the report is still one JSON object");
    assert_eq!(
        restarted, body,
        "the restarted app answers a different report, so the answer is the process's and not the file's"
    );
    after.finish().await.expect("the app stops");
}
