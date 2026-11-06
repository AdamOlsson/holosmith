//! `server deploy`: the seven steps, the words, the install tree and the two proxies (HS-047, spec item 10).
//!
//! These tests drive [`deploy::run`] over a machine they build, and that is the same discipline `tests/doctor.rs`
//! established for the same reason: a deploy is a set of claims about *a host* — its daemon, its git, its paths —
//! so the only way to test one without becoming a test of the machine running it is to hand every claim in.
//! `git`, `df`, `tailscale`, `sudo`, `install` and `chmod` are scripts this test wrote (the `Tools` seam), systemd
//! is a directory it leaves out, the daemon is a [`Fake`], and `--root` is what keeps the install tree inside the
//! temp directory.
//!
//! Two things are driven **for real**, because a fake would prove nothing about them: the mode and byte reads run
//! `/usr/bin/install` and `/usr/bin/chmod` over a real tree (a shim cannot show that `install -m 0600` produces a
//! 0600 file), and step 1's port check binds real sockets (that is the one finding a deploy downgrades, and a
//! fake port would not exercise `doctor`'s own read).
//!
//! ADR-0056 is the design these assert: a step is a read and an apply, `skipped` is never `satisfied`, a failure
//! stops the run, and the proxies' recipe is the boot's own.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use holosmith::config::{Config, Role};
use holosmith::container;
use holosmith::deploy::{self, DeployStep, Line, Report, Target, Word};
use holosmith::docker;
use holosmith::doctor;
use holosmith::images::{self, Expectation, Kind};
use holosmith::key;
use holosmith::preflight;
use holosmith::proxies;
use holosmith::selftest;

mod support;
use support::TempDir;

/// Hold this for the whole body of a test here: one test at a time.
///
/// The same trap `tests/doctor.rs` documents: `exec` of a file any process holds open for writing fails with
/// `ETXTBSY`, and a forked child inherits the write descriptor of a shim written a moment earlier. Every test in
/// this file writes shims and then runs them, and nothing else in this binary forks, so serialising each whole
/// body is the whole fix.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An executable shim for `name` answering `body`, in `dir`'s own `bin`.
fn shim(dir: &TempDir, name: &str, body: &str) -> PathBuf {
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("the shim directory is creatable");
    let path = bin.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("a shim is writable");
    let mut permissions = std::fs::metadata(&path)
        .expect("the shim exists")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("a shim is executable");
    path
}

/// The `df -Pk` answer step 1's disk check reads.
fn df_shim(dir: &TempDir) -> PathBuf {
    df_shim_with(dir, 480_000_000)
}

/// A `df -Pk` shim answering `available_kb` free — the knob an exhausted disk needs.
fn df_shim_with(dir: &TempDir, available_kb: u64) -> PathBuf {
    shim(
        dir,
        "df",
        &format!(
            "printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n\
             /dev/mapper/root 1000000000 1 {available_kb} 1%% /\\n'"
        ),
    )
}

/// The `doctor::Tools` step 1 runs: a real git (the merge trial is a measurement, and a shim would fail it), 480 GB
/// free, a logged-in Tailscale and no systemd.
fn doctor_tools(dir: &TempDir) -> doctor::Tools {
    doctor::Tools {
        preflight: preflight::Tools {
            git: PathBuf::from("git"),
            df: df_shim(dir),
        },
        tailscale: shim(dir, "tailscale", "echo '{\"BackendState\":\"Running\"}'"),
        systemd: dir.file("no-systemd-here"),
    }
}

/// The deploy's tools with the machine's own `install` and `chmod`, and a `sudo` that is never reached because
/// every test using these passes `--root`.
///
/// The three are named rather than pathed, exactly as [`deploy::Tools::default`] names them, so this test asks the
/// same question production asks and follows the host's `PATH` — a repository that hard-coded `/usr/bin/install`
/// would fail on a machine whose coreutils is elsewhere, with a message about a missing file rather than about the
/// check.
fn real_tools(dir: &TempDir) -> deploy::Tools {
    deploy::Tools {
        doctor: doctor_tools(dir),
        sudo: PathBuf::from("sudo"),
        install: PathBuf::from("install"),
        chmod: PathBuf::from("chmod"),
    }
}

/// A `sudo` that records and runs nothing.
///
/// **It must not exec anything**: the system-path run names `/etc/holosmith` and `/var/lib/holosmith`, and a shim
/// that forwarded to the real `sudo` would install into the machine running the tests. Recording the argv and
/// exiting 0 is the whole of what test 6 needs to know.
fn sudo_recorder(dir: &TempDir, log: &Path) -> PathBuf {
    shim(
        dir,
        "sudo",
        &format!("printf 'sudo %s\\n' \"$*\" >> '{}'", log.display()),
    )
}

/// An `install` or a `chmod` that records its argv and exits 0, without touching the paths it was given.
fn recorder(dir: &TempDir, name: &str, log: &Path) -> PathBuf {
    shim(
        dir,
        name,
        &format!("printf '{name} %s\\n' \"$*\" >> '{}'", log.display()),
    )
}

/// The deploy's tools over recording shims: `calls` gets every `install`/`chmod` argv, `sudo_calls` every argv the
/// deploy prefixed with `sudo`.
fn recording_tools(dir: &TempDir, calls: &Path, sudo_calls: &Path) -> deploy::Tools {
    deploy::Tools {
        doctor: doctor_tools(dir),
        sudo: sudo_recorder(dir, sudo_calls),
        install: recorder(dir, "install", calls),
        chmod: recorder(dir, "chmod", calls),
    }
}

/// Everything a shim recorded, one line per call.
fn recorded(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// A daemon that answers `GET /_ping` the way a real one does, for as long as the test's process lives.
///
/// `tests/doctor.rs`'s helper, copied for the same reason it exists there: step 1's docker check is a real socket
/// read, so a test that wants it to pass has to answer it.
fn fake_daemon(path: &Path) {
    let listener = UnixListener::bind(path).expect("the test socket is bindable");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nApi-Version: 1.51\r\n\r\nOK");
        }
    });
}

