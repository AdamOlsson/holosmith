//! `server deploy`: the seven steps, the words, the install tree and the two proxies (spec item 10).
//!
//! These tests drive [`deploy::run`] over a machine they build, and that is the same discipline `tests/doctor.rs`
//! established for the same reason: a deploy is a set of claims about *a host* — its daemon, its git, its paths —
//! so the only way to test one without becoming a test of the machine running it is to hand every claim in.
//! `git`, `df`, `tailscale` (both its `status` and its `serve status`), `sudo`, `install`, `chmod`, `systemctl` and
//! `id` are scripts this test wrote (the `Tools` seam), systemd is a directory it leaves out, the daemon is a
//! [`Fake`], and `--root` is what keeps the install tree inside the temp tree. `id` answers the one account question
//! the deploy asks — who is running it, and is that account in the daemon's group — so no arm here depends on the
//! account the gate itself runs as.
//!
//! Three things are driven **for real**, because a fake would prove nothing about them: the mode and byte reads run
//! `/usr/bin/install` and `/usr/bin/chmod` over a real tree (a shim cannot show that `install -m 0600` produces a
//! 0600 file), the VAPID pair comes out of the host's own `openssl` (the DER walk is a claim about that command), and
//! step 1's port check and step 7's health wait both bind real sockets — a fake port would not exercise `doctor`'s
//! own read, and a fake answer would not exercise the wait.
//!
//! ADR-0056 is the design these assert: a step is a read and an apply, `skipped` is never `satisfied`, a failure
//! stops the run, and the proxies' recipe is the boot's own. Since every step applies, so a run meant to reach
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
use holosmith::deploy::{self, DeployStep, Kind, Line, Report, Root, Word};
use holosmith::docker;
use holosmith::doctor;
use holosmith::images::{self, Expectation, Kind as ImageKind};
use holosmith::key;
use holosmith::preflight;
use holosmith::progress::Progress;
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
            // Machine trust's whois: no deploy step runs it, so the name is enough.
            tailscale: PathBuf::from("tailscale"),
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

/// The deploy's tools with the machine's own `install`, `chmod` and `openssl`, a short health wait, and the account
/// question answered the way a machine that can run a session answers it.
///
/// The programs are named rather than pathed, exactly as [`deploy::Tools::default`] names them, so this test asks
/// the same question production asks and follows the host's `PATH` — a repository that hard-coded `/usr/bin/install`
/// would fail on a machine whose coreutils is elsewhere, with a message about a missing file rather than about the
/// check. The key pair is generated by the host's real `openssl`, because a shim would prove nothing about the DER
/// walk; the wait is short because a test may not spend the minute a machine may. `id` is the one answer that is the
/// **machine's** rather than this host's: whether the account running a deploy is in the daemon's group is a fact
/// about a deployed machine, and a test that read this host's answer would pass or fail by whose account ran the gate.
fn real_tools(dir: &TempDir) -> deploy::Tools {
    deploy::Tools {
        doctor: doctor_tools(dir),
        id: id_shim(dir, "id", Some("operator"), "operator docker"),
        health_wait: Duration::from_millis(200),
        ..deploy::Tools::default()
    }
}

/// An `id` answering the two questions step 2 asks about the account running the deploy — `-un` with `name`, and
/// `-nG` with `groups` — or exiting non-zero for both, which is [`None`]: an `id` that cannot say who is running it.
///
/// `file` is the shim's own name inside the fixture's `bin`, so a test that overrides the account answer writes its
/// own script: two callers writing `bin/id` would leave whichever ran last answering both.
fn id_shim(dir: &TempDir, file: &str, name: Option<&str>, groups: &str) -> PathBuf {
    let body = match name {
        Some(name) => format!(
            "case \"$1\" in\n  -un) echo {name} ;;\n  -nG) echo '{groups}' ;;\n  *) exit 1 ;;\nesac"
        ),
        None => "exit 1".to_string(),
    };
    shim(dir, file, &body)
}

/// The body a stand-in for `/api/health` answers: `Health`'s six keys, because the reader parses them —
/// a `200` with anything else is "a port somebody else holds", which is a different fact from "this machine serves".
pub const HEALTH_PAYLOAD: &str = r#"{"name":"holosmith","version":"verify","role":"server","uptime_s":0,"preview_origin":"http://localhost:8081","application_server_key":null}"#;

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

/// A root under `dir`, made the way the operator makes it: 0700, owned by the user running these tests.
///
/// The deploy writes the tree as its owner and does not create the root itself (ADR-0086) — creating it needs the
/// parent, which on a machine is root's — so every test that expects a run to succeed starts here.
fn rooted(dir: &TempDir, label: &str) -> Root {
    let root = Root::resolve(Some(dir.file(label)), Some(dir.file("holosmith.toml")));
    std::fs::create_dir_all(root.path()).expect("the root is creatable");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the root's mode is settable");
    root
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

/// A daemon reached over TCP, answering `GET /_ping` the way [`fake_daemon`] does, and the endpoint that reaches it.
///
/// The host is the kernel's own port for the same reason [`health_responder`] binds one: step 1's docker check is a
/// real read, and a test that wants a `tcp:` endpoint to pass it has to be listening on the other end.
fn tcp_daemon() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port is bindable");
    let address = listener.local_addr().expect("the bound address is known");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nApi-Version: 1.51\r\n\r\nOK");
        }
    });
    format!("tcp://{address}")
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
fn expected_labels(kind: ImageKind, config: &Config) -> BTreeMap<String, String> {
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
    /// would fail on a finding about the containers it had just brought up.
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

    fn selftest(&self, _names: &selftest::Names, _image: &str) -> selftest::Outcome {
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

// ---- the words and the report -------------------------------------------------------------

#[test]
fn the_six_steps_are_the_specs_order() {
    let words: Vec<&str> = DeployStep::ALL.iter().map(|step| step.word()).collect();
    assert_eq!(
        words,
        [
            "preflight",
            "files",
            "images",
            "proxies",
            "service",
            "health"
        ],
        "the step words are not the order this command runs them in"
    );
    let numbers: Vec<u8> = DeployStep::ALL.iter().map(|step| step.number()).collect();
    assert_eq!(numbers, [1, 2, 3, 4, 5, 6]);
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
        fix: None,
    };
    assert_eq!(
        line.to_string(),
        "2. files: satisfied: all 6 paths are present"
    );
}

#[test]
fn the_report_prints_the_tally_and_no_owner_list() {
    let report = Report {
        lines: vec![
            Line {
                step: DeployStep::Preflight,
                word: Word::Satisfied,
                sentence: "10 findings: 8 pass, 0 fail, 2 unknown (systemd, disk)".to_string(),
                fix: None,
            },
            Line {
                step: DeployStep::Files,
                word: Word::Changed,
                sentence: "2 actions; the account operator is in the docker group".to_string(),
                fix: None,
            },
            Line {
                step: DeployStep::Images,
                word: Word::Changed,
                sentence: "built 2 images: holosmith-proxy:0.1.0, agent-image:0.1.0".to_string(),
                fix: None,
            },
            Line {
                step: DeployStep::Service,
                word: Word::Changed,
                sentence: "1 action: install -m 0644 <staged> /var/lib/holosmith/holosmith.service"
                    .to_string(),
                fix: None,
            },
            Line {
                step: DeployStep::Health,
                word: Word::Failed,
                sentence: "the health endpoint never answered".to_string(),
                fix: None,
            },
        ],
        failed: Some(DeployStep::Health),
        dry_run: false,
        root: "/var/lib/holosmith".to_string(),
    };
    let text = report.render_verbose();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "deploy: run");
    // Which tree the report describes is its own line, before the steps: a run's paths mean nothing without the root
    // they are under.
    assert_eq!(lines[1], "deploy: into /var/lib/holosmith");
    // A block per step, one blank line apart — the header's included — and the tally in a block of its own: the
    // step's first line is the only line whose shape is a contract, and it is the `^N. step: word: ` it always was
    // (ADR-0085).
    assert_eq!(
        lines[2..],
        [
            "",
            "1. preflight: satisfied: 10 findings: 8 pass, 0 fail, 2 unknown (systemd, disk)",
            "",
            "2. files: changed: 2 actions; the account operator is in the docker group",
            "",
            "3. images: changed: built 2 images: holosmith-proxy:0.1.0, agent-image:0.1.0",
            "",
            // The numbers are the spec's, so a report that skipped a step's block still names it by its own number.
            "5. service: changed: 1 action: install -m 0644 <staged> \
             /var/lib/holosmith/holosmith.service",
            "",
            "6. health: failed: the health endpoint never answered",
            "",
            "deploy: 5 steps: 1 satisfied, 0 needs, 3 changed, 0 skipped, 1 failed",
        ]
    );
    assert!(
        !text.contains("incomplete"),
        "the report still claims a story owns a step:\n{text}"
    );
    assert!(report.failed());

    // A dry run says so, and a report with nothing failed has no resume claim to make.
    let dry = Report {
        lines: vec![
            Line {
                step: DeployStep::Preflight,
                word: Word::Satisfied,
                sentence: "clean".to_string(),
                fix: None,
            },
            Line {
                step: DeployStep::Files,
                word: Word::Needs,
                sentence: "1 action".to_string(),
                fix: None,
            },
        ],
        failed: None,
        dry_run: true,
        root: "/var/lib/holosmith".to_string(),
    };
    let text = dry.render_verbose();
    assert_eq!(text.lines().next(), Some("deploy: dry run"));
    // Every word a step can report is counted, `needs` included, so the tally's numbers add up to the steps the header
    // names — a six-step run that reported five was missing the one line that says an operator has work left.
    assert_eq!(
        text.lines().last(),
        Some("deploy: 2 steps: 1 satisfied, 1 needs, 0 changed, 0 skipped, 0 failed")
    );
    assert!(!dry.failed());
}

