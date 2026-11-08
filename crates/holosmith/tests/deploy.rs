//! `server deploy`: the seven steps, the words, the install tree and the two proxies (HS-047, spec item 10).
//!
//! These tests drive [`deploy::run`] over a machine they build, and that is the same discipline `tests/doctor.rs`
//! established for the same reason: a deploy is a set of claims about *a host* — its daemon, its git, its paths —
//! so the only way to test one without becoming a test of the machine running it is to hand every claim in.
//! `git`, `df`, `tailscale` (both its `status` and its `serve status`), `sudo`, `install`, `chmod`, `systemctl`,
//! `id`, `stat`, `useradd`, `usermod` and `chown` are scripts this test wrote (the `Tools` seam), systemd is a
//! directory it leaves out, the daemon is a [`Fake`], and `--root` is what keeps the install tree inside the temp
//! directory.
//!
//! Three things are driven **for real**, because a fake would prove nothing about them: the mode and byte reads run
//! `/usr/bin/install` and `/usr/bin/chmod` over a real tree (a shim cannot show that `install -m 0600` produces a
//! 0600 file), the VAPID pair comes out of the host's own `openssl` (the DER walk is a claim about that command), and
//! step 1's port check and step 7's health wait both bind real sockets — a fake port would not exercise `doctor`'s
//! own read, and a fake answer would not exercise the wait.
//!
//! ADR-0056 is the design these assert: a step is a read and an apply, `skipped` is never `satisfied`, a failure
//! stops the run, and the proxies' recipe is the boot's own. Since HS-049 every step applies, so a run meant to reach
//! the end needs a control listener that answers `/api/health` and a `health_wait` short enough to give up on.

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

/// A `tailscale` that answers both questions the deploy asks, and records what it was told to serve.
///
/// `status --json` is the preflight's node state (step 1) and `serve status --json` is step 6's read, composed from
/// the `<port> <target>` lines in `state` — so a `serve --bg` this shim records makes the *next* read answer
/// `satisfied`, which is the whole of the second-run claim. Every other argv is appended to `log`, one line per
/// call, and any other argv exits non-zero, because a shim that answered everything would hide a step asking the
/// wrong question.
fn tailscale_serve_shim(dir: &TempDir, log: &Path, state: &Path) -> PathBuf {
    std::fs::write(state, "").expect("the serve state is writable");
    shim(
        dir,
        "tailscale",
        &format!(
            r#"log='{log}'
state='{state}'
case "$1 $2" in
  "status --json") echo '{{"BackendState":"Running"}}' ;;
  "serve status")
    printf '{{"Web":{{'
    separator=''
    while read -r port target; do
      [ -n "$port" ] || continue
      printf '%s"holosmith.example.ts.net:%s":{{"Handlers":{{"/":{{"Proxy":"%s"}}}}}}' "$separator" "$port" "$target"
      separator=','
    done < "$state"
    printf '}}}}\n'
    ;;
  "serve --bg")
    shift
    printf 'serve %s\n' "$*" >> "$log"
    port=''
    target=''
    for argument in "$@"; do
      case "$argument" in
        --https=*) port="${{argument#--https=}}" ;;
        http://*) target="$argument" ;;
      esac
    done
    printf '%s %s\n' "$port" "$target" >> "$state"
    ;;
  *) exit 1 ;;
esac"#,
            log = log.display(),
            state = state.display()
        ),
    )
}

/// The `doctor::Tools` a deploy runs: a real git (the merge trial is a measurement, and a shim would fail it), 480 GB
/// free, a `tailscale` that answers both the preflight's status and step 6's `serve status`, and no systemd.
fn doctor_tools(dir: &TempDir) -> doctor::Tools {
    doctor::Tools {
        preflight: preflight::Tools {
            git: PathBuf::from("git"),
            df: df_shim(dir),
            du: PathBuf::from("du"),
            openssl: PathBuf::from("openssl"),
        },
        tailscale: tailscale_serve_shim(
            dir,
            &dir.file("tailscale.log"),
            &dir.file("tailscale-serve"),
        ),
        systemd: dir.file("no-systemd-here"),
        ..doctor::Tools::default()
    }
}

/// The deploy's tools with the machine's own `install`, `chmod` and `openssl`, and a short health wait.
///
/// The programs are named rather than pathed, exactly as [`deploy::Tools::default`] names them, so this test asks
/// the same question production asks and follows the host's `PATH` — a repository that hard-coded `/usr/bin/install`
/// would fail on a machine whose coreutils is elsewhere, with a message about a missing file rather than about the
/// check. The key pair is generated by the host's real `openssl`, because a shim would prove nothing about the DER
/// walk; the wait is short because a test may not spend the minute a machine may.
fn real_tools(dir: &TempDir) -> deploy::Tools {
    deploy::Tools {
        doctor: doctor_tools(dir),
        health_wait: Duration::from_millis(200),
        ..deploy::Tools::default()
    }
}

/// A `sudo` that records and runs nothing.
///
/// **It must not exec anything**: the system-path run names `/etc/holosmith` and `/var/lib/holosmith`, and a shim
/// that forwarded to the real `sudo` would install into the machine running the tests. Recording the argv and
/// exiting 0 is the whole of what the system-path tests need to know.
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

/// An `id` answering the two questions step 2 asks — `-u` with `uid`, and `-nG` with `groups` — or exiting non-zero
/// for both, which is [`None`]: no such user.
fn id_shim(dir: &TempDir, uid: Option<&str>, groups: &str) -> PathBuf {
    let body = match uid {
        Some(uid) => format!(
            "case \"$1\" in\n  -u) echo {uid} ;;\n  -nG) echo '{groups}' ;;\n  *) exit 1 ;;\nesac"
        ),
        None => "exit 1".to_string(),
    };
    shim(dir, "id", &body)
}