/// The machine a deploy runs on: a config whose data root is the spec's own (`/var/lib/holosmith`, so the prefixed
/// tree is the brief's table), whose listeners are the kernel's, and whose daemon is `docker_host`.
fn config(docker_host: Option<String>) -> Config {
    Config {
        role: Role::Server,
        docker_host,
        control_listen: "127.0.0.1:0".parse().expect("a listener address"),
        preview_listen: "127.0.0.1:0".parse().expect("a listener address"),
        ..Config::default()
    }
}

/// The machine the deploy is asked about, as a value: the proxies' state that step 4 reads, the images step 3
/// reads and builds, and the argv it would run, with no daemon anywhere.
///
/// The flags are `AtomicBool`s and the rest `Mutex`es because the seam takes `&self`, exactly as a daemon handle
/// does — and a step's second run within one test has to see what its first run did.
struct Fake {
    egress: AtomicBool,
    key: AtomicBool,
    network: AtomicBool,
    calls: Mutex<Vec<Vec<String>>>,
    /// The labels `docker image inspect` would answer for each tag: a tag absent from the map is no such image.
    labels: Mutex<BTreeMap<String, BTreeMap<String, String>>>,
    /// The architecture word the daemon answers, which the build guard compares with this binary's target.
    arch: Mutex<Result<String, String>>,
    /// Every build asked for, as its tag and its argument vector.
    builds: Mutex<Vec<(String, Vec<String>)>>,
    /// The output of a build that fails; `None` is a daemon that builds whatever it is handed.
    failing_build: Mutex<Option<String>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            egress: AtomicBool::new(false),
            key: AtomicBool::new(false),
            network: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
            labels: Mutex::new(BTreeMap::new()),
            // This machine's own word: every arm that is not about the architecture guard has to pass it, and a
            // fixture that guessed `amd64` would make those arms fail on an arm machine.
            arch: Mutex::new(Ok(daemon_word())),
            builds: Mutex::new(Vec::new()),
            failing_build: Mutex::new(None),
        }
    }

    /// The same machine with one image already there, carrying `labels` — the state a deploy leaves behind.
    fn with_labels(self, image: &str, labels: BTreeMap<String, String>) -> Self {
        self.labels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(image.to_string(), labels);
        self
    }

    /// The same machine, with a daemon that builds for another architecture.
    fn with_arch(self, arch: &str) -> Self {
        *self
            .arch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Ok(arch.to_string());
        self
    }

    /// The same machine, with a daemon whose every build fails printing `text`.
    fn with_failing_build(self, text: &str) -> Self {
        *self
            .failing_build
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(text.to_string());
        self
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn builds(&self) -> Vec<(String, Vec<String>)> {
        self.builds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn record(&self, argv: Vec<String>) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(argv);
    }
}

/// The architecture word a daemon on this machine answers, from the target this crate was built for.
fn daemon_word() -> String {
    match env!("HOLOSMITH_TARGET")
        .split('-')
        .next()
        .unwrap_or_default()
    {
        "x86_64" => "amd64".to_string(),
        "aarch64" => "arm64".to_string(),
        other => other.to_string(),
    }
}

/// The labels the Dockerfile's `LABEL` lines write, read back out of a `docker build` argument vector.
///
/// Parsed from [`images::build_arguments`]' own output rather than written as a second list, so that the fake's
/// build and the real one cannot disagree about an argument's spelling.
fn labels_from_args(args: &[String]) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        if argument != "--build-arg" {
            continue;
        }
        let Some((key, value)) = arguments.next().and_then(|pair| pair.split_once('=')) else {
            continue;
        };
        let label = match key {
            "HOLOSMITH_VERSION" => images::LABEL_VERSION,
            "HOLOSMITH_GIT_TAG" => images::LABEL_GIT_TAG,
            "HOLOSMITH_PI_VERSION" => images::LABEL_PI,
            "HOLOSMITH_TOOLCHAINS" => images::LABEL_TOOLCHAINS,
            _ => continue,
        };
        // The label carries what the argument carried: the Dockerfile's `LABEL` line has no way to rewrite it.
        labels.insert(label.to_string(), value.to_string());
    }
    labels
}

/// The labels an image built by this binary carries, as `docker image inspect` would answer them.
fn expected_labels(kind: Kind, config: &Config) -> BTreeMap<String, String> {
    let expectation = Expectation::of(kind, config);
    labels_from_args(&images::build_arguments(&kind.image(), &expectation))
}

impl proxies::Containers for Fake {
    fn running(&self, name: &str) -> bool {
        match name {
            container::PROXY_NAME => self.egress.load(Ordering::SeqCst),
            key::KEY_PROXY_NAME => self.key.load(Ordering::SeqCst),
            _ => false,
        }
    }

    fn image_exists(&self, image: &str) -> bool {
        self.labels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(image)
    }

    fn network_names(&self, _filter: &str, _bound: Duration) -> Result<Vec<String>, String> {
        Ok(if self.network.load(Ordering::SeqCst) {
            vec![docker::EGRESS_NETWORK.to_string()]
        } else {
            Vec::new()
        })
    }

    fn ensure_network(&self, name: &str) -> Result<(), String> {
        self.record(vec![
            "network".to_string(),
            "create".to_string(),
            name.to_string(),
        ]);
        self.network.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn replace(&self, name: &str, args: &[String], _bound: Duration) -> Result<(), String> {
        self.record(args.to_vec());
        match name {
            container::PROXY_NAME => self.egress.store(true, Ordering::SeqCst),
            key::KEY_PROXY_NAME => self.key.store(true, Ordering::SeqCst),
            _ => {}
        }
        Ok(())
    }
}

impl doctor::Machine for Fake {
    /// A deploy's preflight does not read the proxies — only `doctor --full` does — so a `Fake` that answered
    /// `true` here would be claiming a measurement nobody made.
    fn running(&self, _name: &str) -> bool {
        false
    }

    fn image_pi(&self, _image: &str) -> Result<String, String> {
        Err("not asked".to_string())
    }

    /// The same read step 3 makes, so a test that wanted both would not need two fixtures.
    fn image_labels(&self, image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        Ok(self
            .labels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(image)
            .cloned())
    }