/// Every word a step can report, in one report: one run cannot report all five, and the terse shape is a claim about
/// how each word reads.
fn five_words() -> Report {
    Report {
        lines: vec![
            Line {
                step: DeployStep::Preflight,
                word: Word::Satisfied,
                sentence: "10 findings: 8 pass, 0 fail, 2 unknown (systemd, disk)".to_string(),
                fix: None,
            },
            Line {
                step: DeployStep::Files,
                word: Word::Changed,
                sentence: "2 actions:".to_string(),
                fix: None,
            },
            Line {
                step: DeployStep::Images,
                word: Word::Skipped,
                sentence: "a hub builds no images".to_string(),
                fix: None,
            },
            Line {
                step: DeployStep::Proxies,
                word: Word::Needs,
                sentence: "2 actions:\n    - create the egress network\n    - replace hs_key, the only thing \
                          that holds a real provider key, detached with `--restart unless-stopped`"
                    .to_string(),
                fix: None,
            },
            Line {
                step: DeployStep::Service,
                word: Word::Changed,
                sentence: "1 action; still yours to run: `systemctl --user daemon-reload`; `systemctl \
                          --user enable --now holosmith.service`"
                    .to_string(),
                fix: None,
            },
            Line {
                step: DeployStep::Health,
                word: Word::Failed,
                sentence: "the control origin (127.0.0.1:8080) did not answer `/api/health` within 60 s"
                    .to_string(),
                fix: Some("journalctl --user -u holosmith.service -n 50".to_string()),
            },
        ],
        failed: Some(DeployStep::Health),
        dry_run: false,
        root: "/var/lib/holosmith".to_string(),
    }
}

#[test]
fn the_terse_report_is_one_marked_line_per_step() {
    // The report an operator reads by default (ADR-0085): the mark is the verdict, and a step says more only when it
    // wrote something, needs something or failed. `scripts/verify.sh`'s `case_install_terse` diffs this shape byte for
    // byte on a real run; this is the shape itself, one fixture with all five words in it.
    assert_eq!(
        five_words().render(),
        "1. preflight ✓\n\
         2. files ✓ (2 actions)\n\
         3. images –\n\
         4. proxies ! 2 actions\n\
         \x20   - create the egress network\n\
         \x20   - replace hs_key, the only thing that holds a real provider key, detached with `--restart \
         unless-stopped`\n\
         5. service ✓ (1 action)\n\
         \x20   still yours to run: `systemctl --user daemon-reload`; `systemctl --user enable --now \
         holosmith.service`\n\
         6. health ✗ the control origin (127.0.0.1:8080) did not answer `/api/health` within 60 s\n\
         \x20   fix: journalctl --user -u holosmith.service -n 50\n"
    );
    // The header and the tally do not survive into it: one line per step, and the number is what a reader counts.
    assert!(!five_words().render().contains("deploy:"));
    assert!(!five_words().render().contains("satisfied"));
}

#[test]
fn a_failed_step_without_a_repair_of_its_own_gets_the_runs_resume_command() {
    // `fix:` is one line and it is always pasteable: a step that knows a better answer than a re-run keeps it, and
    // everything else gets the command that just ran (ADR-0085, ADR-0056).
    let mut report = five_words();
    report.fill_fix("holosmith server deploy --root /var/lib/holosmith");
    assert_eq!(
        report.lines.last().and_then(|line| line.fix.as_deref()),
        Some("journalctl --user -u holosmith.service -n 50"),
        "the step's own repair was replaced by the resume command"
    );

    let mut resume_only = Report {
        lines: vec![Line {
            step: DeployStep::Preflight,
            word: Word::Failed,
            sentence: "the daemon at unix:///run/docker.sock does not answer".to_string(),
            fix: None,
        }],
        failed: Some(DeployStep::Preflight),
        dry_run: false,
        root: "/var/lib/holosmith".to_string(),
    };
    resume_only.fill_fix("holosmith server deploy --root /var/lib/holosmith");
    assert_eq!(
        resume_only.render(),
        "1. preflight ✗ the daemon at unix:///run/docker.sock does not answer\n    fix: holosmith server deploy \
         --root /var/lib/holosmith\n"
    );
}

/// What a run told a sink, in the order it told it: the method, the label and the phrase, as one string per line.
#[derive(Default)]
struct Recorded(Vec<String>);

impl Progress for Recorded {
    fn begin(&mut self, label: &str, phrase: &str) {
        self.0.push(format!("begin [{label}] {phrase}"));
    }

    fn note(&mut self, label: &str, phrase: &str) {
        self.0.push(format!("note [{label}] {phrase}"));
    }
}

impl Recorded {
    /// The lines the sink was given, without the method, for the assertions that are about order and labels.
    fn rendered(&self) -> Vec<&str> {
        self.0.iter().map(String::as_str).collect()
    }
}