/// A `stat -c %u` answering `uid`, which is what the data root's owner is compared with.
fn stat_shim(dir: &TempDir, uid: &str) -> PathBuf {
    shim(dir, "stat", &format!("echo {uid}"))
}

/// A `systemctl` that records its argv and answers `is-enabled` with `enabled`.
///
/// The exit code is the answer step 5 reads, so `enabled` is the whole knob: `true` records and exits 0, `false`
/// records and exits 1 (`systemctl`'s own "disabled").
fn systemctl_shim(dir: &TempDir, log: &Path, enabled: bool) -> PathBuf {
    let code = if enabled { 0 } else { 1 };
    shim(
        dir,
        "systemctl",
        &format!(
            "printf 'systemctl %s\\n' \"$*\" >> '{}'\n[ \"$1\" = is-enabled ] && exit {code}\nexit 0",
            log.display()
        ),
    )
}

/// The deploy's tools over recording shims: `calls` gets every `install`/`chmod` argv, `sudo_calls` every argv the
/// deploy prefixed with `sudo`.
///
/// `systemctl` is a shim as well, answering "disabled", so step 5 applies and no real `systemctl` is asked about a
/// unit that is not on this machine. The questions that decide *which* children run — `id` and `stat` — are the
/// caller's to name, because their answers are what the arms differ in; a test that names none gets the host's own
/// programs, which is what a `--root` run's steps never ask.
fn recording_tools(dir: &TempDir, calls: &Path, sudo_calls: &Path) -> deploy::Tools {
    deploy::Tools {
        doctor: doctor_tools(dir),
        sudo: sudo_recorder(dir, sudo_calls),
        install: recorder(dir, "install", calls),
        chmod: recorder(dir, "chmod", calls),
        systemctl: systemctl_shim(dir, &dir.file("systemctl.log"), false),
        health_wait: Duration::from_millis(200),
        ..deploy::Tools::default()
    }
}

/// The body a stand-in for `/api/health` answers: `Health`'s five keys, because the reader parses them (HS-050) —
/// a `200` with anything else is "a port somebody else holds", which is a different fact from "this machine serves".
pub const HEALTH_PAYLOAD: &str = r#"{"name":"holosmith","version":"verify","role":"server","uptime_s":0,"preview_origin":"http://localhost:8081"}"#;

/// A control listener that answers `GET /api/health` for the rest of the test's life, and its address.
///
/// Step 7's apply waits on this, and `doctor --full`'s port check reads it as "the machine is serving", so every run
/// meant to reach the end needs one — bound on the kernel's port and handed back, because the config's
/// `control_listen` has to be the address it landed on.
fn health_responder() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port is bindable");
    let listen = listener.local_addr().expect("the bound address is known");
    std::thread::spawn(move || {
        let answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{HEALTH_PAYLOAD}",
            HEALTH_PAYLOAD.len()
        );
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(answer.as_bytes());
        }
    });
    listen
}