    fn selftest(&self, _image: &str) -> selftest::Outcome {
        selftest::Outcome::Unknown("not asked".to_string())
    }
}

impl images::Images for Fake {
    fn daemon_arch(&self) -> Result<String, String> {
        self.arch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn labels(&self, image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        <Fake as doctor::Machine>::image_labels(self, image)
    }

    fn build(&self, tag: &str, _archive: &[u8], args: &[String]) -> Result<images::Log, String> {
        self.builds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((tag.to_string(), args.to_vec()));
        if let Some(text) = self
            .failing_build
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Ok(images::Log {
                success: false,
                text,
            });
        }
        // A successful build leaves the image it was asked for, labelled with the arguments it was given: a fake
        // that answered "built" and then reported no image would make every second run a build.
        self.labels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(tag.to_string(), labels_from_args(args));
        Ok(images::Log {
            success: true,
            text: String::new(),
        })
    }
}

/// The step's line, or a panic naming the whole report.
fn line(report: &Report, step: DeployStep) -> &Line {
    report
        .lines
        .iter()
        .find(|line| line.step == step)
        .unwrap_or_else(|| panic!("no {step} line in:\n{}", report.render()))
}

/// The mode of a path, `0o777` of it — what `install -m` and `chmod` are asked to produce.
fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("{} has no metadata: {error}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

/// One path's listing entry: its mode, and for a file its length and a hash of its bytes.
///
/// A hash rather than the bytes so a listing line stays readable, and a hash *at all* because the claim
/// `a_second_run_is_satisfied_and_changes_nothing` makes is about what a second run leaves, not just its names.
fn entry(root: &Path, path: &Path) -> String {
    let metadata = std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("{} has no metadata: {error}", path.display()));
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string();
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.is_dir() {
        return format!("{relative} {mode:04o} dir");
    }
    let bytes = std::fs::read(path).expect("a listed file is readable");
    format!("{relative} {mode:04o} {} {:016x}", bytes.len(), fnv(&bytes))
}

/// FNV-1a, which is all a listing needs: a stable fingerprint of one file's bytes.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

/// Every path under `root`, sorted, as [`entry`] prints it: a `find`-style listing a diff can be taken of.
fn listing(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        found.push(entry_of(root, &path));
        if entry.path().is_dir() {
            walk(root, &path, found);
        }
    }
}

/// [`entry`] under a name the recursion can call without shadowing the loop's own `entry`.
fn entry_of(root: &Path, path: &Path) -> String {
    entry(root, path)
}

/// The number of writes a step-2 line says it made — the sentence's own count, so "one child process per action"
/// can be checked against what the shims were actually asked to run.
fn actions(report: &Report) -> usize {
    let line = line(report, DeployStep::Files);
    assert_eq!(
        line.word,
        Word::Changed,
        "step 2 did not change anything: {}",
        line.sentence
    );
    line.sentence
        .split_whitespace()
        .next()
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "step 2's sentence does not count its actions: {}",
                line.sentence
            )
        })
}

// ---- the words and the report -------------------------------------------------------------