#[test]
fn a_run_tells_its_sink_one_begin_per_step_and_two_notes_around_each_build() {
    // The silence the sink exists for: step 3's two builds are minutes and step 6's wait is a minute, and neither
    // prints anything until the report. One begin per evaluated step, in the spec's order, and a note before and after
    // each build — no heartbeat, no timer, one line per fact (ADR-0092).
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-progress");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let machine = Fake::new();
    let mut sink = Recorded::default();

    let report = deploy::run_as_with(
        &deployed,
        &root,
        &real_tools(&dir),
        &machine,
        false,
        Kind::Server,
        &mut sink,
    );
    assert!(!report.failed(), "the run failed:\n{}", report.render());

    let begins: Vec<&str> = sink
        .rendered()
        .into_iter()
        .filter(|line| line.starts_with("begin "))
        .collect();
    let labels: Vec<&str> = begins
        .iter()
        .map(|line| {
            line.trim_start_matches("begin [")
                .split_once(']')
                .map(|(label, _)| label)
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(
        labels,
        [
            "1/6 preflight",
            "2/6 files",
            "3/6 images",
            "4/6 proxies",
            "5/6 service",
            "6/6 health"
        ],
        "the begin lines are not one per step in the spec's order: {begins:?}"
    );
    // The two steps that can take minutes say so, which is the whole reason a phrase is part of the line.
    assert!(
        begins[2].contains("a first install downloads pi, so this takes minutes"),
        "step 3's begin line names no duration: {}",
        begins[2]
    );

    // The two notes around each build, naming the image: the tag is how a reader knows which of the two is running
    // when the minutes are the same size.
    let notes: Vec<&str> = sink
        .rendered()
        .into_iter()
        .filter(|line| line.starts_with("note "))
        .collect();
    assert_eq!(
        notes,
        [
            format!("note [3/6 images] building {}", images::proxy_image()),
            format!("note [3/6 images] built {}", images::proxy_image()),
            format!("note [3/6 images] building {}", images::agent_image()),
            format!("note [3/6 images] built {}", images::agent_image()),
        ],
        "the builds did not tell the sink before and after each one"
    );
}

#[test]
fn a_step_the_run_skips_says_nothing_to_the_sink() {
    // A hub's images and proxies are `skipped`, which has already answered that there is nothing to wait for: a begin
    // line over that silence would be a lie about what the silence is (ADR-0092).
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-progress-hub");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.role = Role::Hub;
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let machine = Fake::new();
    let mut sink = Recorded::default();

    let report = deploy::run_as_with(
        &deployed,
        &root,
        &real_tools(&dir),
        &machine,
        false,
        Kind::Hub,
        &mut sink,
    );
    assert!(!report.failed(), "the hub run failed:\n{}", report.render());
    assert_eq!(
        sink.rendered(),
        [
            "begin [1/6 preflight] asking `doctor --quick` about this machine",
            "begin [2/6 files] reading and writing the install tree, the config and the secrets",
            "begin [5/6 service] writing the systemd unit",
            "begin [6/6 health] asking `/api/health` and `doctor --full` about the machine",
        ],
        "a hub's two skips put a line on the sink"
    );
}

#[test]
fn a_dry_run_lists_the_steps_and_creates_nothing() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-dry-run");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let machine = Fake::new();

    let report = deploy::run(&deployed, &root, &real_tools(&dir), &machine, true);

    assert!(!report.failed(), "a dry run failed:\n{}", report.render());
    assert_eq!(
        report
            .lines
            .iter()
            .map(|line| line.step)
            .collect::<Vec<DeployStep>>(),
        DeployStep::ALL,
        "a dry run did not name all six steps:\n{}",
        report.render()
    );
    // Step 1's plan is its read, step 3's is a build of both images, and steps 2, 5 and 6 are plans too: a dry run
    // reads the machine for every step.
    assert_eq!(line(&report, DeployStep::Preflight).word, Word::Satisfied);
    assert_eq!(
        line(&report, DeployStep::Files).word,
        Word::Needs,
        "a dry run did not print a plan for step 2"
    );
    // The plan is a count on the step's own line and then one action per line under it (ADR-0085): a four-action
    // plan is four readable lines, not one semicolon-joined sentence that a phone wraps mid-command.
    let sentence = line(&report, DeployStep::Files).sentence.clone();
    let mut plan = sentence.lines();
    let header = plan
        .next()
        .unwrap_or_else(|| panic!("step 2's plan is empty"));
    let actions: Vec<&str> = plan.collect();
    assert!(
        actions.len() > 1 && actions.iter().all(|action| action.starts_with("    - ")),
        "step 2's plan is not one indented action per line: {sentence}"
    );
    assert!(
        header.starts_with(&format!("{} action", actions.len())),
        "step 2's plan counts its actions anywhere but on its own line: {sentence}"
    );
    assert!(
        actions
            .iter()
            .any(|action| action.contains(&root.config_file().display().to_string())),
        "step 2's plan does not name the config file: {sentence}"
    );
    assert_eq!(
        line(&report, DeployStep::Images).word,
        Word::Needs,
        "a dry run did not print a plan for step 3: {}",
        line(&report, DeployStep::Images).sentence
    );
    // Step 5's plan names the unit's path and the two `systemctl --user` commands that enable it, step 6's the wait
    // and the second reading.
    let service = line(&report, DeployStep::Service);
    assert_eq!(service.word, Word::Needs, "{}", service.sentence);
    assert!(
        service
            .sentence
            .contains(&root.unit().display().to_string()),
        "step 5's plan does not name the unit file: {}",
        service.sentence
    );
    for command in [
        "install -m 0644",
        "systemctl --user daemon-reload",
        "systemctl --user enable --now holosmith.service",
    ] {
        assert!(
            service.sentence.contains(command),
            "step 5's plan does not name `{command}`: {}",
            service.sentence
        );
    }
    assert!(
        !service.sentence.contains("sudo"),
        "step 5's plan escalates: {}",
        service.sentence
    );
    let health = line(&report, DeployStep::Health);
    assert_eq!(health.word, Word::Needs, "{}", health.sentence);
    assert!(
        health.sentence.contains("/api/health") && health.sentence.contains("doctor --full"),
        "step 6's plan is not the wait and the second reading: {}",
        health.sentence
    );
    for step in DeployStep::ALL {
        assert_ne!(
            line(&report, step).word,
            Word::Skipped,
            "{step} is still skipped, and this release has no producer for that word"
        );
    }
    // Nothing was written: the root is there (the operator made it) and nothing under it is.
    assert!(!root.config_file().exists(), "a dry run wrote the config");
    assert!(!root.data_root().exists(), "a dry run made the data root");
    assert!(!root.unit().exists(), "a dry run wrote the unit");
    assert!(
        machine.calls().is_empty() && machine.builds().is_empty(),
        "a dry run started something: {:?} {:?}",
        machine.calls(),
        machine.builds()
    );
}

#[test]
fn the_config_is_outside_the_root_and_its_directory_is_made_when_absent() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-config-outside");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    // A fresh account: `~/.config` itself does not exist, so the config file's directory is this run's to make
    // and it is the one directory step 2 writes that is not under the root.
    let config_file = dir.file("config/holosmith.toml");
    let root = Root::resolve(Some(dir.file("root")), Some(config_file.clone()));
    std::fs::create_dir_all(root.path()).expect("the root is creatable");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the root's mode is settable");
    let deployed = root.config(&config);

    let report = deploy::run(&deployed, &root, &real_tools(&dir), &Fake::new(), false);
    assert!(!report.failed(), "the install failed:\n{}", report.render());
    assert!(
        config_file.is_file(),
        "the config was not written where --config named it"
    );
    assert_eq!(mode(&config_file), 0o644);
    let directory = root
        .config_dir()
        .expect("an absolute config path has a directory");
    assert_eq!(
        mode(&directory),
        0o755,
        "the config's directory was not made 0755"
    );
    assert!(
        !root.path().join(holosmith::config::CONFIG_FILE).exists()
            && !root.path().join("config.toml").exists(),
        "the config landed inside the root"
    );
    // The unit names the config the run wrote, which is what makes the daemon read the operator's own file
    // rather than resolve a home of its own.
    let unit = std::fs::read_to_string(root.unit()).expect("the unit is readable");
    assert!(
        unit.contains(&format!("--config {}", config_file.display())),
        "the unit does not name the config: {unit}"
    );

    // A directory that is already there is the operator's: 0700 is an ordinary mode for `~/.config`, so a second
    // run must neither re-mode it nor report it as work.
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
        .expect("the directory's mode is settable");
    let report = deploy::run(&deployed, &root, &real_tools(&dir), &Fake::new(), true);
    assert_eq!(
        line(&report, DeployStep::Files).word,
        Word::Satisfied,
        "a run over a complete tree is not satisfied: {}",
        line(&report, DeployStep::Files).sentence
    );
    assert_eq!(
        mode(&directory),
        0o700,
        "the run re-moded a directory that was already there"
    );
}

#[test]
fn an_install_creates_the_install_tree_with_its_modes() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-install");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    // Step 6 waits for this answer, so the control listener is a real one that gives it.
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);

    let report = deploy::run(&deployed, &root, &real_tools(&dir), &Fake::new(), false);
    assert!(!report.failed(), "the install failed:\n{}", report.render());
    assert_eq!(line(&report, DeployStep::Files).word, Word::Changed);

    let paths = [
        (root.path().to_path_buf(), 0o700),
        (root.config_file(), 0o644),
        (root.data_root(), 0o755),
        (root.data_root().join("secrets"), 0o700),
        (root.data_root().join("secrets/keys.toml"), 0o600),
        // The pair step 2 generates, and the unit step 5 renders: both are part of the tree an install leaves.
        (root.data_root().join("secrets/vapid.toml"), 0o600),
        (root.unit(), 0o644),
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

    // The file it wrote is the file `serve` reads: it re-parses, and its two roots are the root's.
    let loaded = Config::load(Some(root.config_file()))
        .unwrap_or_else(|error| panic!("the deployed config does not load: {error}"));
    assert_eq!(
        loaded.config.data_root,
        root.data_root(),
        "the written config does not name the root's data root"
    );
    assert_eq!(loaded.config.projects_root, root.projects_root());
    // Everything the deploy must not move is the machine's own, unchanged.
    assert_eq!(loaded.config.control_listen, deployed.control_listen);
    assert_eq!(loaded.config.docker_host, deployed.docker_host);
    // The empty key store is the one `key list` reads, not a file it would refuse.
    let secrets = key::Store::from_data_root(&loaded.config.data_root)
        .load()
        .expect("the deployed key file parses");
    assert!(secrets.providers.is_empty() && secrets.sessions.is_empty());
    // The unit `ExecStart` names the config explicitly, so the daemon never resolves `~` (ADR-0086 D1).
    let unit = std::fs::read_to_string(root.unit()).expect("the unit is readable");
    assert!(
        unit.contains(&format!("--config {}", root.config_file().display())),
        "the unit does not name the config: {unit}"
    );
}

#[test]
fn a_second_run_is_satisfied_and_changes_nothing() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-second-run");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let first = deploy::run(&deployed, &root, &tools, &machine, false);
    assert!(!first.failed(), "the first run failed:\n{}", first.render());
    let before = listing(root.path());
    assert!(!before.is_empty(), "the first run installed nothing");

    let second = deploy::run(&deployed, &root, &tools, &machine, false);
    assert!(
        !second.failed(),
        "the second run failed:\n{}",
        second.render()
    );
    // Every step, not just the three that were the whole install: a second deploy changes nothing at all.
    for step in DeployStep::ALL {
        assert_eq!(
            line(&second, step).word,
            Word::Satisfied,
            "{step} was not already satisfied: {}",
            line(&second, step).sentence
        );
    }
    let files = &line(&second, DeployStep::Files).sentence;
    assert!(
        files.contains("holds the data root") && files.contains("the config is"),
        "step 2 does not say what it read: {files}"
    );
    assert_eq!(
        listing(root.path()),
        before,
        "the second run changed the tree"
    );
}