/// A data root that exists with the mode a deploy writes, and its 0700 secrets directory.
///
/// The fixture a **system-path** arm needs, because the two things a recorder cannot do are the things `doctor`'s
/// `perms` finding and step 4 read: the deploy's `install` creates the tree (a recorder creates nothing), and step 4
/// makes the key store itself through `key::Store::ensure_file`. A test whose subject is a write, not a tree, starts
/// from the tree a previous deploy left — and the store's own choice of the data root's mode is `key.rs`'s unit
/// test (`the_data_root_the_key_store_makes_is_the_mode_the_deploy_writes`), not this fixture's.
fn deployed_data_root(path: &Path) {
    std::fs::create_dir_all(path).expect("the data root is creatable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("the data root's mode is settable");
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
    /// What the network self-test answers, which is a *finding* in step 7's `doctor --full` rather than a machine
    /// fact this deploy changes.
    selftest: Mutex<selftest::Outcome>,
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
            selftest: Mutex::new(selftest::Outcome::Passed),
        }
    }

    /// The same machine, with a self-test that measures `outcome` — step 7's failure arm.
    fn with_selftest(self, outcome: selftest::Outcome) -> Self {
        *self
            .selftest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = outcome;
        self
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
            "HOLOSMITH_BINARY_SHA256" => images::LABEL_BINARY,
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
    // The digest this process would build from, memoised by the product itself: the test binary is tens of
    // megabytes and an unoptimised SHA-256 is slow, and the production path asks the same question.
    let digest = images::own_digest().expect("the test binary is readable");
    let expectation = Expectation::of_digest(kind, config, &digest);
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
    /// The state the same fake answers [`proxies::Containers`]' question with. Step 7 runs `doctor --full`, which
    /// reads the two proxies, so a run meant to reach the end has to see what step 4 started — otherwise the deploy
    /// would fail on a finding about the containers it had just brought up (HS-049).
    fn running(&self, name: &str) -> bool {
        <Self as proxies::Containers>::running(self, name)
    }

    /// This build's pinned pi, because `--full` asks the agent image for it and an `Err` here would be a finding
    /// rather than a measurement.
    fn image_pi(&self, _image: &str) -> Result<String, String> {
        Ok(holosmith::pins::pi_version().to_string())
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
        self.selftest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
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
fn the_report_prints_the_tally_and_no_owner_list() {
    // A report built by hand: the shape is the acceptance's, and the claims that matter are the summary's four
    // counts (always present, so a reader can diff two runs) and the absence of the owner list HS-049 removed —
    // every step has a body now, so a line naming who owns what remains would name a story that is done.
    let report = Report {
        lines: vec![
            Line {
                step: DeployStep::Preflight,
                word: Word::Satisfied,
                sentence: "10 findings: 8 pass, 0 fail, 2 unknown (systemd, disk)".to_string(),
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
                word: Word::Changed,
                sentence:
                    "2 actions: install -m 0644 <staged> /etc/systemd/system/holosmith.service"
                        .to_string(),
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
        "1. preflight: satisfied: 10 findings: 8 pass, 0 fail, 2 unknown (systemd, disk)"
    );
    // The tally's four counts are the whole summary: `skipped` prints as zero and the `incomplete:` line is gone.
    assert_eq!(
        lines[7],
        "deploy: 5 steps: 1 satisfied, 3 changed, 0 skipped, 1 failed"
    );
    assert_eq!(lines.len(), 8);
    assert!(
        !text.contains("incomplete"),
        "the report still claims a story owns a step:\n{text}"
    );
    assert!(report.failed());

    // A dry run says so, and a report with nothing failed has no resume claim to make.
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
    // Step 1's plan is its read, step 3's is a build of both images, and steps 5, 6 and 7 are plans too: a dry run
    // reads the machine for every step, so nothing is left to a later story any more (HS-049).
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
    // Step 5's plan names the unit's path, step 6's the two origins, step 7's the wait and the second reading.
    let service = line(&report, DeployStep::Service);
    assert_eq!(service.word, Word::Needs, "{}", service.sentence);
    assert!(
        service
            .sentence
            .contains(&target.unit_path().display().to_string()),
        "step 5's plan does not name the unit file: {}",
        service.sentence
    );
    let origins = line(&report, DeployStep::Origins);
    assert_eq!(origins.word, Word::Needs, "{}", origins.sentence);
    for fact in [
        "--https=443".to_string(),
        "--https=8444".to_string(),
        format!("http://127.0.0.1:{}", deployed.control_listen.port()),
    ] {
        assert!(
            origins.sentence.contains(&fact),
            "step 6's plan does not name {fact}: {}",
            origins.sentence
        );
    }
    let health = line(&report, DeployStep::Health);
    assert_eq!(health.word, Word::Needs, "{}", health.sentence);
    assert!(
        health.sentence.contains("/api/health") && health.sentence.contains("doctor --full"),
        "step 7's plan is not the wait and the second reading: {}",
        health.sentence
    );
    for step in DeployStep::ALL {
        assert_ne!(
            line(&report, step).word,
            Word::Skipped,
            "{step} is still skipped, and this release has no producer for that word"
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
fn an_install_creates_the_install_tree_with_its_modes() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-install");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    // Step 7 waits for this answer, so the control listener is a real one that gives it.
    config.control_listen = health_responder();
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
        // The pair step 2 generates, and the unit step 5 renders: both are part of the tree an install leaves.
        (root.join("var/lib/holosmith/secrets/vapid.toml"), 0o600),
        (root.join("etc/systemd/system/holosmith.service"), 0o644),
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
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
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
    // Every step, not just the three that were the whole install in HS-047: a second deploy changes nothing at all.
    for step in DeployStep::ALL {
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
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    let root = dir.file("root");
    let target = Target::resolve(Some(root.clone()));
    let deployed = target.config(&config);
    // The tree a previous deploy left, with a real key in the store **and modes a looser umask left behind**: the
    // two things a deploy must handle differently. `install` would truncate the bytes, so a mode it does own is
    // repaired with `chmod`, which touches nothing — and the fixture starts wrong on purpose, because a mode that is
    // already right makes the repair's only assertion a comparison of the fixture with itself (HS-049's review,
    // STD-1: with the fixture at 0600, deleting `Kind::Kept`'s `Action::Mode` arm left the whole suite green).
    deployed_data_root(&deployed.data_root);
    let secrets = deployed.data_root.join(key::SECRETS_DIR);
    std::fs::create_dir_all(&secrets).expect("the secrets directory is creatable");
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o755))
        .expect("the secrets directory's mode is settable");
    let keys = secrets.join(key::KEYS_FILE);
    let stored = "[providers]\nanthropic = \"sk-ant-not-a-real-key\"\n[sessions]\n";
    std::fs::write(&keys, stored).expect("the key file is writable");
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o644))
        .expect("the key file's mode is settable");

    // Step 1 reads the same wrong modes — and only *remarks* on them, because repairing them is this step's job
    // (ADR-0058). A failure here would be the dead end the review called SPEC-2.
    let report = deploy::run(&deployed, &target, &real_tools(&dir), &Fake::new(), false);
    assert!(!report.failed(), "the install failed:\n{}", report.render());
    assert!(
        line(&report, DeployStep::Preflight)
            .sentence
            .contains("step 2 repairs"),
        "step 1 did not remark on the wrong modes it read: {}",
        line(&report, DeployStep::Preflight).sentence
    );

    assert_eq!(
        std::fs::read_to_string(&keys).expect("the key file is readable"),
        stored,
        "the deploy rewrote the file that holds every stored provider key"
    );
    assert_eq!(mode(&keys), 0o600, "the key file's mode was not repaired");
    assert_eq!(
        mode(&secrets),
        0o700,
        "the secrets directory's mode was not repaired"
    );
    // The store is repaired with `chmod` and never with `install`: an `install` of that path would truncate the
    // bytes above, and the words say which write the step chose instead.
    let changed = line(&report, DeployStep::Files).sentence.clone();
    // A directory is repaired by re-running `install -d` (which truncates nothing), a file by `chmod` — the two
    // spellings `Action` has, and both are asserted here so neither arm can lose its detector.
    for expected in [
        format!("chmod 0600 {}", keys.display()),
        format!("install -d -m 0700 {}", secrets.display()),
    ] {
        assert!(
            changed.contains(&expected),
            "the mode repair is not the action step 2 reported ({expected}): {changed}"
        );
    }
    assert!(
        !changed.contains(&format!("<staged> {}", keys.display())),
        "the deploy planned an `install` over the key file it must never truncate: {changed}"
    );

    // A mode that is wrong is repaired, and repaired with `chmod` rather than with the truncating `install`: the
    // config file's bytes are already this deploy's, so only its mode can be wrong.
    let config_file = root.join("etc/holosmith/config.toml");
    std::fs::set_permissions(&config_file, std::fs::Permissions::from_mode(0o600))
        .expect("the config file's mode is settable");
    let report = deploy::run(&deployed, &target, &real_tools(&dir), &Fake::new(), false);
    let changed = line(&report, DeployStep::Files).sentence.clone();
    assert!(
        changed.contains(&format!("chmod 0644 {}", config_file.display())),
        "the mode repair is not the action step 2 reported: {changed}"
    );
    assert_eq!(mode(&config_file), 0o644, "the mode was not repaired");
    assert_eq!(
        std::fs::read_to_string(&keys).expect("the key file is readable"),
        stored,
        "the deploy rewrote the file that holds every stored provider key"
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
    let mut rooted = config(Some(format!("unix://{}", socket.display())));
    rooted.control_listen = health_responder();
    let target = Target::resolve(Some(root));
    let deployed = target.config(&rooted);
    // The tree a previous deploy left: step 4 makes the key store itself, and step 7's `perms` reads the modes, so a
    // recorder run has to start from a data root that exists with the mode the deploy writes.
    deployed_data_root(&deployed.data_root);
    let calls = dir.file("root-calls.log");
    let sudo_calls = dir.file("root-sudo.log");
    // No such user, so the two runs' step 2 is the same seven items with the same children on either path.
    let id = id_shim(&dir, None, "");
    let report = deploy::run(
        &deployed,
        &target,
        &deploy::Tools {
            id: id.clone(),
            ..recording_tools(&dir, &calls, &sudo_calls)
        },
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
    let direct: Vec<String> = recorded(&calls)
        .into_iter()
        .filter(|call| !call.contains("/etc/systemd/system"))
        .collect();
    assert_eq!(
        direct.len(),
        actions(&report),
        "step 2 reported {} actions and the shims ran {direct:?}",
        actions(&report)
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

    // The system run: same actions, each one prefixed by `sudo`. The shims run nothing, so `/etc` is untouched —
    // and the data root is the temp directory's, rather than the machine's own `/var/lib/holosmith`, because a
    // recorder run must not chmod a real tree.
    let mut system = config(Some(format!("unix://{}", socket.display())));
    system.control_listen = health_responder();
    system.data_root = dir.file("system-data");
    let target = Target::resolve(None);
    let deployed = target.config(&system);
    deployed_data_root(&deployed.data_root);
    let calls = dir.file("system-calls.log");
    let sudo_calls = dir.file("system-sudo.log");
    let report = deploy::run(
        &deployed,
        &target,
        &deploy::Tools {
            id,
            ..recording_tools(&dir, &calls, &sudo_calls)
        },
        &Fake::new(),
        false,
    );
    assert!(
        !report.failed(),
        "the system run failed:\n{}",
        report.render()
    );
    // Step 5 writes its own unit file through `sudo` too, and starts systemd with it: this is step 2's count, so the
    // unit's install and the two `systemctl` verbs are not part of it.
    let prefixed: Vec<String> = recorded(&sudo_calls)
        .into_iter()
        .filter(|call| !call.contains("/etc/systemd/system") && !call.contains("systemctl"))
        .collect();
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
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.data_root = dir.file("data");
    let target = Target::resolve(None);
    let deployed = target.config(&config);
    deployed_data_root(&deployed.data_root);
    let missing = dir.file("no-sudo-here");
    let tools = deploy::Tools {
        doctor: doctor_tools(&dir),
        sudo: missing.clone(),
        install: PathBuf::from("/usr/bin/install"),
        chmod: PathBuf::from("/usr/bin/chmod"),
        // No such user, so the first pending action is `useradd` — the child the missing `sudo` cannot run.
        id: id_shim(&dir, None, ""),
        ..deploy::Tools::default()
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
        sentence.contains("useradd --system --groups docker"),
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
    // promises is safe (ADR-0056). It answers `/api/health`, because that is what makes a held port "this machine
    // serving" rather than somebody else's process — step 7 waits on the same answer.
    let listen = health_responder();
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
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
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
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
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
fn step_three_sees_a_rebuilt_artifact_through_the_digest_label() {
    // The review's P1: `git describe --dirty` is the same for two builds of one tree, so version + tag + pi
    // compared equal over an image holding the *previous* binary while `hs_egress` kept serving it. Every label is
    // this binary's except the digest — which is the state a rebuild that did not bump anything produces — and the
    // step must plan a build that names it. Delete the `LABEL_BINARY` arm of `mismatches` and this test fails with
    // `satisfied`.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-images-rebuilt");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&config);
    let mut agent = expected_labels(Kind::Agent, &deployed);
    agent.insert(images::LABEL_BINARY.to_string(), "0".repeat(64));
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
        "binary-sha256".to_string(),
        "0".repeat(64),
        images::own_digest().expect("this test's own binary is readable"),
    ] {
        assert!(
            step.sentence.contains(&fact),
            "the reason does not name {fact}: {}",
            step.sentence
        );
    }
    assert!(
        !step.sentence.contains(&images::proxy_image()),
        "a current image is in the plan for a build: {}",
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
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
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
fn a_root_install_says_which_half_of_step_two_it_did_not_do() {
    // A `--root` install creates no service user and runs no chown, because the tree is the invoking user's. A step
    // that reports a pass must say so, or "all 7 paths are present with their modes" reads as the whole of item 10
    // step 2 when the user and the ownership were never read (HS-049).
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-root-remark");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let first = deploy::run(&deployed, &target, &tools, &machine, false);
    assert_eq!(line(&first, DeployStep::Files).word, Word::Changed);
    let second = deploy::run(&deployed, &target, &tools, &machine, false);
    assert_eq!(line(&second, DeployStep::Files).word, Word::Satisfied);
    let planned = deploy::run(&deployed, &target, &tools, &machine, true);
    for report in [&first, &second] {
        let sentence = &line(report, DeployStep::Files).sentence;
        assert!(
            sentence.contains("; --root: no service user is created"),
            "step 2's line does not say what a --root install did not do: {sentence}"
        );
    }
    // The dry run's plan already carries it: the plan for a `--root` run is not the plan for a system one.
    assert!(
        line(&planned, DeployStep::Files)
            .sentence
            .contains("; --root: no service user is created"),
        "the dry run's plan does not say what step 2 leaves alone: {}",
        line(&planned, DeployStep::Files).sentence
    );
    // The satisfied line names the half it *did* do — the tree, the user it would have created, and the pair.
    let satisfied = line(&second, DeployStep::Files).sentence.clone();
    assert!(
        satisfied.contains("all 7 paths are present with their modes"),
        "step 2's pass does not count the paths it read: {satisfied}"
    );
}

// ---- the service user, the pair and the ownership (HS-049) -------------------------------

#[test]
fn the_service_user_and_its_docker_group_are_read_before_they_are_made() {
    // `useradd` and `usermod` do not exist on every machine (this one has neither), so the three arms are read
    // through the recorder: what the deploy *would* run is the claim, and nothing outside the temp directory is
    // touched.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-user");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    // A data root inside the temp directory, so the only thing this test's arms differ in is the `id` answer.
    config.data_root = dir.file("data");
    let target = Target::resolve(None);
    let deployed = target.config(&config);
    deployed_data_root(&deployed.data_root);
    let etc = Path::new("/etc/holosmith").exists();

    // 1. No such user: the system user, in the daemon's group, with the data root as its home.
    let calls = dir.file("calls.log");
    let sudo_calls = dir.file("sudo.log");
    let report = deploy::run(
        &deployed,
        &target,
        &deploy::Tools {
            id: id_shim(&dir, None, ""),
            ..recording_tools(&dir, &calls, &sudo_calls)
        },
        &Fake::new(),
        false,
    );
    let created = format!(
        "sudo useradd --system --groups docker --home-dir {} --shell /usr/sbin/nologin holosmith",
        deployed.data_root.display()
    );
    assert!(
        recorded(&sudo_calls).contains(&created),
        "the service user was not created with the spec's argv ({created}):\n{}",
        report.render()
    );

    // 2. The user exists and the group does not have it: one `usermod`, never a second `useradd`.
    std::fs::remove_file(&sudo_calls).expect("the recorder log is removable");
    let report = deploy::run(
        &deployed,
        &target,
        &deploy::Tools {
            id: id_shim(&dir, Some("4242"), "holosmith"),
            ..recording_tools(&dir, &calls, &sudo_calls)
        },
        &Fake::new(),
        false,
    );
    let added = recorded(&sudo_calls);
    assert!(
        added.contains(&"sudo usermod -aG docker holosmith".to_string()),
        "a user outside the docker group was not added to it: {added:?}\n{}",
        report.render()
    );
    assert!(
        !added.iter().any(|call| call.contains("useradd")),
        "a user that exists was created again: {added:?}"
    );

    // 3. The user is there and in the group: no child at all, and the line names what it read.
    std::fs::remove_file(&sudo_calls).expect("the recorder log is removable");
    let report = deploy::run(
        &deployed,
        &target,
        &deploy::Tools {
            id: id_shim(&dir, Some("4242"), "holosmith docker"),
            ..recording_tools(&dir, &calls, &sudo_calls)
        },
        &Fake::new(),
        false,
    );
    let quiet = recorded(&sudo_calls);
    assert!(
        !quiet.iter().any(|call| call.contains("useradd")),
        "the deploy created a user that is there: {quiet:?}"
    );
    assert!(
        !quiet.iter().any(|call| call.contains("usermod")),
        "the deploy added a user to the group it is already in: {quiet:?}"
    );
    // The tree itself is installed by the recorders, which create nothing, so step 2 is still `changed` — the
    // *user* half is what answered "nothing to do".
    assert!(
        !line(&report, DeployStep::Files)
            .sentence
            .contains("useradd")
            && !line(&report, DeployStep::Files)
                .sentence
                .contains("usermod"),
        "step 2's plan names a user write it did not need: {}",
        line(&report, DeployStep::Files).sentence
    );
    assert_eq!(
        Path::new("/etc/holosmith").exists(),
        etc,
        "a recording run touched the machine's own config directory"
    );
}

#[test]
fn the_install_tree_is_chowned_to_the_service_user_only_when_it_is_not() {
    // The two uids decide it, and `stat` is asked about a data root that exists — inside the temp directory, on a
    // system-path target, so `stat`'s answer is the shim's and the writes are recorded rather than run.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-chown");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.data_root = dir.file("data");
    let target = Target::resolve(None);
    let deployed = target.config(&config);
    deployed_data_root(&deployed.data_root);
    let calls = dir.file("calls.log");
    let sudo_calls = dir.file("sudo.log");

    // The data root is root's and the service user is not, which is what a `sudo install` leaves behind: one
    // recursive chown of the tree and one each for the two paths whose own directory is not in it.
    let report = deploy::run(
        &deployed,
        &target,
        &deploy::Tools {
            id: id_shim(&dir, Some("4242"), "holosmith docker"),
            stat: stat_shim(&dir, "0"),
            ..recording_tools(&dir, &calls, &sudo_calls)
        },
        &Fake::new(),
        false,
    );
    let chowns = recorded(&sudo_calls);
    for expected in [
        format!(
            "sudo chown -R holosmith:holosmith {}",
            config.data_root.display()
        ),
        "sudo chown holosmith:holosmith /etc/holosmith/config.toml".to_string(),
        "sudo chown holosmith:holosmith /run/holosmith".to_string(),
    ] {
        assert!(
            chowns.contains(&expected),
            "the tree was not given to the service user ({expected}): {chowns:?}\n{}",
            report.render()
        );
    }
    assert!(
        !chowns.iter().any(|call| call.contains("useradd")),
        "a user that exists was created again: {chowns:?}"
    );

    // The same tree, already the service user's: no chown at all.
    std::fs::remove_file(&sudo_calls).expect("the recorder log is removable");
    let report = deploy::run(
        &deployed,
        &target,
        &deploy::Tools {
            id: id_shim(&dir, Some("4242"), "holosmith docker"),
            stat: stat_shim(&dir, "4242"),
            ..recording_tools(&dir, &calls, &sudo_calls)
        },
        &Fake::new(),
        false,
    );
    let chowns = recorded(&sudo_calls);
    assert!(
        // `starts_with` rather than `contains`: the temp directory's own name has "chown" in it.
        !chowns.iter().any(|call| call.starts_with("sudo chown ")),
        "the deploy chowned a tree that was already the service user's: {chowns:?}\n{}",
        report.render()
    );
}

#[test]
fn vapid_keys_are_generated_once_and_never_rewritten() {
    // The one arm that runs for real: `install`, `chmod` and `openssl` are the machine's own, because the claim is
    // about the bytes a real command writes.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-vapid");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    let root = dir.file("root");
    let target = Target::resolve(Some(root.clone()));
    let deployed = target.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let first = deploy::run(&deployed, &target, &tools, &machine, false);
    assert!(!first.failed(), "the first run failed:\n{}", first.render());
    let step = line(&first, DeployStep::Files);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    assert!(
        step.sentence
            .contains("openssl ecparam -name prime256v1 -genkey"),
        "step 2 does not name the child that generated the pair: {}",
        step.sentence
    );
    let path = holosmith::vapid::store(&deployed.data_root);
    assert!(path.exists(), "{} was not generated", path.display());
    assert_eq!(mode(&path), 0o600, "the pair is not 0600");
    let text = std::fs::read_to_string(&path).expect("the generated pair is readable");
    let pair = holosmith::vapid::parse(&text).expect("the generated file is a pair");
    // The two lengths a Web Push consumer relies on: a 65-byte point is 87 characters and a 32-byte scalar 43.
    assert_eq!(pair.public_key.len(), 87, "{}", pair.public_key);
    assert_eq!(pair.private_key.len(), 43, "{}", pair.private_key);
    let point = holosmith::vapid::unbase64url(&pair.public_key).expect("the point is base64url");
    let scalar = holosmith::vapid::unbase64url(&pair.private_key).expect("the scalar is base64url");
    assert_eq!(point.len(), 65);
    assert_eq!(point[0], 0x04, "the point is not uncompressed");
    assert_eq!(scalar.len(), 32);

    let bytes = std::fs::read(&path).expect("the pair is readable");
    let second = deploy::run(&deployed, &target, &tools, &machine, false);
    assert!(
        !second.failed(),
        "the second run failed:\n{}",
        second.render()
    );
    assert_eq!(
        line(&second, DeployStep::Files).word,
        Word::Satisfied,
        "the second run did not read the pair it found: {}",
        line(&second, DeployStep::Files).sentence
    );
    assert_eq!(
        std::fs::read(&path).expect("the pair is readable"),
        bytes,
        "the second run rewrote the key pair"
    );
}

#[test]
fn a_vapid_file_that_is_not_a_pair_fails_the_step_and_is_not_regenerated() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-vapid-garbage");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let root = dir.file("root");
    let target = Target::resolve(Some(root));
    let deployed = target.config(&config);
    deployed_data_root(&deployed.data_root);
    let path = holosmith::vapid::store(&deployed.data_root);
    let secrets = path.parent().expect("the pair lives in a directory");
    std::fs::create_dir_all(secrets).expect("the secrets directory is creatable");
    std::fs::set_permissions(secrets, std::fs::Permissions::from_mode(0o700))
        .expect("the secrets directory's mode is settable");
    let garbage = "this is not a key pair\n";
    std::fs::write(&path, garbage).expect("the pair path is writable");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("the pair's mode is settable");

    let report = deploy::run(&deployed, &target, &real_tools(&dir), &Fake::new(), false);

    assert_eq!(report.failed, Some(DeployStep::Files));
    let step = line(&report, DeployStep::Files);
    assert_eq!(step.word, Word::Failed);
    assert!(
        step.sentence.contains("will not be regenerated"),
        "the failure does not say the pair is kept: {}",
        step.sentence
    );
    assert!(
        step.sentence.contains("push subscription"),
        "the failure does not say what regenerating would cost: {}",
        step.sentence
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("the pair path is readable"),
        garbage,
        "a file the deploy refused to read was rewritten"
    );
}

// ---- step 5: the unit, step 6: the origins, step 7: the health wait ----------------------

#[test]
fn the_unit_is_the_bytes_this_binary_renders_and_the_machine_reads_enabled() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-unit");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);

    // A `--root` install: the file is real, the machine half is not attempted, and the line says so.
    let mut rooted = config(Some(format!("unix://{}", socket.display())));
    rooted.control_listen = health_responder();
    let root = dir.file("root");
    let target = Target::resolve(Some(root.clone()));
    let deployed = target.config(&rooted);
    let tools = real_tools(&dir);
    let exe = std::env::current_exe().expect("this test's own path is readable");
    // The spec's block, byte for byte: a rendering that matches itself would prove nothing about the file an
    // operator's systemd reads.
    let expected = format!(
        "[Unit]\nDescription=Holosmith server\nAfter=network-online.target docker.service\n\
         Wants=network-online.target\nRequires=docker.service\n\n[Service]\nType=simple\n\
         ExecStartPre={exe} doctor --quick\nExecStart={exe} serve\nRestart=on-failure\n\
         RestartSec=5s\nKillMode=mixed\nTimeoutStopSec=30s\n\n[Install]\nWantedBy=multi-user.target\n",
        exe = exe.display()
    );
    assert_eq!(
        deploy::unit_text(&exe, false),
        expected,
        "the rendering is not the spec's unit"
    );

    let first = deploy::run(&deployed, &target, &tools, &Fake::new(), false);
    let step = line(&first, DeployStep::Service);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    let unit = target.unit_path();
    assert_eq!(
        std::fs::read_to_string(&unit).expect("the unit is readable"),
        expected,
        "the unit is not the bytes this binary renders"
    );
    assert_eq!(mode(&unit), 0o644, "the unit is not 0644");
    for line in [
        "Restart=on-failure",
        "WantedBy=multi-user.target",
        " doctor --quick",
        " serve",
    ] {
        assert!(
            expected.contains(line),
            "the unit has no `{line}`:\n{expected}"
        );
    }
    assert!(
        expected.contains(&format!("ExecStartPre={} doctor --quick", exe.display())),
        "`ExecStartPre` is not the argv shape the spec asks for:\n{expected}"
    );
    assert!(
        !expected.contains("User=") && !expected.contains("Group="),
        "a --root unit names a service user it does not have:\n{expected}"
    );
    let second = deploy::run(&deployed, &target, &tools, &Fake::new(), false);
    let step = line(&second, DeployStep::Service);
    assert_eq!(step.word, Word::Satisfied, "{}", step.sentence);
    assert!(
        step.sentence.contains("--root: the unit is in the tree"),
        "the pass does not say the machine half was not read: {}",
        step.sentence
    );

    // A system-path install: the file is written and the two verbs run, `daemon-reload` first, because `enable
    // --now` on a unit systemd has not re-read would start the previous rendering.
    let mut system = config(Some(format!("unix://{}", socket.display())));
    system.data_root = dir.file("system-data");
    let target = Target::resolve(None);
    let deployed = target.config(&system);
    deployed_data_root(&deployed.data_root);
    let calls = dir.file("calls.log");
    let sudo_calls = dir.file("sudo.log");
    let systemctl_log = dir.file("systemctl.log");
    // Built by hand rather than from [`recording_tools`], whose own `systemctl` shim would be written over this
    // one: the answer is the whole subject of the arm.
    let recorded_tools = |enabled: bool| deploy::Tools {
        doctor: doctor_tools(&dir),
        sudo: sudo_recorder(&dir, &sudo_calls),
        install: recorder(&dir, "install", &calls),
        chmod: recorder(&dir, "chmod", &calls),
        systemctl: systemctl_shim(&dir, &systemctl_log, enabled),
        id: id_shim(&dir, None, ""),
        health_wait: Duration::from_millis(200),
        ..deploy::Tools::default()
    };

    let report = deploy::run(
        &deployed,
        &target,
        &recorded_tools(false),
        &Fake::new(),
        false,
    );
    let step = line(&report, DeployStep::Service);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    assert!(
        step.sentence.contains("daemon-reload") && step.sentence.contains("enable --now"),
        "the plan does not name the two verbs: {}",
        step.sentence
    );
    let writes = recorded(&sudo_calls);
    let reload = writes
        .iter()
        .position(|call| call.ends_with("systemctl daemon-reload"))
        .unwrap_or_else(|| panic!("`daemon-reload` was never run: {writes:?}"));
    let enable = writes
        .iter()
        .position(|call| call.ends_with("systemctl enable --now holosmith.service"))
        .unwrap_or_else(|| panic!("`enable --now` was never run: {writes:?}"));
    assert!(
        reload < enable,
        "`enable --now` ran before `daemon-reload`: {writes:?}"
    );
    assert!(
        recorded(&systemctl_log).contains(&"systemctl is-enabled holosmith.service".to_string()),
        "the read never asked systemd whether the unit is enabled: {:?}",
        recorded(&systemctl_log)
    );

    // `is-enabled` is read on every run and is the half of `satisfied` the machine answers: with the shim answering
    // 0 the read says so. The other half — the file's bytes — cannot be satisfied here, because the `install` a
    // system-path run uses is a recorder and the unit it would write is `/etc/systemd/system/holosmith.service` on
    // this machine; the `--root` arm above is what proves the file half.
    std::fs::remove_file(&sudo_calls).expect("the recorder log is removable");
    let report = deploy::run(
        &deployed,
        &target,
        &recorded_tools(true),
        &Fake::new(),
        false,
    );
    assert_eq!(
        recorded(&systemctl_log)
            .iter()
            .filter(|call| call.ends_with("is-enabled holosmith.service"))
            .count(),
        2,
        "the second run did not read whether the unit is enabled: {:?}",
        recorded(&systemctl_log)
    );
    let step = line(&report, DeployStep::Service);
    assert_eq!(
        step.word,
        Word::Changed,
        "the recorder wrote no unit file, so the step had bytes to install: {}",
        step.sentence
    );
}

#[test]
fn both_origins_are_asked_for_and_the_second_run_reads_status() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-origins");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();
    let log = dir.file("tailscale.log");

    let first = deploy::run(&deployed, &target, &tools, &machine, false);
    let step = line(&first, DeployStep::Origins);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    let served = recorded(&log);
    for expected in [
        format!(
            "serve --bg --yes --https=443 http://127.0.0.1:{}",
            deployed.control_listen.port()
        ),
        format!(
            "serve --bg --yes --https=8444 http://127.0.0.1:{}",
            deployed.preview_listen.port()
        ),
    ] {
        assert!(
            served.contains(&expected),
            "the origin was not asked for ({expected}): {served:?}\n{}",
            first.render()
        );
        assert!(
            step.sentence.contains(&expected),
            "step 6's line does not name what it applied: {}",
            step.sentence
        );
    }

    let second = deploy::run(&deployed, &target, &tools, &machine, false);
    let step = line(&second, DeployStep::Origins);
    assert_eq!(
        step.word,
        Word::Satisfied,
        "the second run did not read the status it applied: {}",
        step.sentence
    );
    for fact in [
        deploy::control_target(&deployed),
        deploy::preview_target(&deployed),
        "443".to_string(),
        "8444".to_string(),
    ] {
        assert!(
            step.sentence.contains(&fact),
            "the pass does not name {fact}: {}",
            step.sentence
        );
    }
    assert_eq!(
        recorded(&log).len(),
        served.len(),
        "the second run asked for an origin that was already served: {:?}",
        recorded(&log)
    );
}

/// A self-test failure's sentence, used verbatim: step 7's failure must be `doctor`'s own words.
const BROKEN_NETWORK: &str = "a probe container on the temporary internal network reached the internet: the egress allow-list is not what \
     this machine runs";

#[test]
fn step_seven_waits_for_health_and_reports_doctors_sentence() {
    let _serial = one_at_a_time();

    // The machine serving and fit: the endpoint answers and `--full` has no failing finding.
    let dir = TempDir::new("deploy-health");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut serving = config(Some(format!("unix://{}", socket.display())));
    serving.control_listen = health_responder();
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&serving);
    let report = deploy::run(&deployed, &target, &real_tools(&dir), &Fake::new(), false);
    assert!(
        !report.failed(),
        "a serving machine failed the deploy:\n{}",
        report.render()
    );
    let step = line(&report, DeployStep::Health);
    assert_eq!(step.word, Word::Satisfied, "{}", step.sentence);
    assert!(
        step.sentence.contains("/api/health"),
        "the pass does not say what answered: {}",
        step.sentence
    );
    assert!(
        step.sentence.contains("none failing"),
        "the pass does not report `doctor --full`: {}",
        step.sentence
    );

    // The machine serving but not fit: the wait answers, the second reading fails, and the step is `failed` with
    // that finding's sentence verbatim — step 1's rule, one step later.
    let dir = TempDir::new("deploy-health-finding");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut broken = config(Some(format!("unix://{}", socket.display())));
    broken.control_listen = health_responder();
    let target = Target::resolve(Some(dir.file("root")));
    let deployed = target.config(&broken);
    let machine = Fake::new().with_selftest(selftest::Outcome::Failed(BROKEN_NETWORK.to_string()));
    let report = deploy::run(&deployed, &target, &real_tools(&dir), &machine, false);

    assert_eq!(report.failed, Some(DeployStep::Health));
    let step = line(&report, DeployStep::Health);
    assert_eq!(step.word, Word::Failed);
    assert_eq!(
        step.sentence, BROKEN_NETWORK,
        "the failure is not the finding's own sentence"
    );
    assert_eq!(
        report.lines.len(),
        7,
        "the run did not stop at the step that failed:\n{}",
        report.render()
    );
}

#[test]
fn wait_for_health_answers_in_milliseconds_for_both_arms() {
    let _serial = one_at_a_time();
    // One arm: a listener that answers, which is the whole of what the wait is for.
    let listen = health_responder();
    let started = std::time::Instant::now();
    assert!(
        deploy::wait_for_health(listen, Duration::from_millis(50)),
        "a listener answering /api/health did not satisfy the wait"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the wait took {:?} over a listener that was already answering",
        started.elapsed()
    );

    // The other arm: a port the kernel has just proved free by giving it to a listener this drops.
    let quiet = TcpListener::bind("127.0.0.1:0")
        .expect("a port is bindable")
        .local_addr()
        .expect("the bound address is known");
    let started = std::time::Instant::now();
    assert!(
        !deploy::wait_for_health(quiet, Duration::from_millis(50)),
        "a port nobody answers on satisfied the wait"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(50),
        "the wait gave up before its bound: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "the wait overran its bound: {elapsed:?}"
    );
}