#[test]
fn the_seven_steps_are_the_specs_order() {
    let words: Vec<&str> = DeployStep::ALL.iter().map(|step| step.word()).collect();
    assert_eq!(
        words,
        [
            "preflight",
            "files",
            "images",
            "proxies",
            "service",
            "origins",
            "health"
        ],
        "the step words are not item 10's seven steps"
    );
    let numbers: Vec<u8> = DeployStep::ALL.iter().map(|step| step.number()).collect();
    assert_eq!(numbers, [1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(DeployStep::Files.to_string(), "files");
    assert_eq!(DeployStep::Health.to_string(), "health");
    // The five words, in the spec's vocabulary: `skipped` is never `satisfied`, and `needs` is a dry run's.
    let words: Vec<&str> = [
        Word::Satisfied,
        Word::Needs,
        Word::Changed,
        Word::Skipped,
        Word::Failed,
    ]
    .iter()
    .map(|word| word.as_str())
    .collect();
    assert_eq!(
        words,
        ["satisfied", "needs", "changed", "skipped", "failed"]
    );
    assert_eq!(Word::Needs.to_string(), "needs");
    // And a line is the number, the step, the word and the sentence: `Finding`'s three fields after the number.
    let line = Line {
        step: DeployStep::Files,
        word: Word::Satisfied,
        sentence: "all 6 paths are present".to_string(),
    };
    assert_eq!(
        line.to_string(),
        "2. files: satisfied: all 6 paths are present"
    );
}

#[test]
fn the_report_prints_the_tally_and_who_owns_what_remains() {
    // A report built by hand: the shape is the acceptance's, and the two claims that matter are the summary's four
    // counts (always present, so a reader can diff two runs) and the incomplete line naming the stories.
    let report = Report {
        lines: vec![
            Line {
                step: DeployStep::Preflight,
                word: Word::Satisfied,
                sentence: "9 findings: 7 pass, 0 fail, 2 unknown (systemd, disk)".to_string(),
            },
            Line {
                step: DeployStep::Files,
                word: Word::Changed,
                sentence: "2 actions: install -d -m 0700 /run/holosmith".to_string(),
            },
            Line {
                step: DeployStep::Images,
                word: Word::Changed,
                sentence: "built 2 images: holosmith-proxy:0.1.0, agent-image:0.1.0".to_string(),
            },
            Line {
                step: DeployStep::Service,
                word: Word::Skipped,
                sentence: "HS-049 installs the unit".to_string(),
            },
            Line {
                step: DeployStep::Health,
                word: Word::Failed,
                sentence: "the health endpoint never answered".to_string(),
            },
        ],
        failed: Some(DeployStep::Health),
        dry_run: false,
        target: "the system paths, with sudo where each write needs it".to_string(),
    };
    let text = report.render();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "deploy: run");
    // Which tree the report describes is its own line, before the steps: a `--root` run's paths mean nothing
    // without the privilege the whole run used.
    assert_eq!(
        lines[1],
        "deploy: into the system paths, with sudo where each write needs it"
    );
    assert_eq!(
        lines[2],
        "1. preflight: satisfied: 9 findings: 7 pass, 0 fail, 2 unknown (systemd, disk)"
    );
    assert_eq!(
        lines[7],
        "deploy: 5 steps: 1 satisfied, 2 changed, 1 skipped, 1 failed"
    );
    assert_eq!(
        lines[8],
        "deploy: incomplete: 1 of 7 steps belongs to later stories — HS-049: service"
    );
    assert_eq!(lines.len(), 9);
    assert!(report.failed());

    // A dry run says so, and a report with nothing skipped has no incomplete line to print.
    let dry = Report {
        lines: vec![Line {
            step: DeployStep::Preflight,
            word: Word::Satisfied,
            sentence: "clean".to_string(),
        }],
        failed: None,
        dry_run: true,
        target: "the system paths".to_string(),
    };
    let text = dry.render();
    assert_eq!(text.lines().next(), Some("deploy: dry run"));
    assert!(
        !text.contains("incomplete"),
        "a run with nothing skipped claimed it was incomplete:\n{text}"
    );
    assert!(!dry.failed());
}

// ---- the dry run and the install ----------------------------------------------------------

#[test]
fn a_dry_run_lists_the_steps_and_creates_nothing() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-dry-run");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let root = dir.file("root");
    let target = Target::resolve(Some(root.clone()));
    let deployed = target.config(&config);
    let machine = Fake::new();

    let report = deploy::run(&deployed, &target, &real_tools(&dir), &machine, true);

    assert!(!report.failed(), "a dry run failed:\n{}", report.render());
    assert_eq!(
        report
            .lines
            .iter()
            .map(|line| line.step)
            .collect::<Vec<DeployStep>>(),
        DeployStep::ALL,
        "a dry run did not name all seven steps:\n{}",
        report.render()
    );
    // Step 1's plan is its read, step 3's is a build of both images, and the three steps this release cannot
    // fill say so.
    assert_eq!(line(&report, DeployStep::Preflight).word, Word::Satisfied);
    assert_eq!(
        line(&report, DeployStep::Files).word,
        Word::Needs,
        "a dry run did not print a plan for step 2"
    );
    assert!(
        line(&report, DeployStep::Files)
            .sentence
            .contains("install -d -m 0755"),
        "step 2's plan names no action: {}",
        line(&report, DeployStep::Files).sentence
    );
    assert_eq!(
        line(&report, DeployStep::Images).word,
        Word::Needs,
        "a dry run did not print a plan for step 3: {}",
        line(&report, DeployStep::Images).sentence
    );
    for step in [DeployStep::Service, DeployStep::Origins, DeployStep::Health] {
        assert_eq!(
            line(&report, step).word,
            Word::Skipped,
            "{step} is not skipped: {}",
            line(&report, step).sentence
        );
    }
    assert!(!root.exists(), "a dry run created {}", root.display());
    assert!(
        machine.calls().is_empty() && machine.builds().is_empty(),
        "a dry run started something: {:?} {:?}",
        machine.calls(),
        machine.builds()
    );
}

#[test]
fn an_install_creates_the_four_paths_with_their_modes() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-install");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let root = dir.file("root");
    let target = Target::resolve(Some(root.clone()));
    let deployed = target.config(&config);

    let report = deploy::run(&deployed, &target, &real_tools(&dir), &Fake::new(), false);
    assert!(!report.failed(), "the install failed:\n{}", report.render());
    assert_eq!(line(&report, DeployStep::Files).word, Word::Changed);

    let paths = [
        (root.join("etc/holosmith"), 0o755),
        (root.join("etc/holosmith/config.toml"), 0o644),
        (root.join("var/lib/holosmith"), 0o755),
        (root.join("var/lib/holosmith/secrets"), 0o700),
        (root.join("var/lib/holosmith/secrets/keys.toml"), 0o600),
        (root.join("run/holosmith"), 0o700),
    ];
    for (path, expected) in paths {
        assert!(path.exists(), "{} was not installed", path.display());
        assert_eq!(
            mode(&path),
            expected,
            "{} is {:04o} where {:04o} was asked",
            path.display(),
            mode(&path),
            expected
        );
    }
    assert!(
        root.join("var/lib/holosmith").is_dir(),
        "the data root is not a directory"
    );

    // The file it wrote is the file `serve` reads: it re-parses, and its two roots are the target's.
    let loaded = Config::load(Some(root.join("etc/holosmith/config.toml")))
        .unwrap_or_else(|error| panic!("the deployed config does not load: {error}"));
    assert_eq!(
        loaded.config.data_root,
        root.join("var/lib/holosmith"),
        "the written config does not name the install tree's data root"
    );
    assert_eq!(
        loaded.config.projects_root,
        root.join("srv/holosmith/projects")
    );
    // Everything the deploy must not move is the machine's own, unchanged.
    assert_eq!(loaded.config.control_listen, deployed.control_listen);
    assert_eq!(loaded.config.docker_host, deployed.docker_host);
    // The empty key store is the one `key list` reads, not a file it would refuse.
    let secrets = key::Store::from_data_root(&loaded.config.data_root)
        .load()
        .expect("the deployed key file parses");
    assert!(secrets.providers.is_empty() && secrets.sessions.is_empty());
}

#[test]
fn a_second_run_is_satisfied_and_changes_nothing() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-second-run");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let root = dir.file("root");
    let target = Target::resolve(Some(root.clone()));
    let deployed = target.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let first = deploy::run(&deployed, &target, &tools, &machine, false);
    assert!(!first.failed(), "the first run failed:\n{}", first.render());
    let before = listing(&root);
    assert!(!before.is_empty(), "the first run installed nothing");

    let second = deploy::run(&deployed, &target, &tools, &machine, false);
    assert!(
        !second.failed(),
        "the second run failed:\n{}",
        second.render()
    );
    for step in [
        DeployStep::Preflight,
        DeployStep::Files,
        DeployStep::Proxies,
    ] {
        assert_eq!(
            line(&second, step).word,
            Word::Satisfied,
            "{step} was not already satisfied: {}",
            line(&second, step).sentence
        );
    }
    assert!(
        line(&second, DeployStep::Files)
            .sentence
            .contains("present with their modes"),
        "step 2 does not say what it read: {}",
        line(&second, DeployStep::Files).sentence
    );
    assert_eq!(listing(&root), before, "the second run changed the tree");
}