#[test]
fn an_existing_key_file_is_never_rewritten() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-keys");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    // The tree a previous deploy left, with a real key in the store **and modes a looser umask left behind**: the
    // two things a deploy must handle differently. `install` would truncate the bytes, so a mode it does own is
    // repaired with `chmod`, which touches nothing — and the fixture starts wrong on purpose, because a mode that is
    // already right makes the repair's only assertion a comparison of the fixture with itself (the review,
    // STD-1: with the fixture at 0600, deleting `ItemKind::Kept`'s `Action::Mode` arm left the whole suite green).
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
    let report = deploy::run(&deployed, &root, &real_tools(&dir), &Fake::new(), false);
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
    let config_file = root.config_file();
    std::fs::set_permissions(&config_file, std::fs::Permissions::from_mode(0o600))
        .expect("the config file's mode is settable");
    let report = deploy::run(&deployed, &root, &real_tools(&dir), &Fake::new(), false);
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
fn a_failing_preflight_stops_at_step_one() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-broken-machine");
    let config = config(Some(format!(
        "unix://{}",
        dir.file("absent.sock").display()
    )));
    let root = rooted(&dir, "root");

    let report = deploy::run(
        &root.config(&config),
        &root,
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
        !root.data_root().exists(),
        "a failed preflight installed something at {}",
        root.data_root().display()
    );

    // A dry run reads the same machine, and a dry run that hid it would be useless: it fails too.
    let dry = deploy::run(
        &root.config(&config),
        &root,
        &real_tools(&dir),
        &Fake::new(),
        true,
    );
    assert_eq!(dry.failed, Some(DeployStep::Preflight));
    assert!(dry.render_verbose().contains("deploy: dry run"));
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
    let root = rooted(&dir, "root");

    let report = deploy::run(
        &root.config(&config),
        &root,
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
        &root.config(&broken),
        &root,
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
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let planned = deploy::run(&deployed, &root, &tools, &machine, true);
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

    let report = deploy::run(&deployed, &root, &tools, &machine, false);
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
    let second = deploy::run(&deployed, &root, &tools, &machine, false);
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
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let machine = Fake::new()
        .with_labels(
            &images::proxy_image(),
            expected_labels(ImageKind::Proxy, &deployed),
        )
        .with_labels(
            &images::agent_image(),
            expected_labels(ImageKind::Agent, &deployed),
        );

    let report = deploy::run(&deployed, &root, &real_tools(&dir), &machine, false);

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
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let mut agent = expected_labels(ImageKind::Agent, &deployed);
    agent.insert(
        images::LABEL_PI.to_string(),
        "0.0.0-not-the-pinned-one".to_string(),
    );
    let machine = Fake::new()
        .with_labels(
            &images::proxy_image(),
            expected_labels(ImageKind::Proxy, &deployed),
        )
        .with_labels(&images::agent_image(), agent);

    let planned = deploy::run(&deployed, &root, &real_tools(&dir), &machine, true);
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
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let mut agent = expected_labels(ImageKind::Agent, &deployed);
    agent.insert(images::LABEL_BINARY.to_string(), "0".repeat(64));
    let machine = Fake::new()
        .with_labels(
            &images::proxy_image(),
            expected_labels(ImageKind::Proxy, &deployed),
        )
        .with_labels(&images::agent_image(), agent);

    let planned = deploy::run(&deployed, &root, &real_tools(&dir), &machine, true);
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
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let elsewhere = if daemon_word() == "amd64" {
        "arm64"
    } else {
        "amd64"
    };
    let machine = Fake::new().with_arch(elsewhere);

    let report = deploy::run(&deployed, &root, &real_tools(&dir), &machine, false);

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
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let machine = Fake::new().with_failing_build(COPY_FAILURE);

    let report = deploy::run(&deployed, &root, &real_tools(&dir), &machine, false);

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

#[test]
fn a_build_that_failed_says_so_in_its_note() {
    // The one line a reader watching a silent step gets, and it may not claim the success the report is about to deny:
    // the note's word is the build's own answer (ADR-0092 D3). Still two notes, one before and one after, so a reader
    // is told the build ended — just not wrongly how.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-images-failed-note");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let config = config(Some(format!("unix://{}", socket.display())));
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let machine = Fake::new().with_failing_build(COPY_FAILURE);
    let mut sink = Recorded::default();

    let report = deploy::run_as_with(
        &deployed,
        &root,
        &real_tools(&dir),
        &machine,
        false,
        Kind::Server,
        &mut sink,
    );
    assert_eq!(
        report.failed,
        Some(DeployStep::Images),
        "{}",
        report.render()
    );

    let notes: Vec<&str> = sink
        .rendered()
        .into_iter()
        .filter(|line| line.starts_with("note "))
        .collect();
    assert_eq!(
        notes,
        [
            format!("note [3/6 images] building {}", images::proxy_image()),
            format!("note [3/6 images] did not build {}", images::proxy_image()),
        ],
        "the note over a failed build does not say it did not build"
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
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let first = deploy::run(&deployed, &root, &tools, &machine, false);
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

    let second = deploy::run(&deployed, &root, &tools, &machine, false);
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
fn the_two_halves_union_into_both_and_nothing_is_refused() {
    //  deleted `deploy::refusal`: a role is no longer a reason to refuse a deploy, and the spec's Roles table
    // ("running `server deploy` and `hub deploy` on the same machine makes it `both`") is a function instead. Every
    // arm is here because the union is symmetric in one direction and not the other, and the asymmetry is the bug a
    // one-line implementation gets wrong: `server` on a `hub` is `both`, and `hub` on a `server` is `both`, but
    // `hub` on a `hub` is still `hub`.
    for (found, kind, want) in [
        (Role::Server, Kind::Server, Role::Server),
        (Role::Hub, Kind::Hub, Role::Hub),
        (Role::Both, Kind::Server, Role::Both),
        (Role::Both, Kind::Hub, Role::Both),
        (Role::Hub, Kind::Server, Role::Both),
        (Role::Server, Kind::Hub, Role::Both),
    ] {
        assert_eq!(
            deploy::union(found, kind),
            want,
            "{found} + {} deploy",
            kind.word()
        );
    }
    // The role each command installs is the half it is named after — the fact `union` rests on.
    assert_eq!(Kind::Server.installs(), Role::Server);
    assert_eq!(Kind::Hub.installs(), Role::Hub);
    assert_eq!(Kind::Hub.word(), "hub");
}

#[test]
fn a_hub_deploy_runs_five_steps_and_skips_the_two_a_hub_has_no_work_for() {
    // The acceptance's first bullet, as a run: steps 1, 2, 5, 6 and 7 apply, steps 3 and 4 are `skipped` with a
    // reason, and the tally counts them apart. Two measurements underneath it: the machine is **never asked to build
    // an image or start a container**, and the file the run leaves boots as a hub — a unit that names no system unit,
    // because the user manager does not own those names (ADR-0088 D2).
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-hub");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.role = Role::Hub;
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let report = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(!report.failed(), "the hub run failed:\n{}", report.render());
    let words: Vec<(u8, Word)> = report
        .lines
        .iter()
        .map(|line| (line.step.number(), line.word))
        .collect();
    assert_eq!(
        words,
        [
            (1, Word::Satisfied),
            (2, Word::Changed),
            (3, Word::Skipped),
            (4, Word::Skipped),
            (5, Word::Changed),
            (6, Word::Satisfied),
        ],
        "the hub's plan is not the spec's:\n{}",
        report.render()
    );
    // A skip is never a pass, and its line says what is left out rather than that nothing was needed.
    for (step, word) in [
        (DeployStep::Images, "images"),
        (DeployStep::Proxies, "proxies"),
    ] {
        let line = line(&report, step);
        assert_eq!(line.word, Word::Skipped, "{}", line.sentence);
        assert!(
            line.sentence.contains("a hub"),
            "the {word} line does not say who has no work for it: {}",
            line.sentence
        );
    }
    assert!(
        line(&report, DeployStep::Images)
            .sentence
            .contains("builds no images"),
        "{}",
        line(&report, DeployStep::Images).sentence
    );
    assert!(
        line(&report, DeployStep::Proxies)
            .sentence
            .contains("no proxies"),
        "{}",
        line(&report, DeployStep::Proxies).sentence
    );
    // Nothing was built and nothing was started: `calls` is the argv of a proxy or a network, `builds` of an image.
    assert!(
        machine.builds().is_empty(),
        "a hub deploy built an image: {:?}",
        machine.builds()
    );
    assert!(
        machine.calls().is_empty(),
        "a hub deploy asked the daemon for something: {:?}",
        machine.calls()
    );
    // The file it wrote is the machine that boots next, and a hub's unit requires no daemon.
    let written =
        std::fs::read_to_string(root.config_file()).expect("the config the run wrote is readable");
    assert!(
        written.contains("role = \"hub\""),
        "step 2 did not write the role it installed:\n{written}"
    );
    let unit = std::fs::read_to_string(root.unit()).expect("the unit is readable");
    assert!(
        !unit.contains("docker.service"),
        "a hub's unit requires a daemon this machine does not have:\n{unit}"
    );
    assert!(
        unit.contains("Description=Holosmith hub"),
        "the unit does not describe the machine it boots:\n{unit}"
    );

    // The second run changes nothing: the same five satisfied, the same two skipped.
    let again = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(
        !again.failed(),
        "the second hub run failed:\n{}",
        again.render()
    );
    for step in [
        DeployStep::Preflight,
        DeployStep::Files,
        DeployStep::Service,
        DeployStep::Health,
    ] {
        let line = line(&again, step);
        assert_eq!(line.word, Word::Satisfied, "{}", line.sentence);
    }
    assert_eq!(
        again
            .lines
            .iter()
            .filter(|l| l.word == Word::Skipped)
            .count(),
        2,
        "the second run lost the two skips:\n{}",
        again.render()
    );
}

#[test]
fn a_hub_deploy_asks_no_docker_and_no_git_question() {
    // Item 10 §6's "steps 1 (without the Docker and git checks)" — and the whole reason a hub deploy is the machine
    // that has neither. The same machine, the same config, the same refused `git`, both halves: the server half's
    // step 1 fails on the absent daemon, and the hub half's passes. If the `Kind` were dropped from the checked
    // config this test fails on the second assertion, which is the one the acceptance is about.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-hub-checks");
    let mut config = config(Some(format!(
        "unix://{}",
        dir.file("absent.sock").display()
    )));
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    // A hub asks the account nothing at all, so an `id` that cannot answer must leave its step 2 alone: the daemon's
    // group is a question only the half that drives one has (item 10 §6).
    let mut tools = real_tools(&dir);
    tools.id = id_shim(&dir, "id-blind", None, "");
    tools.doctor.preflight.git = shim(&dir, "git", "exit 1");
    let machine = Fake::new();

    let server = deploy::run_as(&deployed, &root, &tools, &machine, true, Kind::Server);
    let preflight = line(&server, DeployStep::Preflight);
    assert_eq!(preflight.word, Word::Failed, "{}", preflight.sentence);
    assert!(
        preflight.sentence.contains("absent.sock"),
        "the server half did not fail on the daemon: {}",
        preflight.sentence
    );

    let hub = deploy::run_as(&deployed, &root, &tools, &machine, true, Kind::Hub);
    let preflight = line(&hub, DeployStep::Preflight);
    assert_eq!(
        preflight.word,
        Word::Satisfied,
        "a hub deploy asked a question a hub has no answer for: {}",
        preflight.sentence
    );
    for absent in ["docker", "git"] {
        assert!(
            !preflight.sentence.contains(absent),
            "a hub's preflight still reports {absent}: {}",
            preflight.sentence
        );
    }
    let hub_files = line(&hub, DeployStep::Files);
    for absent in ["docker", "usermod", "-un"] {
        assert!(
            !hub_files.sentence.contains(absent),
            "a hub's step 2 asked the account question ({absent}): {}",
            hub_files.sentence
        );
    }
}

#[test]
fn a_hub_deploy_on_a_server_machine_installs_the_union() {
    // Acceptance bullet 4's first half: a machine whose config says `server`, deployed as a hub, ends up `both` —
    // and nothing was refused to get there. The unit is the second half of the same fact: a `both` machine runs
    // sessions, so its unit still requires the daemon even though the run that wrote it installed the hub's half.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-union-hub");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    assert_eq!(config.role, Role::Server);
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let report = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(!report.failed(), "the run failed:\n{}", report.render());
    let written =
        std::fs::read_to_string(root.config_file()).expect("the config the run wrote is readable");
    assert!(
        written.contains("role = \"both\""),
        "a hub deploy on a server machine did not install `both`:\n{written}"
    );
    let unit = std::fs::read_to_string(root.unit()).expect("the unit is readable");
    assert!(
        !unit.contains("docker.service"),
        "a `both` machine's unit names a system unit this manager does not own:\n{unit}"
    );
    assert!(
        unit.contains("Description=Holosmith both"),
        "the unit does not describe the machine it boots:\n{unit}"
    );
    // Still a hub's five steps: the union changed the role, not the plan.
    assert_eq!(
        line(&report, DeployStep::Images).word,
        Word::Skipped,
        "the union turned a hub deploy into a server one"
    );
}

#[test]
fn a_server_deploy_on_a_hub_machine_installs_the_union() {
    // The other direction, and the one the deleted `refusal` used to forbid. Everything the server half needs is
    // here — a daemon that answers, an image build, two proxies — because a `server deploy` is the seven steps
    // whatever the machine's config says; only the file it leaves is different.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-union-server");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.role = Role::Hub;
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let report = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Server);
    assert!(!report.failed(), "the run failed:\n{}", report.render());
    for step in [DeployStep::Images, DeployStep::Proxies] {
        let line = line(&report, step);
        assert_eq!(line.word, Word::Changed, "{}", line.sentence);
    }
    let written =
        std::fs::read_to_string(root.config_file()).expect("the config the run wrote is readable");
    assert!(
        written.contains("role = \"both\""),
        "a server deploy on a hub machine did not install `both`:\n{written}"
    );
    let unit = std::fs::read_to_string(root.unit()).expect("the unit is readable");
    assert!(
        !unit.contains("docker.service"),
        "a `both` machine's unit names a system unit this manager does not own:\n{unit}"
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
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    assert!(
        !deployed.data_root.exists(),
        "this test means nothing unless the data root is absent, and the whole story is that a fresh host's is"
    );
    let mut tools = real_tools(&dir);
    // 1 MiB free against the default `min_free_gb` of 20.
    tools.doctor.preflight.df = df_shim_with(&dir, 1024);

    let report = deploy::run(&deployed, &root, &tools, &Fake::new(), false);
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
fn step_two_prints_no_command_for_a_second_account() {
    // The account half of this step is now one question about **this** account — is it in the daemon's group — so the
    // second account is gone from the words as well as from the code: no `useradd`, no
    // `usermod -aG docker holosmith`, and no `chown` or `install -d -o` among the **account and tree commands step 2
    // prints**, in either the member or the non-member arm, and in a dry run and an apply alike (ADR-0088). Step 6 is
    // not part of that claim: it answers a failing `doctor --full` finding verbatim, and `doctor`'s `perms` sentence
    // spells a `chown` for the tree's owner, which is a real fix for an operator and a finding a correctly-owned tree
    // — the one every run here starts from — does not produce.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-account-remark");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let machine = Fake::new();
    let member = deploy::Tools {
        id: id_shim(&dir, "id-member", Some("operator"), "operator docker"),
        ..real_tools(&dir)
    };
    let unmember = deploy::Tools {
        id: id_shim(&dir, "id-unmember", Some("operator"), "operator sudo"),
        ..real_tools(&dir)
    };

    let mut reports = vec![
        deploy::run(&deployed, &root, &member, &machine, true),
        deploy::run(&deployed, &root, &unmember, &machine, true),
        deploy::run(&deployed, &root, &member, &machine, false),
        deploy::run(&deployed, &root, &unmember, &machine, false),
    ];
    // The second pair reads a tree the first pair's apply wrote, so the account is the only thing left to be wrong.
    reports.push(deploy::run(&deployed, &root, &unmember, &machine, false));
    for report in &reports {
        let text = report.render();
        for absent in [
            "useradd",
            "usermod -aG docker holosmith",
            "chown",
            "install -d -o",
        ] {
            assert!(
                !text.contains(absent),
                "a deploy run still prints `{absent}`:\n{text}"
            );
        }
    }
}

#[test]
fn a_missing_group_membership_is_one_command_a_reason_and_a_re_login() {
    // The one thing this step cannot do for the account running it: `usermod` is root's, and this program never
    // escalates (ADR-0088). So the line carries exactly one command, the reason it is the operator's to run, and the
    // fact that a group reaches a login only at the next one — never a claim that the machine is ready.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-account-group");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let machine = Fake::new();
    let tools = deploy::Tools {
        id: id_shim(&dir, "id-unmember", Some("operator"), "operator sudo"),
        ..real_tools(&dir)
    };

    // The apply writes the tree it owns and keeps the command on the line: `changed` is a claim about the writes.
    let first = deploy::run(&deployed, &root, &tools, &machine, false);
    assert!(!first.failed(), "the first run failed:\n{}", first.render());
    let applied = line(&first, DeployStep::Files);
    assert_eq!(applied.word, Word::Changed, "{}", applied.sentence);
    assert!(
        applied
            .sentence
            .contains("sudo usermod -aG docker operator"),
        "the apply dropped the command: {}",
        applied.sentence
    );

    // The second run has nothing to write and one thing left to do, and it is not the run's: `needs`, not
    // `satisfied` — a pass here would say the machine is ready when its account cannot open the daemon's socket.
    let second = deploy::run(&deployed, &root, &tools, &machine, false);
    assert!(
        !second.failed(),
        "the second run failed:\n{}",
        second.render()
    );
    let step = line(&second, DeployStep::Files);
    assert_eq!(step.word, Word::Needs, "{}", step.sentence);
    assert_eq!(
        step.sentence.matches("sudo usermod").count(),
        1,
        "the line prints more than one command: {}",
        step.sentence
    );
    assert!(
        step.sentence.contains("sudo usermod -aG docker operator"),
        "the command does not name the account running the deploy: {}",
        step.sentence
    );
    assert!(
        step.sentence.contains("owned by the docker group"),
        "the line does not say why the command is the operator's: {}",
        step.sentence
    );
    assert!(
        step.sentence.contains("log out and back in"),
        "the line does not say what makes the group reach a login: {}",
        step.sentence
    );
}

#[test]
fn the_account_running_the_deploy_is_read_and_named() {
    // Two arms of one read, both about the same account: in the daemon's group, and not. The member arm is the
    // demo's "step 2 says `satisfied` and names the account"; the reading itself is `id -un` and `id -nG`, so an
    // `id` that cannot answer is the step's failure rather than a guessed command.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-account-read");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let machine = Fake::new();
    let member = deploy::Tools {
        id: id_shim(&dir, "id-member", Some("operator"), "operator docker"),
        ..real_tools(&dir)
    };

    let first = deploy::run(&deployed, &root, &member, &machine, false);
    assert!(!first.failed(), "the run failed:\n{}", first.render());
    let second = deploy::run(&deployed, &root, &member, &machine, false);
    let step = line(&second, DeployStep::Files);
    assert_eq!(step.word, Word::Satisfied, "{}", step.sentence);
    assert!(
        step.sentence
            .contains("the account operator is in the docker group"),
        "the satisfied arm does not name the account it read: {}",
        step.sentence
    );
    assert!(
        !step.sentence.contains("usermod"),
        "a machine whose account may open the socket was told to join a group: {}",
        step.sentence
    );

    // An `id` that cannot say who is running the deploy fails the step: the command this step prints is a `usermod`
    // for that account, and there is nothing here to guess a name with.
    let blind = deploy::Tools {
        id: id_shim(&dir, "id-blind", None, ""),
        ..real_tools(&dir)
    };
    let report = deploy::run(&deployed, &root, &blind, &machine, true);
    let step = line(&report, DeployStep::Files);
    assert_eq!(step.word, Word::Failed, "{}", step.sentence);
    assert!(
        step.sentence.contains("-un") && step.sentence.contains("no account"),
        "the failure does not say what could not be read: {}",
        step.sentence
    );

    // The same for an `id` that answers exit 0 and nothing at all for the groups: "no groups" is not "in none", and
    // a step that read it as the latter would print a `usermod` for an account that may already be in the group.
    let groupless = deploy::Tools {
        id: id_shim(&dir, "id-groupless", Some("operator"), ""),
        ..real_tools(&dir)
    };
    let report = deploy::run(&deployed, &root, &groupless, &machine, true);
    let step = line(&report, DeployStep::Files);
    assert_eq!(step.word, Word::Failed, "{}", step.sentence);
    assert!(
        step.sentence.contains("-nG") && step.sentence.contains("no groups"),
        "the failure does not say what could not be read: {}",
        step.sentence
    );
}

#[test]
fn a_tcp_endpoint_asks_no_group_question_and_keeps_the_step_satisfied() {
    // The group is how a **socket** is opened, so a daemon on another host is a machine where the account's groups say
    // nothing about whether it works: step 2 asks `id` nothing, prints no command, and does not report `needs` over a
    // membership that would change nothing. The `id` here cannot answer at all, which is what makes the absence of a
    // read the assertion rather than a description — a step that asked would fail.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-tcp-endpoint");
    let mut config = config(Some(tcp_daemon()));
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let machine = Fake::new();
    let tools = deploy::Tools {
        id: id_shim(&dir, "id-blind", None, ""),
        ..real_tools(&dir)
    };

    let first = deploy::run(&deployed, &root, &tools, &machine, false);
    assert!(!first.failed(), "the first run failed:\n{}", first.render());
    let applied = line(&first, DeployStep::Files);
    assert_eq!(applied.word, Word::Changed, "{}", applied.sentence);

    // The tree is written and the account is not a question, so the second run is `satisfied` — the word this
    // configuration earned before step 2 read an account at all.
    let second = deploy::run(&deployed, &root, &tools, &machine, false);
    assert!(
        !second.failed(),
        "the second run failed:\n{}",
        second.render()
    );
    let step = line(&second, DeployStep::Files);
    assert_eq!(step.word, Word::Satisfied, "{}", step.sentence);
    for absent in ["docker", "usermod", "account"] {
        assert!(
            !step.sentence.contains(absent),
            "a tcp endpoint was told something about an account ({absent}): {}",
            step.sentence
        );
    }
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
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let first = deploy::run(&deployed, &root, &tools, &machine, false);
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
    let second = deploy::run(&deployed, &root, &tools, &machine, false);
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
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
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

    let report = deploy::run(&deployed, &root, &real_tools(&dir), &Fake::new(), false);

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
fn a_user_unit_names_no_system_unit_whatever_the_role() {
    // The rule ADR-0088 D2 records: `docker.service` and `network-online.target` are **system** units, and this
    // account's manager does not own those names. `After=` and `Wants=` against one are inert; `Requires=` is a job
    // systemd refuses before `ExecStartPre` runs, which is why the unit that carried it never started and left no
    // journal entry. The rendering therefore carries none of the three, and Docker is `ExecStartPre`'s question —
    // pinned for every role, because the old rendering differed by role and that difference is what made a `both`
    // machine's unit unstartable.
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-unit-roles");
    let root = rooted(&dir, "root");
    let exe = PathBuf::from("/usr/local/bin/holosmith");
    for role in [Role::Hub, Role::Server, Role::Both] {
        let unit = deploy::unit_text(&exe, role, &root);
        for absent in [
            "Requires=",
            "Wants=",
            "After=",
            "docker.service",
            "network-online.target",
        ] {
            assert!(
                !unit.contains(absent),
                "a {role} unit carries `{absent}`, which this manager cannot resolve:\n{unit}"
            );
        }
        assert!(
            unit.contains("StartLimitIntervalSec=0"),
            "a {role} unit would give up on a daemon that is not up yet:\n{unit}"
        );
        assert!(
            unit.contains(&format!("Description=Holosmith {role}")),
            "the unit does not describe the machine it boots:\n{unit}"
        );
    }
}

#[test]
fn the_unit_is_the_bytes_this_binary_renders_and_the_machine_reads_enabled() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-unit");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);

    // A `--root` install: the file is real, the machine half is not attempted, and the line says so.
    let mut machine_config = config(Some(format!("unix://{}", socket.display())));
    machine_config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&machine_config);
    let tools = real_tools(&dir);
    let exe = std::env::current_exe().expect("this test's own path is readable");
    // The spec's block, byte for byte: a rendering that matches itself would prove nothing about the file an
    // operator's systemd reads. The program is the only thing that differs between the two renderings below — the
    // binary this test is running, and the release the deploy plants from it.
    let block = |exe: &Path| {
        format!(
            "[Unit]\nDescription=Holosmith server\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\n\
             UMask=0077\nNoNewPrivileges=yes\nLockPersonality=yes\nRestrictSUIDSGID=yes\n\
             RestrictNamespaces=yes\nRestrictRealtime=yes\nSystemCallFilter=@system-service\n\
             ExecStartPre={exe} doctor --quick\nExecStart={exe} serve --config {config}\n\
             Restart=on-failure\nRestartSec=5s\nKillMode=mixed\nTimeoutStopSec=30s\n\n\
             [Install]\nWantedBy=default.target\n",
            exe = exe.display(),
            config = root.config_file().display(),
        )
    };
    let expected = block(&exe);
    assert_eq!(
        deploy::unit_text(&exe, Role::Server, &root),
        expected,
        "the rendering is not the unit this root describes"
    );

    let first = deploy::run(&deployed, &root, &tools, &Fake::new(), false);
    let step = line(&first, DeployStep::Service);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    let unit = root.unit();
    // The file is the rendering for the release the deploy planted from this binary, not for the path it was started
    // from: a unit that named the latter would be a machine that stops booting the moment the download is gone.
    let planted = block(&root.current().join("holosmith"));
    assert_eq!(
        std::fs::read_to_string(&unit).expect("the unit is readable"),
        planted,
        "the unit does not run the release the deploy planted"
    );
    assert_eq!(mode(&unit), 0o644, "the unit is not 0644");
    for line in [
        "Restart=on-failure",
        "WantedBy=default.target",
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
    // The keep-list, in both directions: the directives the measurement found safe are there, and the five that make
    // systemd enable `PrivateUsers=` — and hand the daemon gid 65534, no supplementary groups and a readable
    // `/etc/shadow` — are not. `ReadWritePaths=` goes with `ProtectSystem=`, since it is that mechanism's exception.
    for directive in [
        "NoNewPrivileges=yes",
        "UMask=0077",
        "LockPersonality=yes",
        "RestrictSUIDSGID=yes",
        "RestrictNamespaces=yes",
        "RestrictRealtime=yes",
        "SystemCallFilter=@system-service",
    ] {
        assert!(
            expected.contains(directive),
            "the unit has no `{directive}`:\n{expected}"
        );
    }
    for absent in [
        "User=",
        "Group=",
        "PrivateTmp=",
        "ProtectSystem=",
        "ProtectHome=",
        "ReadWritePaths=",
    ] {
        assert!(
            !expected.contains(absent),
            "the unit carries `{absent}`, which a user unit cannot have:\n{expected}"
        );
    }
    let second = deploy::run(&deployed, &root, &tools, &Fake::new(), false);
    let step = line(&second, DeployStep::Service);
    assert_eq!(step.word, Word::Satisfied, "{}", step.sentence);
    assert!(
        step.sentence
            .contains("still yours to run: systemctl --user daemon-reload"),
        "the pass does not say what is left to run: {}",
        step.sentence
    );

    // The same bytes and the same commands for a dry run over a complete tree: the enable is still words, and no
    // child of root's is named anywhere in the pair (ADR-0088 — the unit is the operator's own file).
    let again = deploy::run(&deployed, &root, &tools, &Fake::new(), true);
    let plan = line(&again, DeployStep::Service).sentence.clone();
    assert!(
        plan.contains("systemctl --user daemon-reload")
            && plan.contains("systemctl --user enable --now holosmith.service"),
        "the plan does not name the two commands that enable the unit: {plan}"
    );
    assert!(
        !plan.contains("sudo"),
        "the plan names an escalation a user unit does not need: {plan}"
    );
}

#[test]
fn step_seven_waits_for_health_and_reports_doctors_sentence() {
    let _serial = one_at_a_time();

    // The machine serving and fit: the endpoint answers and `--full` has no failing finding.
    let dir = TempDir::new("deploy-health");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut serving = config(Some(format!("unix://{}", socket.display())));
    serving.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&serving);
    let report = deploy::run(&deployed, &root, &real_tools(&dir), &Fake::new(), false);
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
    let root = rooted(&dir, "root");
    let deployed = root.config(&broken);
    const BROKEN_NETWORK: &str = "the probe reached the host";
    let machine = Fake::new().with_selftest(selftest::Outcome::Failed(BROKEN_NETWORK.to_string()));
    let report = deploy::run(&deployed, &root, &real_tools(&dir), &machine, false);

    assert_eq!(report.failed, Some(DeployStep::Health));
    let step = line(&report, DeployStep::Health);
    assert_eq!(step.word, Word::Failed);
    assert_eq!(
        step.sentence, BROKEN_NETWORK,
        "the failure is not the finding's own sentence"
    );
    assert_eq!(
        report.lines.len(),
        6,
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

// ---- a promoted machine's unit runs through `current` ------------------------------------------------

/// Once `current` resolves, step 5's `ExecStart` is the release tree's symlink — and a second deploy leaves the unit
/// exactly as it found it.
///
/// The claim is one fact with two halves, and both are here: the *first* run writes a unit that names
/// `<install>/opt/holosmith/current/holosmith` (so a promotion's switch is what the next boot picks up), and the
/// *second* run's read finds those same bytes (so `server deploy`, run after a promotion, neither undoes it nor
/// calls it changed).
#[test]
fn a_release_tree_makes_the_unit_run_through_current() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-release-tree");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.role = Role::Hub;
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");

    // A release tree with one live release, before the deploy runs: this is what a promotion leaves behind.
    let release = holosmith::release::Release::of(&root);
    std::fs::create_dir_all(release.releases()).expect("the releases directory is creatable");
    let bytes = b"#!/bin/sh\nexit 0\n";
    let artifact = dir.file("v0.1.0-holosmith");
    std::fs::write(&artifact, bytes).expect("the fixture binary is writable");
    let digest = images::digest(&artifact).expect("the fixture binary is readable");
    release
        .write("v0.1.0", bytes, &digest)
        .expect("a release is writable");
    release.switch("v0.1.0").expect("the release is live");

    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();
    let report = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(!report.failed(), "the hub run failed:\n{}", report.render());
    assert_eq!(
        line(&report, DeployStep::Service).word,
        Word::Changed,
        "{}",
        line(&report, DeployStep::Service).sentence
    );
    let unit = std::fs::read_to_string(root.unit()).expect("the unit is readable");
    let live = format!("{}/holosmith", root.current().display());
    assert!(
        unit.contains(&format!("ExecStart={live} serve --config")),
        "the unit does not run through `current`:\n{unit}"
    );
    assert!(
        unit.contains(&format!("ExecStartPre={live} doctor --quick")),
        "the unit's pre-line does not run the release it starts:\n{unit}"
    );

    // The second run reads the same bytes, so nothing changed and nothing was undone.
    let again = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(
        !again.failed(),
        "the second hub run failed:\n{}",
        again.render()
    );
    assert_eq!(
        line(&again, DeployStep::Service).word,
        Word::Satisfied,
        "a deploy on a promoted machine rewrote the unit: {}",
        line(&again, DeployStep::Service).sentence
    );

    // A release tree whose `current` names a directory with no binary in it is *not* a live release (`Release::live`
    // answers `None`), so step 5 repairs it: it plants this binary as a release of its own and points `current` at
    // that — a unit pointing through a `current` that cannot start would be an install that never boots again, and a
    // unit naming whatever path this run was started from would be the same break one download later.
    std::fs::remove_file(release.binary("v0.1.0")).expect("the release binary is removable");
    let broken = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(
        !broken.failed(),
        "the run over a broken release tree failed:\n{}",
        broken.render()
    );
    // And step 1 says why it did not refuse: a deploy is one of the two commands that repairs a broken `current`, so
    // the failure `doctor --quick` reports is a remark here (`LINK_NOT_LIVE`).
    assert_eq!(
        line(&broken, DeployStep::Preflight).word,
        Word::Satisfied,
        "a broken `current` refused the deploy that repairs it: {}",
        line(&broken, DeployStep::Preflight).sentence
    );
    assert!(
        line(&broken, DeployStep::Preflight)
            .sentence
            .contains("step 5 plants"),
        "the preflight line does not say what repairs the tree: {}",
        line(&broken, DeployStep::Preflight).sentence
    );
    assert_eq!(
        line(&broken, DeployStep::Service).word,
        Word::Changed,
        "{}",
        line(&broken, DeployStep::Service).sentence
    );
    // The repair is the seed: `releases/<this binary's own tag>/holosmith` holds the running bytes, and `current` — a
    // relative link — names it, so the unit's `ExecStart` resolves again.
    let tag = holosmith::version::VersionInfo::current().git_tag;
    assert!(
        release.binary(&tag).is_file(),
        "the broken tree was not repaired with a release named after this binary ({tag})"
    );
    assert_eq!(
        release.link(),
        Some(PathBuf::from(format!("releases/{tag}"))),
        "`current` does not point at the release this run planted"
    );
    let unit = std::fs::read_to_string(root.unit()).expect("the unit is readable");
    let live = format!("{}/holosmith", root.current().display());
    assert!(
        unit.contains(&format!("ExecStart={live} serve")),
        "the repaired unit does not run through `current`:\n{unit}"
    );
    assert!(
        unit.contains(&format!("ExecStartPre={live} doctor --quick")),
        "the repaired unit's pre-line does not run the release it starts:\n{unit}"
    );
}

// ---- a tree with no release in it is planted from the binary this run is -----------------------------

/// A fresh install leaves the unit running a binary **inside the install tree**, not the file it was started from.
///
/// The release directory is named after this binary's own git tag and holds its exact bytes and their digest,
/// `current` is a **relative** link to that directory, and the unit names `current/holosmith` — which is what makes
/// the path the download landed in stop mattering. The second run finds the tree live and rewrites nothing, because
/// the same rule that plants it is the one that keeps a later `deploy` from undoing a promotion.
#[test]
fn a_fresh_tree_is_seeded_with_the_binary_this_run_is() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-seed-fresh");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.role = Role::Hub;
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    let report = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(!report.failed(), "the hub run failed:\n{}", report.render());
    assert_eq!(
        line(&report, DeployStep::Service).word,
        Word::Changed,
        "{}",
        line(&report, DeployStep::Service).sentence
    );

    let exe = std::env::current_exe().expect("this test's own path is readable");
    let running = std::fs::read(&exe).expect("this test's own bytes are readable");
    let tag = holosmith::version::VersionInfo::current().git_tag;
    let release = holosmith::release::Release::of(&root);
    let planted = release.binary(&tag);
    assert_eq!(
        std::fs::read(&planted).expect("the planted release is readable"),
        running,
        "the planted release is not the bytes this run is running"
    );
    assert_eq!(
        std::fs::read_to_string(release.dir_of(&tag).join(holosmith::release::DIGEST))
            .expect("the planted digest is readable"),
        format!(
            "{}  holosmith\n",
            images::digest(&exe).expect("the running binary is readable")
        ),
        "the release's digest file is not this binary's digest"
    );
    // Relative, so the tree stays movable — and it is what the unit resolves at every (re)start.
    assert_eq!(
        release.link(),
        Some(PathBuf::from(format!("releases/{tag}"))),
        "`current` is not a relative link to the release this run planted"
    );
    let live = root.current().join("holosmith");
    let unit = std::fs::read_to_string(root.unit()).expect("the unit is readable");
    assert!(
        unit.contains(&format!("ExecStart={} serve", live.display())),
        "the unit does not run the release the deploy planted:\n{unit}"
    );
    assert!(
        unit.contains(&format!("ExecStartPre={} doctor --quick", live.display())),
        "the unit's pre-line does not run the release it starts:\n{unit}"
    );

    // The second run: the tree is live, so step 5 is a read over the same bytes — every artefact the first run wrote
    // is untouched, down to the planted binary's mtime.
    let unit1 = std::fs::read(root.unit()).expect("the unit is readable");
    let planted_at = std::fs::metadata(&planted)
        .and_then(|meta| meta.modified())
        .expect("the planted release's mtime reads");
    let again = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(
        !again.failed(),
        "the second hub run failed:\n{}",
        again.render()
    );
    assert_eq!(
        line(&again, DeployStep::Service).word,
        Word::Satisfied,
        "a deploy over a tree it planted itself rewrote the unit: {}",
        line(&again, DeployStep::Service).sentence
    );
    assert_eq!(
        release.history(),
        vec![tag.clone()],
        "the second run planted a second release"
    );
    assert_eq!(
        std::fs::metadata(&planted)
            .and_then(|meta| meta.modified())
            .expect("the planted release's mtime reads"),
        planted_at,
        "the second run rewrote the release's binary"
    );
    assert_eq!(
        std::fs::read(root.unit()).expect("the unit is readable"),
        unit1,
        "the second run rewrote the unit"
    );

    // The plan counts what it would do. `current` removed with the release intact is the state a `rm current`
    // hand-repair leaves: the tree still holds this binary and the digest that names it, so the only write is the
    // switch — a `Release` action there would claim a rewrite of two files that already say what they should.
    std::fs::remove_file(root.current()).expect("the link is removable");
    let relinked = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(
        !relinked.failed(),
        "the hub run over a tree with no `current` failed:\n{}",
        relinked.render()
    );
    let step = line(&relinked, DeployStep::Service);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    assert!(
        step.sentence.starts_with("1 action; ") && !step.sentence.contains("from this binary"),
        "the plan claims a release write the run would not perform: {}",
        step.sentence
    );
    assert_eq!(
        release.link(),
        Some(PathBuf::from(format!("releases/{tag}"))),
        "the switch did not put `current` back"
    );

    // A `current` the tree cannot explain is **taken over**, and the line says which target it replaced: a link that
    // moved with no word is the one thing a `(1 action)` cannot show, and a promotion refuses this same state rather
    // than guessing what the previous release was.
    std::fs::remove_file(root.current()).expect("the link is removable");
    std::os::unix::fs::symlink("/somewhere/else", root.current())
        .expect("a link to another path is creatable");
    let taken_over = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(
        !taken_over.failed(),
        "the hub run over an unexplained `current` failed:\n{}",
        taken_over.render()
    );
    let step = line(&taken_over, DeployStep::Service);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    assert!(
        step.sentence.contains("/somewhere/else")
            && step.sentence.contains("not a release of this tree"),
        "the line does not say which link it replaced: {}",
        step.sentence
    );
    assert_eq!(
        release.link(),
        Some(PathBuf::from(format!("releases/{tag}"))),
        "the takeover did not point `current` at this run's release"
    );
}

// ---- what stops the seed, and what the line says about it -------------------------------------------

/// The `placement` arm: a `releases/<this binary's tag>/holosmith` that already holds **different** bytes cannot be
/// planted, so step 5 falls back to the binary this run is running — one action, the unit, and the refusal in the
/// sentence.
///
/// This is the arm ADR-0094 D4 promises costs the seed and not the install, and the one a machine trips when a tree
/// has been seeded by a build whose bytes the tag does not describe. The release is left as it was found, because one
/// directory holding two binaries is the ambiguity a rollback cannot resolve (ADR-0082 D4).
#[test]
fn a_release_holding_another_binary_falls_back_to_this_run() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-seed-refused");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.role = Role::Hub;
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    // Another binary under this run's own tag, before the run: the one state the tree refuses to overwrite.
    let tag = holosmith::version::VersionInfo::current().git_tag;
    let release = holosmith::release::Release::of(&root);
    let other = b"#!/bin/sh\nexit 1\n";
    let placeholder = dir.file("other-holosmith");
    std::fs::write(&placeholder, other).expect("the fixture binary is writable");
    let other_digest = images::digest(&placeholder).expect("the fixture binary is readable");
    release
        .write(&tag, other, &other_digest)
        .expect("a release is writable");

    let report = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(!report.failed(), "the hub run failed:\n{}", report.render());
    let step = line(&report, DeployStep::Service);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    assert!(
        step.sentence.starts_with("1 action; "),
        "the step planned more than the unit's own write: {}",
        step.sentence
    );
    assert!(
        step.sentence.contains("no release was planted")
            && step.sentence.contains("already holds a different binary"),
        "the line does not say why the tree was not seeded: {}",
        step.sentence
    );
    // The refused release is untouched, no `current` was made for a release the run did not write, and the unit names
    // the binary this run is running — which is the whole point of falling back rather than failing.
    assert_eq!(
        std::fs::read(release.binary(&tag)).expect("the release is readable"),
        other,
        "the refused release was rewritten"
    );
    assert_eq!(
        release.link(),
        None,
        "the run pointed `current` at a release it refused to write"
    );
    let exe = std::env::current_exe().expect("this test's own path is readable");
    let unit = std::fs::read_to_string(root.unit()).expect("the unit is readable");
    assert!(
        unit.contains(&format!("ExecStart={} serve", exe.display())),
        "the unit does not name the binary this run is running:\n{unit}"
    );
    assert!(
        unit.contains(&format!("ExecStartPre={} doctor --quick", exe.display())),
        "the unit's pre-line does not run the binary this run is running:\n{unit}"
    );
}

/// A live release holding **different** bytes is left alone — and the `satisfied` line an operator reads says so:
/// both digests, and the one command that does replace a live release.
///
/// `server deploy` after a `cargo build` is the case: the unit keeps naming the release that is live, because that is
/// what keeps a deploy from undoing a promotion. The clause has to survive into the **default** rendering, or the
/// operator who ran the quick start's command is told `5. service ✓` and nothing else.
#[test]
fn a_live_release_that_is_not_this_run_is_named_on_the_line() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-live-remark");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.role = Role::Hub;
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    // A live release that is not this binary: what a promotion of another build leaves behind.
    let release = holosmith::release::Release::of(&root);
    std::fs::create_dir_all(release.releases()).expect("the releases directory is creatable");
    let other = b"#!/bin/sh\nexit 0\n";
    let placeholder = dir.file("v0.1.0-holosmith");
    std::fs::write(&placeholder, other).expect("the fixture binary is writable");
    let live_digest = images::digest(&placeholder).expect("the fixture binary is readable");
    release
        .write("v0.1.0", other, &live_digest)
        .expect("a release is writable");
    release.switch("v0.1.0").expect("the release is live");

    let first = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(!first.failed(), "the hub run failed:\n{}", first.render());
    assert_eq!(
        line(&first, DeployStep::Service).word,
        Word::Changed,
        "{}",
        line(&first, DeployStep::Service).sentence
    );

    // The second run finds the unit it wrote and the release it did not touch: `satisfied`, with the clause.
    let again = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(
        !again.failed(),
        "the second hub run failed:\n{}",
        again.render()
    );
    let step = line(&again, DeployStep::Service);
    assert_eq!(step.word, Word::Satisfied, "{}", step.sentence);
    let exe = std::env::current_exe().expect("this test's own path is readable");
    let own = images::digest(&exe).expect("this test's own bytes are readable");
    assert!(
        step.sentence.contains(&own[..12]) && step.sentence.contains(&live_digest[..12]),
        "the satisfied line does not name both binaries: {}",
        step.sentence
    );
    assert!(
        step.sentence.contains("holosmith upgrade"),
        "the satisfied line does not name what does replace a live release: {}",
        step.sentence
    );
    // And the clause is in the report an operator actually reads, not only in `--verbose`.
    assert!(
        again
            .render()
            .contains("    this run's binary is not the live release's"),
        "the default report dropped the clause: {}",
        again.render()
    );
    // The tree is as it was: one release, still live, still those bytes.
    assert_eq!(release.history(), vec!["v0.1.0".to_string()]);
    assert_eq!(
        std::fs::read(release.binary("v0.1.0")).expect("the release is readable"),
        other,
        "the deploy rewrote a live release"
    );
}

/// The **write** arm: a `current` that is a plain directory — the state ADR-0084's table names — stops the switch, and
/// the run falls back once rather than failing step 5 with no unit at all.
///
/// No read can see this one: `Release::link` answers `None` for anything that is not a symlink, so the seed looks
/// fresh and the failure arrives at `rename(2)`. The unit still lands, naming the binary this run is running, which is
/// what the deploy wrote before it seeded anything — and the entry the operator made is not deleted to make room.
#[test]
fn a_current_that_is_a_directory_still_leaves_a_unit() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("deploy-seed-unwritable");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(Some(format!("unix://{}", socket.display())));
    config.role = Role::Hub;
    config.control_listen = health_responder();
    let root = rooted(&dir, "root");
    let deployed = root.config(&config);
    let tools = real_tools(&dir);
    let machine = Fake::new();

    // `current` as a directory holding something: a `rename` onto it fails, which is the one failure only the write
    // can find.
    std::fs::create_dir_all(root.current()).expect("a plain directory is creatable");
    std::fs::write(root.current().join("note"), "the operator's own entry\n")
        .expect("the entry is writable");

    let report = deploy::run_as(&deployed, &root, &tools, &machine, false, Kind::Hub);
    assert!(!report.failed(), "the hub run failed:\n{}", report.render());
    let step = line(&report, DeployStep::Service);
    assert_eq!(step.word, Word::Changed, "{}", step.sentence);
    assert!(
        step.sentence.starts_with("1 action; "),
        "the failed switch stayed in the plan: {}",
        step.sentence
    );
    assert!(
        step.sentence.contains("no release was planted")
            && step.sentence.contains("could not be replaced"),
        "the line does not carry the write's own reason: {}",
        step.sentence
    );
    let exe = std::env::current_exe().expect("this test's own path is readable");
    let unit = std::fs::read_to_string(root.unit()).expect("the unit is readable");
    assert!(
        unit.contains(&format!("ExecStart={} serve", exe.display())),
        "the fallback did not write a unit naming this run:\n{unit}"
    );
    assert!(
        root.current().is_dir(),
        "the run replaced an entry it did not understand"
    );
}