#[test]
fn an_existing_key_file_is_never_rewritten() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-keys");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let root = dir.file("root");
    let target = Target::resolve(Some(root.clone()));
    let deployed = target.config(&config);
    // A key store with a real key in it, and a mode that is wrong on purpose: the two things a deploy must handle
    // differently. `install` would truncate the bytes; `chmod` repairs the mode and touches nothing else.
    let secrets = deployed.data_root.join(key::SECRETS_DIR);
    std::fs::create_dir_all(&secrets).expect("the secrets directory is creatable");
    let keys = secrets.join(key::KEYS_FILE);
    let stored = "[providers]\nanthropic = \"sk-ant-not-a-real-key\"\n[sessions]\n";
    std::fs::write(&keys, stored).expect("the key file is writable");
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o644))
        .expect("the key file's mode is settable");

    let report = deploy::run(&deployed, &target, &real_tools(&dir), &Fake::new(), false);
    assert!(!report.failed(), "the install failed:\n{}", report.render());

    assert_eq!(
        std::fs::read_to_string(&keys).expect("the key file is readable"),
        stored,
        "the deploy rewrote the file that holds every stored provider key"
    );
    assert_eq!(
        mode(&keys),
        0o600,
        "the key file's mode was not repaired with chmod"
    );
    assert_eq!(mode(&secrets), 0o700);
    // And the write it did make was a `chmod`, not an `install`, which is what `0600` after a truncation would look
    // like: the bytes above are the evidence, and the words here are the reason.
    let changed = line(&report, DeployStep::Files).sentence.clone();
    assert!(
        changed.contains(&format!("chmod 0600 {}", keys.display())),
        "the mode repair is not the action step 2 reported: {changed}"
    );
}

#[test]
fn the_system_paths_prefix_every_write_with_sudo_and_root_prefixes_none() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-sudo");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);

    // The `--root` run: the tree is the invoking user's, so nothing is prefixed by `sudo`.
    let root = dir.file("root");
    let rooted = config(Some(format!("unix://{}", socket.display())));
    let target = Target::resolve(Some(root));
    let deployed = target.config(&rooted);
    let calls = dir.file("root-calls.log");
    let sudo_calls = dir.file("root-sudo.log");
    let report = deploy::run(
        &deployed,
        &target,
        &recording_tools(&dir, &calls, &sudo_calls),
        &Fake::new(),
        false,
    );
    assert!(
        !report.failed(),
        "the rooted run failed:\n{}",
        report.render()
    );
    assert_eq!(
        recorded(&sudo_calls),
        Vec::<String>::new(),
        "a --root run asked sudo to write the invoking user's tree"
    );
    let direct = recorded(&calls);
    assert_eq!(
        direct.len(),
        actions(&report),
        "step 2 reported {} actions and the shims ran {}",
        actions(&report),
        direct.len()
    );
    assert!(
        direct.iter().all(|call| call.starts_with("install ")),
        "a --root run's writes are not all `install`: {direct:?}"
    );
    assert_eq!(
        report.target,
        format!(
            "{}, as the invoking user (no sudo)",
            dir.file("root").display()
        )
    );

    // The system run: same actions, each one prefixed by `sudo`. The shims run nothing, so `/etc` is untouched.
    let system = config(Some(format!("unix://{}", socket.display())));
    let target = Target::resolve(None);
    let deployed = target.config(&system);
    let calls = dir.file("system-calls.log");
    let sudo_calls = dir.file("system-sudo.log");
    let report = deploy::run(
        &deployed,
        &target,
        &recording_tools(&dir, &calls, &sudo_calls),
        &Fake::new(),
        false,
    );
    assert!(
        !report.failed(),
        "the system run failed:\n{}",
        report.render()
    );
    let prefixed = recorded(&sudo_calls);
    assert_eq!(
        prefixed.len(),
        actions(&report),
        "step 2 reported {} actions and sudo was asked {} times",
        actions(&report),
        prefixed.len()
    );
    assert!(
        prefixed.iter().all(|call| call.starts_with("sudo ")),
        "the system paths were written without sudo: {prefixed:?}"
    );
    assert_eq!(
        recorded(&calls),
        Vec::<String>::new(),
        "a system-path run called install or chmod without sudo"
    );
    assert!(
        prefixed
            .iter()
            .any(|call| call.contains("/etc/holosmith/config.toml")),
        "no write named the config path: {prefixed:?}"
    );
    assert_eq!(
        report.target,
        "the system paths, with sudo where each write needs it"
    );
}

#[test]
fn a_missing_sudo_is_a_sentence_not_a_panic() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-no-sudo");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let target = Target::resolve(None);
    let deployed = target.config(&config);
    let missing = dir.file("no-sudo-here");
    let tools = deploy::Tools {
        doctor: doctor_tools(&dir),
        sudo: missing.clone(),
        install: PathBuf::from("/usr/bin/install"),
        chmod: PathBuf::from("/usr/bin/chmod"),
    };

    let report = deploy::run(&deployed, &target, &tools, &Fake::new(), false);

    assert_eq!(report.failed, Some(DeployStep::Files));
    assert_eq!(line(&report, DeployStep::Files).word, Word::Failed);
    let sentence = &line(&report, DeployStep::Files).sentence;
    assert!(
        sentence.contains("sudo"),
        "the sentence does not name sudo: {sentence}"
    );
    assert!(
        sentence.contains(&missing.display().to_string()),
        "the sentence does not name the program it could not run: {sentence}"
    );
    assert!(
        sentence.contains("install -d"),
        "the sentence does not name the argv it tried: {sentence}"
    );
}

// ---- step 1's one divergence, and the failure that stops the run ---------------------------

#[test]
fn a_failing_preflight_stops_at_step_one() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-broken-machine");
    let config = config(Some(format!(
        "unix://{}",
        dir.file("absent.sock").display()
    )));
    let root = dir.file("root");
    let target = Target::resolve(Some(root.clone()));

    let report = deploy::run(
        &target.config(&config),
        &target,
        &real_tools(&dir),
        &Fake::new(),
        false,
    );

    assert_eq!(report.failed, Some(DeployStep::Preflight));
    assert_eq!(
        report.lines.len(),
        1,
        "the run did not stop:\n{}",
        report.render()
    );
    assert_eq!(line(&report, DeployStep::Preflight).word, Word::Failed);
    // The sentence is `serve`'s refusal and `doctor`'s line, verbatim — not a deploy's rephrasing of it.
    let sentence = &line(&report, DeployStep::Preflight).sentence;
    assert!(
        sentence.contains("start it with `sudo systemctl start docker`"),
        "the sentence is not the preflight's own: {sentence}"
    );
    assert!(
        !root.exists(),
        "a failed preflight installed something at {}",
        root.display()
    );

    // A dry run reads the same machine, and a dry run that hid it would be useless: it fails too.
    let dry = deploy::run(
        &target.config(&config),
        &target,
        &real_tools(&dir),
        &Fake::new(),
        true,
    );
    assert_eq!(dry.failed, Some(DeployStep::Preflight));
    assert!(dry.render().contains("deploy: dry run"));
}

#[test]
fn a_held_port_does_not_stop_the_deploy_but_another_failure_does() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-held-port");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    // Something already serving on the control port: the machine `server deploy` is re-run on, which the spec
    // promises is safe (ADR-0056).
    let held = TcpListener::bind("127.0.0.1:0").expect("a port is bindable");
    let listen = held.local_addr().expect("the bound address is known");
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = listen;
    let root = dir.file("root");
    let target = Target::resolve(Some(root.clone()));

    let report = deploy::run(
        &target.config(&config),
        &target,
        &real_tools(&dir),
        &Fake::new(),
        false,
    );
    assert!(
        !report.failed(),
        "a serving machine failed the deploy:\n{}",
        report.render()
    );
    assert_eq!(line(&report, DeployStep::Preflight).word, Word::Satisfied);
    let sentence = line(&report, DeployStep::Preflight).sentence.clone();
    assert!(
        sentence.contains("the control port is held"),
        "the held port is not a remark: {sentence}"
    );
    assert!(
        sentence.contains("re-run on a serving machine is not a failure here"),
        "the remark does not say why it is not a failure: {sentence}"
    );
    assert!(
        sentence.contains("fail"),
        "the remark does not carry the tally it qualifies: {sentence}"
    );

    // The same machine with a daemon that does not answer: both findings are in the list, and the non-port one is
    // the failure — the divergence downgrades `ports` and nothing else.
    drop(held);
    let mut broken = config.clone();
    broken.docker_host = Some(format!("unix://{}", dir.file("absent.sock").display()));
    let report = deploy::run(
        &target.config(&broken),
        &target,
        &real_tools(&dir),
        &Fake::new(),
        false,
    );
    assert_eq!(report.failed, Some(DeployStep::Preflight));
    let sentence = &line(&report, DeployStep::Preflight).sentence;
    assert!(
        sentence.contains("start it with"),
        "the failure is not the daemon's own sentence: {sentence}"
    );
    assert!(
        !sentence.contains("re-run on a serving machine"),
        "a real failure was downgraded to a remark: {sentence}"
    );
    assert_eq!(report.lines.len(), 1);
}

// ---- step 3: the two images --------------------------------------------------------------

/// What a failed `COPY` build printed on this machine, trimmed to the lines the sentence is parsed from: the
/// instruction inside the Dockerfile, the layer, and the child's own last line.
const COPY_FAILURE: &str = "#3 [2/3] COPY missing-file /x
#3 0.044 ERROR: failed to build: failed to solve: failed to compute cache key: failed to calculate checksum of ref \"/missing-file\": not found
------
 > [2/3] COPY missing-file /x:
------
   1 | FROM alpine:3@sha256:3f4d6b32e26703c6082839234f43769d5046b3ac7c910033911d1e71d8cd630a
   2 | >>> COPY missing-file /x
   3 |
------
ERROR: failed to build: failed to solve: failed to compute cache key: failed to calculate checksum of ref \"/missing-file\": not found";

#[test]
fn step_three_plans_a_build_for_both_absent_images_and_then_builds_them() {
    // The first deploy on a machine that has never built either image: the read is two "not on this machine"
    // reasons, a dry run prints them and builds nothing, and the run builds both — proxy first, because step 4
    // needs it.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-images-absent");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let planned = deploy::run(&deployed, &target, &tools, &machine, true);
    let step = line(&planned, DeployStep::Images);
    assert_eq!(step.word, Word::Needs, "{}", step.sentence);
    for image in [images::proxy_image(), images::agent_image()] {
        assert!(
            step.sentence
                .contains(&format!("{image} is not on this machine")),
            "the plan does not name {image}'s absence: {}",
            step.sentence
        );
    }
    assert!(
        machine.builds().is_empty(),
        "a dry run built an image: {:?}",
        machine.builds()
    );

    let report = deploy::run(&deployed, &target, &tools, &machine, false);
    assert!(!report.failed(), "the build failed:\n{}", report.render());
    let step = line(&report, DeployStep::Images);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    assert!(
        step.sentence.contains(&images::proxy_image())
            && step.sentence.contains(&images::agent_image()),
        "the change does not name both tags: {}",
        step.sentence
    );
    let builds = machine.builds();
    assert_eq!(
        builds.len(),
        2,
        "step 3 did not build both images: {builds:?}"
    );
    assert_eq!(
        builds[0].0,
        images::proxy_image(),
        "the agent image was built before the proxy image, whose container step 4 starts"
    );
    // The argument vector is `build_arguments`' own: the flags, then `-` last, because the context is the archive
    // on stdin.
    let (_, args) = &builds[0];
    assert_eq!(args.first().map(String::as_str), Some("build"));
    assert_eq!(args.last().map(String::as_str), Some("-"));
    assert!(
        args.iter().any(|argument| argument
            == &format!("HOLOSMITH_VERSION={}", env!("CARGO_PKG_VERSION"))),
        "the build carries no version argument: {args:?}"
    );
    assert!(
        args.iter().any(|argument| argument
            == &format!("HOLOSMITH_PI_VERSION={}", holosmith::pins::pi_version())),
        "the build carries no pinned pi: {args:?}"
    );

    // The build left the images the read compares with, so a second run is honestly satisfied — the whole point of
    // the labels.
    let second = deploy::run(&deployed, &target, &tools, &machine, false);
    let step = line(&second, DeployStep::Images);
    assert_eq!(step.word, Word::Satisfied, "{}", step.sentence);
    assert_eq!(machine.builds().len(), 2, "the second run built again");
}

#[test]
fn step_three_is_satisfied_when_both_images_carry_this_binarys_labels() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-images-current");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&config);
    let machine = Fake::new()
        .with_labels(
            &images::proxy_image(),
            expected_labels(Kind::Proxy, &deployed),
        )
        .with_labels(
            &images::agent_image(),
            expected_labels(Kind::Agent, &deployed),
        );

    let report = deploy::run(&deployed, &target, &real_tools(&dir), &machine, false);

    let step = line(&report, DeployStep::Images);
    assert_eq!(step.word, Word::Satisfied, "{}", step.sentence);
    for fact in [
        images::proxy_image(),
        images::agent_image(),
        env!("CARGO_PKG_VERSION").to_string(),
        holosmith::pins::pi_version().to_string(),
        "python3 ripgrep".to_string(),
    ] {
        assert!(
            step.sentence.contains(&fact),
            "the read does not name {fact}: {}",
            step.sentence
        );
    }
    assert!(
        machine.builds().is_empty(),
        "an image whose labels are this binary's was built again: {:?}",
        machine.builds()
    );
}

#[test]
fn step_three_names_the_label_that_differs() {
    // The read is the labels and not the tag: the agent image is there under its right name and says another pi
    // version, which is exactly the image an upgrade has to rebuild.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-images-stale");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&config);
    let mut agent = expected_labels(Kind::Agent, &deployed);
    agent.insert(
        images::LABEL_PI.to_string(),
        "0.0.0-not-the-pinned-one".to_string(),
    );
    let machine = Fake::new()
        .with_labels(
            &images::proxy_image(),
            expected_labels(Kind::Proxy, &deployed),
        )
        .with_labels(&images::agent_image(), agent);

    let planned = deploy::run(&deployed, &target, &real_tools(&dir), &machine, true);
    let step = line(&planned, DeployStep::Images);
    assert_eq!(step.word, Word::Needs, "{}", step.sentence);
    for fact in [
        images::agent_image(),
        "pi-version".to_string(),
        "0.0.0-not-the-pinned-one".to_string(),
        holosmith::pins::pi_version().to_string(),
    ] {
        assert!(
            step.sentence.contains(&fact),
            "the reason does not name {fact}: {}",
            step.sentence
        );
    }
    // The proxy image is current, so it is not in the plan: one action, not two.
    assert!(
        !step.sentence.contains(&images::proxy_image()),
        "a current image is in the plan for a build: {}",
        step.sentence
    );
    assert!(
        step.sentence.starts_with("1 action:"),
        "the plan does not count one build: {}",
        step.sentence
    );
}

#[test]
fn step_three_refuses_a_daemon_that_builds_for_another_architecture() {
    // A `docker_host` may point at a socket elsewhere, and an image built there has a base for that machine: the
    // guard is what turns "the proxy will not start" into a deploy that says why.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-images-arch");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&config);
    let elsewhere = if daemon_word() == "amd64" {
        "arm64"
    } else {
        "amd64"
    };
    let machine = Fake::new().with_arch(elsewhere);

    let report = deploy::run(&deployed, &target, &real_tools(&dir), &machine, false);

    assert_eq!(
        report.failed,
        Some(DeployStep::Images),
        "{}",
        report.render()
    );
    let sentence = &line(&report, DeployStep::Images).sentence;
    let binary = env!("HOLOSMITH_TARGET")
        .split('-')
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(
        sentence.contains(elsewhere) && sentence.contains(&binary),
        "the refusal does not name both architectures: {sentence}"
    );
    assert!(
        machine.builds().is_empty(),
        "a build was attempted for another architecture: {:?}",
        machine.builds()
    );
    assert_eq!(report.lines.len(), 3, "the run did not stop at step 3");
}

#[test]
fn a_failed_build_is_step_threes_own_sentence_and_stops_the_run() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-images-failure");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&config);
    let machine = Fake::new().with_failing_build(COPY_FAILURE);

    let report = deploy::run(&deployed, &target, &real_tools(&dir), &machine, false);

    assert_eq!(
        report.failed,
        Some(DeployStep::Images),
        "{}",
        report.render()
    );
    let sentence = &line(&report, DeployStep::Images).sentence;
    assert!(
        sentence.contains("COPY missing-file /x"),
        "the failure does not name the instruction: {sentence}"
    );
    assert!(
        !sentence.contains('\n'),
        "the failure is the whole build log rather than one sentence: {sentence}"
    );
    // The proxy image is built first, so its failure is the only build the run asked for.
    assert_eq!(machine.builds().len(), 1, "{:?}", machine.builds());
    assert_eq!(
        machine.builds()[0].0,
        images::proxy_image(),
        "the step did not build the proxy image first"
    );
    assert!(
        !report.render().contains("4. proxies:"),
        "a failed build did not stop the run:\n{}",
        report.render()
    );
}

// ---- step 4 and the refusal --------------------------------------------------------------

#[test]
fn step_four_starts_both_proxies_and_the_second_run_reads_running() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-proxies");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let root = dir.file("root");
    let target = Target::resolve(Some(root));
    let deployed = target.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let first = deploy::run(&deployed, &target, &tools, &machine, false);
    assert!(!first.failed(), "the first run failed:\n{}", first.render());
    let step = line(&first, DeployStep::Proxies);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    let calls = machine.calls();
    let egress = calls
        .iter()
        .find(|argv| {
            argv.iter()
                .any(|argument| argument == container::PROXY_NAME)
        })
        .unwrap_or_else(|| panic!("the egress proxy was never started: {calls:?}"));
    // The recipe `serve` runs, argument for argument: the name, the machine's network, the restart policy and the
    // internal subcommand that is what the container actually is.
    for expected in [
        "--name",
        container::PROXY_NAME,
        "--network",
        docker::EGRESS_NETWORK,
        "--restart",
        "unless-stopped",
        "proxy",
        "egress",
    ] {
        assert!(
            egress.iter().any(|argument| argument == expected),
            "the egress proxy's argv has no {expected}: {egress:?}"
        );
    }
    let key_proxy = calls
        .iter()
        .find(|argv| argv.iter().any(|argument| argument == key::KEY_PROXY_NAME))
        .unwrap_or_else(|| panic!("the key proxy was never started: {calls:?}"));
    assert!(
        key_proxy.iter().any(|argument| argument == "key"),
        "the key proxy's argv does not run the key subcommand: {key_proxy:?}"
    );
    // The network was missing, so it was created before the containers that attach to it.
    assert!(
        calls
            .first()
            .is_some_and(|argv| argv.first().is_some_and(|word| word == "network")),
        "the egress network was not created first: {calls:?}"
    );

    let second = deploy::run(&deployed, &target, &tools, &machine, false);
    assert!(
        !second.failed(),
        "the second run failed:\n{}",
        second.render()
    );
    let step = line(&second, DeployStep::Proxies);
    assert_eq!(step.word, Word::Satisfied, "{}", step.sentence);
    assert!(
        step.sentence.contains(docker::EGRESS_NETWORK),
        "the read does not name the network it found: {}",
        step.sentence
    );
    assert!(
        step.sentence.contains(container::PROXY_NAME)
            && step.sentence.contains(key::KEY_PROXY_NAME),
        "the read does not name both containers: {}",
        step.sentence
    );
    assert_eq!(
        machine.calls().len(),
        calls.len(),
        "the second run replaced a proxy that was already running"
    );
}

#[test]
fn a_hub_role_is_refused() {
    let hub = Config {
        role: Role::Hub,
        ..Config::default()
    };
    let sentence = deploy::refusal(&hub).expect("a hub is refused");
    assert!(sentence.contains("hub"), "{sentence}");
    assert!(
        sentence.contains("runs no sessions"),
        "the refusal does not say why a hub installs no server: {sentence}"
    );
    assert!(
        sentence.contains("hub deploy") && sentence.contains("phase 3"),
        "the refusal does not name the command that does: {sentence}"
    );
    assert!(sentence.contains("HS-072"), "{sentence}");
    // A server and a `both` machine are deployable: this is one role's refusal, not a machine's.
    assert_eq!(deploy::refusal(&Config::default()), None);
    assert_eq!(
        deploy::refusal(&Config {
            role: Role::Server,
            ..Config::default()
        }),
        None
    );
}

#[test]
fn a_fresh_install_still_measures_the_disk_floor_the_data_root_will_land_on() {
    // The floor exists for the **first** deploy, and on a fresh host the data root does not exist yet — the state
    // `preflight::disk` answers `unknown` for. Without the re-ask this step would say `satisfied` with
    // `1 unknown (disk)` and install into a full filesystem, so this test fails if the re-ask is removed: the point
    // is that the run *stops before it writes anything*.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-disk");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let root = dir.file("root");
    let target = Target::resolve(Some(root));
    let deployed = target.config(&config);
    assert!(
        !deployed.data_root.exists(),
        "this test means nothing unless the data root is absent, and the whole story is that a fresh host's is"
    );
    let mut tools = real_tools(&dir);
    // 1 MiB free against the default `min_free_gb` of 20.
    tools.doctor.preflight.df = df_shim_with(&dir, 1024);

    let report = deploy::run(&deployed, &target, &tools, &Fake::new(), false);
    let step = line(&report, DeployStep::Preflight);
    assert_eq!(
        report.failed,
        Some(DeployStep::Preflight),
        "a host with no room reported step 1 as {}:\n{}",
        step.word,
        report.render()
    );
    assert!(
        step.sentence.contains("min_free_gb"),
        "the failure does not name the floor: {}",
        step.sentence
    );
    // The ancestor that exists is what was measured, and the sentence names it — not the data root nobody has
    // created yet.
    assert!(
        step.sentence.contains(&dir.path().display().to_string()),
        "the failure does not name the path it measured: {}",
        step.sentence
    );
    assert!(
        !step.sentence.contains("no data root at"),
        "{}",
        step.sentence
    );
    assert!(
        !deployed.data_root.exists(),
        "a run that failed step 1 created the data root:\n{}",
        report.render()
    );
}

#[test]
fn step_two_says_which_half_of_the_spec_it_did_not_read() {
    // Item 10 step 2 also asks for the service user and the VAPID keys, which are HS-049's. A step that reports a
    // pass must say what it did not read, or "all 6 paths are present with their modes" reads as the whole step.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-remainder");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let first = deploy::run(&deployed, &target, &tools, &machine, false);
    assert_eq!(line(&first, DeployStep::Files).word, Word::Changed);
    let second = deploy::run(&deployed, &target, &tools, &machine, false);
    assert_eq!(line(&second, DeployStep::Files).word, Word::Satisfied);
    for report in [&first, &second] {
        let sentence = &line(report, DeployStep::Files).sentence;
        assert!(
            sentence.contains("HS-049") && sentence.contains("VAPID"),
            "step 2's line does not say which half it did not read: {sentence}"
        );
    }
    // A dry run's plan says it too, so the operator reading a plan is not told the step is complete either.
    let planned = deploy::run(&deployed, &target, &tools, &machine, true);
    assert!(
        line(&planned, DeployStep::Files)
            .sentence
            .contains("HS-049"),
        "the dry run's plan does not say which half step 2 leaves alone"
    );
}
