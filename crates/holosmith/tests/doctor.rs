//! `doctor`: the machine's fitness, check by check (HS-045, spec item 10 §1 and §7, V2).
//!
//! These tests drive `doctor::run` over a machine they build: `git`, `df` and `tailscale` are scripts the test
//! wrote (the `Tools` seam), systemd is a directory this test makes or leaves out, and the daemon questions are a
//! [`Machine`] literal. That is the same discipline `tests/preflight.rs` established — a check about the host
//! cannot be tested against the host — and it is why these tests do not care which git version, Tailscale state
//! or free space the machine running them has.
//!
//! Two of the checks are *measurements* rather than questions, and they are driven for real: the merge trial runs
//! a genuine `git` over a throwaway repository it creates and removes, and the port check binds real sockets. A
//! fake for either would prove nothing about V2 or about `ExecStartPre`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use holosmith::clock;
use holosmith::config::{Config, Role};
use holosmith::db::{self, Database, OpenOptions};
use holosmith::doctor::{self, Depth, Machine, Report, Tools};
use holosmith::images;
use holosmith::key;
use holosmith::preflight::{self, Check, Finding, Outcome};
use holosmith::selftest;
use holosmith::vapid;

mod support;
use support::TempDir;

/// Hold this for the whole body of a test here: one test at a time.
///
/// The same trap `tests/preflight.rs` documents: `exec` of a file any process holds open for writing fails with
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

/// A `git` shim answering `version` — and, because the merge trial asks a git more than its version, anything
/// else with exit 0 and one line, which is the *wrong* answer V2 exists to catch.
fn git_shim(dir: &TempDir, version: &str) -> PathBuf {
    shim(
        dir,
        "git",
        &format!(
            "case \"$1\" in\n  --version) echo 'git version {version}' ;;\n  *) echo 0000000000000000000000000000000000000000 ;;\nesac"
        ),
    )
}

/// A `df -Pk` shim answering `available_kb` free.
fn df_shim(dir: &TempDir, available_kb: u64) -> PathBuf {
    shim(
        dir,
        "df",
        &format!(
            "printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n\
             /dev/mapper/root 1000000000 1 {available_kb} 1%% /\\n'"
        ),
    )
}

/// A `tailscale` shim answering `body` — one arm of `tailscale status --json`.
fn tailscale_shim(dir: &TempDir, body: &str) -> PathBuf {
    shim(dir, "tailscale", body)
}

/// The tools a test built: git 2.43 (or `version`), 480 GB free, a logged-in Tailscale and no systemd.
fn tools(dir: &TempDir, version: &str) -> Tools {
    Tools {
        preflight: preflight::Tools {
            git: git_shim(dir, version),
            df: df_shim(dir, 480_000_000),
            du: PathBuf::from("du"),
        },
        tailscale: tailscale_shim(dir, "echo '{\"BackendState\":\"Running\"}'"),
        systemd: dir.file("no-systemd-here"),
        ..Tools::default()
    }
}

/// The same tools, but with the machine's own `git` — for the arms whose subject *is* a real trial.
fn tools_with_real_git(dir: &TempDir) -> Tools {
    Tools {
        preflight: preflight::Tools {
            git: PathBuf::from("git"),
            df: df_shim(dir, 480_000_000),
            du: PathBuf::from("du"),
        },
        ..tools(dir, "2.43.0")
    }
}

/// A config whose data root is inside `dir`, whose daemon is `docker_host`, and whose listeners are the kernel's.
fn config(dir: &TempDir, docker_host: Option<String>) -> Config {
    Config {
        data_root: dir.path().join("data"),
        docker_host,
        control_listen: "127.0.0.1:0".parse().expect("a listener address"),
        preview_listen: "127.0.0.1:0".parse().expect("a listener address"),
        ..Config::default()
    }
}

/// The data root a deploy leaves: present, with the mode `server deploy` writes.
///
/// Every check that takes an existing data root needs it to be the mode `perms` expects — a directory made by
/// `create_dir_all` is 0775 under a permissive umask, which is a wrong mode and would make the modes check a failure
/// in tests whose subject is something else.
fn deployed_data_root(config: &Config) {
    std::fs::create_dir_all(&config.data_root).expect("the data root is creatable");
    std::fs::set_permissions(&config.data_root, std::fs::Permissions::from_mode(0o755))
        .expect("the data root's mode is settable");
}

/// A daemon that answers every `GET /_ping` the way a real one does, and how many it answered.
///
/// Unlike `tests/preflight.rs`'s one-shot socket, this one keeps listening: a `doctor` run asks once, and these
/// tests drive more than one run over the same machine (a quick run and a full one, three Tailscale arms), so a
/// socket that answered once would make every run after the first fail on the daemon rather than on its subject.
fn fake_daemon(path: &Path) -> Arc<AtomicUsize> {
    let listener = UnixListener::bind(path).expect("the test socket is bindable");
    let answered = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&answered);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nApi-Version: 1.51\r\n\r\nOK");
            seen.fetch_add(1, Ordering::SeqCst);
        }
    });
    answered
}

/// A machine whose four container questions the test decides.
struct FakeMachine {
    running: Vec<&'static str>,
    image_pi: Result<String, String>,
    /// The image's `org.holosmith.*` labels: `Ok(None)` is an image built by another tool (`pi-agent:local`).
    image_labels: Result<Option<BTreeMap<String, String>>, String>,
    selftest: selftest::Outcome,
}

impl FakeMachine {
    /// The machine `--full` passes on: both proxies up, the pinned pi, a measured isolation, and an image with no
    /// Holosmith labels — the supported by-hand image, whose label half the arms below compare.
    ///
    /// An image with no labels answers `null` from `{{json .Config.Labels}}`, which [`images::Images`]'s Docker impl
    /// turns into an **empty map**, not a `None` (that is "the image is not there"). The fake answers what the real
    /// reader answers, or the tests would prove an arm production can never reach.
    fn healthy() -> Self {
        Self {
            running: vec!["hs_egress", "hs_key"],
            image_pi: Ok(holosmith::pins::pi_version().to_string()),
            image_labels: Ok(Some(BTreeMap::new())),
            selftest: selftest::Outcome::Passed,
        }
    }
}

impl Machine for FakeMachine {
    fn running(&self, name: &str) -> bool {
        self.running.contains(&name)
    }

    fn image_pi(&self, _image: &str) -> Result<String, String> {
        self.image_pi.clone()
    }

    fn image_labels(&self, _image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        self.image_labels.clone()
    }

    fn selftest(&self, _image: &str) -> selftest::Outcome {
        self.selftest.clone()
    }
}

/// The finding whose check is `check`.
fn finding(report: &Report, check: Check) -> &Finding {
    report
        .findings
        .iter()
        .find(|finding| finding.check == check)
        .unwrap_or_else(|| panic!("no {check} finding in:\n{}", report.render()))
}

/// The `nth` finding whose check is `check` — the two ports, the two proxies.
fn nth(report: &Report, check: Check, index: usize) -> &Finding {
    let matches: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|finding| finding.check == check)
        .collect();
    matches
        .get(index)
        .copied()
        .unwrap_or_else(|| panic!("no {index}th {check} finding in:\n{}", report.render()))
}

/// What a finding says, for a failure message.
fn says(finding: &Finding) -> &str {
    finding.sentence.as_str()
}

#[test]
fn a_clean_machine_passes_quick_with_its_whole_list_in_order() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-clean");
    let socket = dir.file("docker.sock");
    let answered = fake_daemon(&socket);
    let mut config = config(&dir, Some(format!("unix://{}", socket.display())));
    // A data root that exists is what `df`, the schema check and the modes check are about; the checks create
    // nothing. Its mode is the one `server deploy` writes, because `perms` reads it.
    deployed_data_root(&config);
    config.role = Role::Server;
    let report = doctor::run(
        &config,
        Depth::Quick,
        &tools_with_real_git(&dir),
        &FakeMachine::healthy(),
    );

    assert!(
        answered.load(Ordering::SeqCst) > 0,
        "the daemon was never asked"
    );
    assert!(
        !report.failed(),
        "a clean machine failed:\n{}",
        report.render()
    );
    let words: Vec<&str> = report
        .findings
        .iter()
        .map(|finding| finding.check.as_str())
        .collect();
    assert_eq!(
        words,
        [
            "systemd",
            "docker",
            "git",
            "merge",
            "disk",
            "tailscale",
            "ports",
            "ports",
            "database",
            "perms"
        ],
        "the quick list is not the spec's list, in the spec's order:\n{}",
        report.render()
    );
    for check in [
        Check::Docker,
        Check::Git,
        Check::Merge,
        Check::Disk,
        Check::Tailscale,
        Check::Database,
        Check::Perms,
    ] {
        assert_eq!(
            finding(&report, check).outcome,
            Outcome::Pass,
            "{} did not pass: {}",
            check,
            says(finding(&report, check))
        );
    }
    // The one honest line on a machine that has no systemd: not a failure (`serve` by hand is supported), and
    // not a pass either (a deploy needs it) — ADR-0054.
    assert_eq!(finding(&report, Check::Systemd).outcome, Outcome::Unknown);
    // Port 0 is the kernel's, so both port checks are the "nothing to hold" arm rather than a pass to be proud
    // of; the held-port arm is `a_held_port_is_a_failure_before_a_start_and_the_machine_serving_after_one`.
    assert_eq!(nth(&report, Check::Ports, 0).outcome, Outcome::Pass);
    assert_eq!(
        report.render().lines().last(),
        Some("doctor: 10 findings: 9 pass, 0 fail, 1 unknown"),
        "the tally is not one line of the report"
    );
}

#[test]
fn a_held_port_is_a_failure_before_a_start_and_the_machine_serving_after_one() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-ports");
    let socket = dir.file("docker.sock");
    // Both runs need the daemon; whether it answered is `a_clean_machine...`'s assertion.
    let _answered = fake_daemon(&socket);
    let mut config = config(&dir, Some(format!("unix://{}", socket.display())));
    config.role = Role::Server;
    deployed_data_root(&config);
    let tools = Tools {
        preflight: preflight::Tools {
            git: PathBuf::from("git"),
            df: df_shim(&dir, 480_000_000),
            du: PathBuf::from("du"),
        },
        ..tools(&dir, "2.43.0")
    };

    // `ExecStartPre`'s moment: something holds the control port and this machine is not serving, so a start
    // would fail to bind.
    let held = TcpListener::bind("127.0.0.1:0").expect("a port is bindable");
    let listen = held.local_addr().expect("the bound address is known");
    config.control_listen = listen;
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    let port = nth(&report, Check::Ports, 0);
    assert_eq!(port.outcome, Outcome::Fail, "{}", says(port));
    assert!(says(port).contains("in use"), "{}", says(port));
    assert!(report.failed(), "a held port before a start is a failure");
    drop(held);

    // Item 10 §7's moment: the same port held by a listener whose `/api/health` answers 200 is the machine
    // serving, which is a pass — otherwise a healthy server could never run `doctor --full`.
    let serving = TcpListener::bind(listen).expect("the port is bindable again");
    serving
        .set_nonblocking(true)
        .expect("the listener can stop blocking");
    let answered_health = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&answered_health);
    let seen_stop = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        // The listener stays up for the whole run: `--full` asks it, and then binds the port itself to find it
        // held. A server that answered once and left would make the port free, which is the other arm.
        while !seen_stop.load(Ordering::SeqCst) {
            match serving.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let mut request = [0_u8; 512];
                    let _ = stream.read(&mut request);
                    // The payload, not a bare 200: the reader parses it (HS-050), so `{}` would be "a port
                    // somebody else holds" rather than this machine serving.
                    let body = r#"{"name":"holosmith","version":"verify","role":"server","uptime_s":0,"preview_origin":"http://localhost:8081"}"#;
                    let answer = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(answer.as_bytes());
                    seen.store(true, Ordering::SeqCst);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    let report = doctor::run(&config, Depth::Full, &tools, &FakeMachine::healthy());
    stop.store(true, Ordering::SeqCst);
    thread.join().expect("the answering thread finishes");
    assert!(
        answered_health.load(Ordering::SeqCst),
        "the health endpoint was never asked"
    );
    assert!(
        !report.failed(),
        "a serving machine failed:\n{}",
        report.render()
    );
    let port = nth(&report, Check::Ports, 0);
    assert_eq!(port.outcome, Outcome::Pass, "{}", says(port));
    assert!(
        says(port).contains("/api/health"),
        "the pass does not say how it knew: {}",
        says(port)
    );
}

#[test]
fn a_docker_that_does_not_answer_is_one_failure_and_the_rest_still_prints() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-no-daemon");
    let mut config = config(
        &dir,
        Some(format!("unix://{}", dir.file("absent.sock").display())),
    );
    config.role = Role::Server;
    deployed_data_root(&config);
    let report = doctor::run(
        &config,
        Depth::Quick,
        &tools_with_real_git(&dir),
        &FakeMachine::healthy(),
    );

    let docker = finding(&report, Check::Docker);
    assert_eq!(docker.outcome, Outcome::Fail);
    // The same sentence `serve` refuses with: both numbers, and both fixes.
    assert!(says(docker).contains("start it with"), "{}", says(docker));
    assert!(says(docker).contains("docker_host"), "{}", says(docker));
    assert!(report.failed());
    assert_eq!(
        report.failures().count(),
        1,
        "one failure was reported twice"
    );
    // A machine missing one thing must still be diagnosable for the rest (ADR-0006's rule for config).
    for check in [Check::Git, Check::Disk, Check::Ports, Check::Database] {
        finding(&report, check);
    }

    // And `--full` does not turn one failure into four: its container checks are "not measured".
    let report = doctor::run(
        &config,
        Depth::Full,
        &tools_with_real_git(&dir),
        &FakeMachine::healthy(),
    );
    for check in [Check::Network, Check::Proxy, Check::Image] {
        let finding = finding(&report, check);
        assert_eq!(
            finding.outcome,
            Outcome::Unknown,
            "{} claimed a verdict without a daemon: {}",
            check,
            says(finding)
        );
        assert!(
            says(finding).contains("docker check"),
            "{} does not say why it was not measured: {}",
            check,
            says(finding)
        );
    }
}

#[test]
fn the_merge_trial_measures_a_real_repository_and_refuses_a_git_that_answers_wrong() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-merge");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(&dir, Some(format!("unix://{}", socket.display())));
    config.role = Role::Server;
    deployed_data_root(&config);
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP: this machine has no git");
        return;
    }

    // A real git: V2's two answers, measured on a repository the check creates and removes.
    let report = doctor::run(
        &config,
        Depth::Quick,
        &tools_with_real_git(&dir),
        &FakeMachine::healthy(),
    );
    let merge = finding(&report, Check::Merge);
    assert_eq!(merge.outcome, Outcome::Pass, "{}", says(merge));
    assert!(says(merge).contains("merge-tree"), "{}", says(merge));

    // A git that reports a new version and answers `merge-tree` with one exit code for both trials is exactly
    // the build V2 forbids, and the check must see it.
    let wrong = TempDir::new("doctor-merge-wrong");
    let tools = tools(&wrong, "2.43.0");
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    assert_eq!(finding(&report, Check::Git).outcome, Outcome::Pass);
    let merge = finding(&report, Check::Merge);
    assert_eq!(merge.outcome, Outcome::Fail, "{}", says(merge));
    assert!(says(merge).contains("does not behave"), "{}", says(merge));
}

#[test]
fn an_old_git_fails_the_version_and_leaves_the_merge_trial_unmeasured() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-old-git");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(&dir, Some(format!("unix://{}", socket.display())));
    config.role = Role::Server;
    deployed_data_root(&config);
    // The shim answers a new git's version but not a new git's `merge-tree` (it exits 0 for both trials), so a
    // merge check that ran would *fail*: that it is "not measured" instead is the evidence the version is what
    // stopped it, and the run has one failure rather than two.
    let report = doctor::run(
        &config,
        Depth::Quick,
        &tools(&dir, "2.37.4"),
        &FakeMachine::healthy(),
    );
    let git = finding(&report, Check::Git);
    assert_eq!(git.outcome, Outcome::Fail);
    assert!(says(git).contains("2.38"), "{}", says(git));
    let merge = finding(&report, Check::Merge);
    assert_eq!(merge.outcome, Outcome::Unknown, "{}", says(merge));
    assert!(says(merge).contains("not measured"), "{}", says(merge));
    // The decision the acceptance asks for, in the sentence an operator reads.
    assert!(
        says(merge).contains("fallback is not implemented"),
        "{}",
        says(merge)
    );
    assert!(report.failed(), "an old git must fail `doctor`");
    assert_eq!(report.failures().count(), 1, "one failure for one cause");
}

#[test]
fn tailscales_three_arms_are_measured() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-tailscale");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(&dir, Some(format!("unix://{}", socket.display())));
    config.role = Role::Server;
    deployed_data_root(&config);

    let arms = [
        ("echo '{\"BackendState\":\"Running\"}'", Outcome::Pass),
        ("echo '{\"BackendState\":\"NeedsLogin\"}'", Outcome::Fail),
        (
            "echo 'failed to connect to local tailscaled' >&2; exit 1",
            Outcome::Unknown,
        ),
    ];
    for (index, (body, expected)) in arms.iter().enumerate() {
        // The base tools are built first and the shim written after: a struct update expression's evaluation
        // order is not something a test's subject should depend on.
        let mut tools = tools(&dir, "2.43.0");
        tools.tailscale = tailscale_shim(&dir, body);
        let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
        let finding = finding(&report, Check::Tailscale);
        assert_eq!(
            finding.outcome,
            *expected,
            "arm {index} was not {expected}: {}",
            says(finding)
        );
        match expected {
            Outcome::Fail => assert!(
                says(finding).contains("tailscale up"),
                "the logged-out arm names no fix: {}",
                says(finding)
            ),
            Outcome::Unknown => assert!(
                says(finding).contains("daemon did not answer"),
                "the unmeasured arm does not say what was missing: {}",
                says(finding)
            ),
            Outcome::Pass => {}
        }
    }
}

#[test]
fn systemd_is_a_pass_where_it_is_there_and_unknown_where_it_is_not() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-systemd");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(&dir, Some(format!("unix://{}", socket.display())));
    config.role = Role::Server;
    deployed_data_root(&config);
    let present = dir.file("run-systemd-system");
    std::fs::create_dir_all(&present).expect("the scratch directory is creatable");
    let mut tools = tools_with_real_git(&dir);
    tools.systemd = present;
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    assert_eq!(finding(&report, Check::Systemd).outcome, Outcome::Pass);
    assert!(!report.failed(), "systemd alone must not fail a machine");
}

#[test]
fn a_newer_database_is_the_one_failure_that_keeps_adr_0047s_promise() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-database");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(&dir, Some(format!("unix://{}", socket.display())));
    config.role = Role::Server;
    config.data_root = dir.file("data");
    deployed_data_root(&config);
    let tools = tools(&dir, "2.43.0");

    // No file: a machine that never served, which is not a finding.
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    let database = finding(&report, Check::Database);
    assert_eq!(database.outcome, Outcome::Pass, "{}", says(database));
    assert!(says(database).contains("no database"), "{}", says(database));

    // This release's schema: the state a serving machine is in.
    let path = config.data_root.join(db::DATABASE_FILE);
    {
        let database = Database::open(&path, OpenOptions::default(), clock::unix_now())
            .expect("the scratch database opens");
        assert_eq!(database.schema_version(), holosmith::schema::SCHEMA_VERSION);
    }
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    assert_eq!(finding(&report, Check::Database).outcome, Outcome::Pass);

    // A release from the future: `serve` warns and carries on, `doctor` refuses — and reading it must not have
    // migrated anything (the row is still 99 afterwards).
    let connection = rusqlite::Connection::open(&path).expect("the scratch database reopens");
    connection
        .execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (99, 'from-the-future', 0)",
            [],
        )
        .expect("the future row is writable");
    drop(connection);
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    let database = finding(&report, Check::Database);
    assert_eq!(database.outcome, Outcome::Fail);
    assert!(
        says(database).contains("doctor") && says(database).contains("restore"),
        "the sentence is not the one `serve` warns with: {}",
        says(database)
    );
    assert_eq!(
        db::recorded_without_migrating(&path).expect("the file is readable"),
        Some(99),
        "the doctor migrated the database it was asked to refuse"
    );

    // The other two arms. A database behind this release is a *pass* — the next start migrates it, and forward-only
    // migrations are what make that safe — and a file that is not a SQLite database at all is a failure, because
    // `serve` cannot open it either and `unknown` would hide the one thing to fix.
    let connection = rusqlite::Connection::open(&path).expect("the scratch database reopens");
    connection
        .execute("DELETE FROM schema_version", [])
        .expect("the version rows are deletable");
    drop(connection);
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    let database = finding(&report, Check::Database);
    assert_eq!(database.outcome, Outcome::Pass, "{}", says(database));
    assert!(
        says(database).contains("migrates it"),
        "an older database does not say the next start migrates it: {}",
        says(database)
    );

    std::fs::write(&path, "this is not a database").expect("the scratch file is writable");
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    let database = finding(&report, Check::Database);
    assert_eq!(database.outcome, Outcome::Fail, "{}", says(database));
    assert!(
        says(database).contains("could not be read"),
        "{}",
        says(database)
    );
}

#[test]
fn a_hub_reports_no_docker_git_or_merge_finding() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-hub");
    let mut config = config(&dir, Some("ssh://adam@host".to_string()));
    config.role = Role::Hub;
    let report = doctor::run(
        &config,
        Depth::Quick,
        &tools(&dir, "2.37.4"),
        &FakeMachine::healthy(),
    );
    // Item 10 §6: `hub deploy` runs step 1 "without the Docker and git checks", and an `ssh://` daemon — which is
    // a complaint for a server — is not even asked about here.
    for check in [Check::Docker, Check::Git, Check::Merge] {
        assert!(
            !report.findings.iter().any(|finding| finding.check == check),
            "a hub was asked the {} question:\n{}",
            check,
            report.render()
        );
    }
    for check in [Check::Disk, Check::Tailscale, Check::Ports, Check::Database] {
        finding(&report, check);
    }
    assert!(
        !report.failed(),
        "a hub's list failed on a check it does not have:\n{}",
        report.render()
    );

    // `--full` on a hub still prints the container questions and cannot measure any of them (item 10 §6 runs step 7
    // here too), and the sentence must name the *hub* rather than a `docker` line the report does not contain.
    let report = doctor::run(
        &config,
        Depth::Full,
        &tools(&dir, "2.37.4"),
        &FakeMachine::healthy(),
    );
    for check in [Check::Network, Check::Proxy, Check::Image] {
        let finding = finding(&report, check);
        assert_eq!(finding.outcome, Outcome::Unknown, "{}", says(finding));
        assert!(
            says(finding).contains("a hub runs no sessions"),
            "{} blames a check the report does not have: {}",
            check,
            says(finding)
        );
    }
}

#[test]
fn full_adds_the_self_test_the_proxies_and_the_image() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-full");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(&dir, Some(format!("unix://{}", socket.display())));
    config.role = Role::Server;
    deployed_data_root(&config);
    let tools = tools_with_real_git(&dir);

    let report = doctor::run(&config, Depth::Full, &tools, &FakeMachine::healthy());
    assert!(
        !report.failed(),
        "a healthy full run failed:\n{}",
        report.render()
    );
    for check in [Check::Network, Check::Proxy, Check::Image] {
        assert_eq!(
            finding(&report, check).outcome,
            Outcome::Pass,
            "{} did not pass: {}",
            check,
            says(finding(&report, check))
        );
    }
    assert!(
        says(finding(&report, Check::Image)).contains(holosmith::pins::pi_version()),
        "the image pass does not name the pinned version: {}",
        says(finding(&report, Check::Image))
    );
    // Both proxies, by name — one line each, because `hs_egress` and `hs_key` are two different machines' facts.
    assert!(says(nth(&report, Check::Proxy, 0)).contains("hs_egress"));
    assert!(says(nth(&report, Check::Proxy, 1)).contains("hs_key"));
    assert_eq!(report.render().matches("proxy: pass:").count(), 2);

    // And the failing arms: a proxy that is not running, an image that runs another pi, and a self-test that
    // measured a leak. Each is one line, and the run fails.
    let unhealthy = FakeMachine {
        running: Vec::new(),
        image_pi: Err("no such image".to_string()),
        selftest: selftest::Outcome::Failed("a container reached 1.1.1.1:443".to_string()),
        ..FakeMachine::healthy()
    };
    let report = doctor::run(&config, Depth::Full, &tools, &unhealthy);
    assert_eq!(finding(&report, Check::Proxy).outcome, Outcome::Fail);
    assert!(says(finding(&report, Check::Proxy)).contains("not running"));
    let image = finding(&report, Check::Image);
    assert_eq!(image.outcome, Outcome::Fail);
    assert!(says(image).contains("did not run"), "{}", says(image));
    let network = finding(&report, Check::Network);
    assert_eq!(network.outcome, Outcome::Fail);
    assert_eq!(says(network), "a container reached 1.1.1.1:443");
    assert!(report.failed());

    // A wrong pi is the other image arm: the sentence is the one HS-031 fails a session with, naming both
    // versions.
    let wrong_pi = FakeMachine {
        image_pi: Ok("0.0.0-not-the-pinned-one".to_string()),
        ..FakeMachine::healthy()
    };
    let report = doctor::run(&config, Depth::Full, &tools, &wrong_pi);
    let image = finding(&report, Check::Image);
    assert_eq!(image.outcome, Outcome::Fail);
    assert!(
        says(image).contains("0.0.0-not-the-pinned-one")
            && says(image).contains("rebuild the agent image"),
        "the sentence is not the one a session fails with: {}",
        says(image)
    );
}

/// The `org.holosmith.*` labels an image built by this binary carries, as `docker image inspect` answers them.
///
/// Built from a real `Expectation`, so the digest is the bytes of the test run's own artifact — the label a
/// rebuild at the same `git describe` would change (ADR-0057).
fn labels(config: &Config) -> BTreeMap<String, String> {
    // The product's own memoised digest of this process: the test binary is tens of megabytes and an unoptimised
    // SHA-256 is slow.
    let digest = images::own_digest().expect("the test binary is readable");
    let expectation = images::Expectation::of_digest(images::Kind::Agent, config, &digest);
    let mut labels = BTreeMap::from([
        (images::LABEL_VERSION.to_string(), expectation.version),
        (images::LABEL_GIT_TAG.to_string(), expectation.git_tag),
        (images::LABEL_PI.to_string(), expectation.pi_version),
        (images::LABEL_BINARY.to_string(), expectation.binary),
    ]);
    labels.insert(
        images::LABEL_TOOLCHAINS.to_string(),
        expectation.toolchains.unwrap_or_default().join(" "),
    );
    labels
}

#[test]
fn full_compares_the_agent_images_labels_with_this_binarys() {
    // The pi version is half the question: an image can run the pinned pi and still carry another toolchain list,
    // which is the half `server deploy`'s step 3 compares (ADR-0057).
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-image-labels");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let mut config = config(&dir, Some(format!("unix://{}", socket.display())));
    config.role = Role::Server;
    deployed_data_root(&config);
    let tools = tools_with_real_git(&dir);

    // The image this binary builds: the pi agrees and the four labels say so, each by its own value.
    let built = FakeMachine {
        image_labels: Ok(Some(labels(&config))),
        ..FakeMachine::healthy()
    };
    let report = doctor::run(&config, Depth::Full, &tools, &built);
    let image = finding(&report, Check::Image);
    assert_eq!(image.outcome, Outcome::Pass, "{}", says(image));
    for fact in [
        config.agent_image.clone(),
        env!("CARGO_PKG_VERSION").to_string(),
        holosmith::pins::pi_version().to_string(),
        "python3 ripgrep".to_string(),
    ] {
        assert!(
            says(image).contains(&fact),
            "{fact} is not in the pass: {}",
            says(image)
        );
    }

    // The same image with another toolchain list: the pi check passes and the labels are what fails, naming both
    // the value the image carries and the one this binary would build.
    let mut mismatched = labels(&config);
    mismatched.insert(images::LABEL_TOOLCHAINS.to_string(), "python3".to_string());
    let stale = FakeMachine {
        image_labels: Ok(Some(mismatched)),
        ..FakeMachine::healthy()
    };
    let report = doctor::run(&config, Depth::Full, &tools, &stale);
    let image = finding(&report, Check::Image);
    assert_eq!(image.outcome, Outcome::Fail, "{}", says(image));
    for fact in [
        "toolchains".to_string(),
        "python3 ripgrep".to_string(),
        "server deploy".to_string(),
    ] {
        assert!(
            says(image).contains(&fact),
            "the failure does not name {fact}: {}",
            says(image)
        );
    }

    // An image with no Holosmith labels at all is a pass with a remark: `pi-agent:local` is a supported machine,
    // and failing it would refuse one the spec allows.
    let report = doctor::run(&config, Depth::Full, &tools, &FakeMachine::healthy());
    let image = finding(&report, Check::Image);
    assert_eq!(image.outcome, Outcome::Pass, "{}", says(image));
    assert!(
        says(image).contains(holosmith::pins::pi_version()),
        "{}",
        says(image)
    );
    assert!(
        says(image).contains("not built from this binary's Dockerfile"),
        "the pass does not say what it could not compare: {}",
        says(image)
    );

    // An image that answers a run and not an inspect is a machine problem, and it still costs a line: a label read
    // that failed silently would make a mismatched image a pass.
    let unreadable = FakeMachine {
        image_labels: Err("permission denied".to_string()),
        ..FakeMachine::healthy()
    };
    let report = doctor::run(&config, Depth::Full, &tools, &unreadable);
    let image = finding(&report, Check::Image);
    assert_eq!(image.outcome, Outcome::Fail, "{}", says(image));
    assert!(says(image).contains("permission denied"), "{}", says(image));

    // The daemon answering "there" to a run and "not there" to an inspect is a machine problem too, and it is not
    // the unlabelled pass: nothing about this image was compared.
    let vanished = FakeMachine {
        image_labels: Ok(None),
        ..FakeMachine::healthy()
    };
    let report = doctor::run(&config, Depth::Full, &tools, &vanished);
    let image = finding(&report, Check::Image);
    assert_eq!(image.outcome, Outcome::Fail, "{}", says(image));
    assert!(
        says(image).contains("was not there when its labels were read"),
        "{}",
        says(image)
    );
}

#[test]
fn the_report_is_the_preflights_sentences_and_not_a_second_copy() {
    let _serial = one_at_a_time();
    // The rule ADR-0028 and ADR-0054 share: what `serve` refuses with is the failing projection of what `doctor`
    // prints, so the two cannot drift.
    let dir = TempDir::new("doctor-projection");
    let mut config = config(&dir, Some("unix://absent.sock".to_string()));
    config.role = Role::Server;
    let tools = tools(&dir, "2.37.4");
    let report = preflight::report_with(&config, &tools.preflight);
    let complaints = preflight::check_with(&config, &tools.preflight);
    let failures: Vec<&Finding> = report
        .iter()
        .filter(|finding| finding.is_failure())
        .collect();
    assert_eq!(
        failures.len(),
        complaints.len(),
        "the complaints are not the report's failures"
    );
    for (finding, complaint) in failures.iter().zip(&complaints) {
        assert_eq!(complaint.check, finding.check);
        assert_eq!(complaint.sentence, finding.sentence);
    }
}

#[test]
fn the_installed_paths_modes_and_owners_are_one_finding() {
    // One check for the whole tree, because that is how an operator fixes it: the first offender's sentence carries
    // the path, both numbers and the command, and a `Finding` is one check and one sentence.
    let _serial = one_at_a_time();
    let dir = TempDir::new("doctor-perms");
    let socket = dir.file("docker.sock");
    let _answered = fake_daemon(&socket);
    let mut config = config(&dir, Some(format!("unix://{}", socket.display())));
    config.role = Role::Server;
    let mut tools = tools_with_real_git(&dir);
    // The whole tree is this test's: the config file and the admin directory are `Tools`' paths, which is what a
    // `--root` deploy hands `doctor` (SPEC-1's prolongation) — so this arm never reads `/etc/holosmith` or
    // `/run/holosmith`, which a host that has deployed for real would otherwise contribute to the verdict.
    tools.config_path = dir.file("etc/holosmith/config.toml");
    tools.admin_dir = dir.file("run/holosmith");
    let secrets = config.data_root.join(key::SECRETS_DIR);
    std::fs::create_dir_all(&secrets).expect("the secrets directory is creatable");
    std::fs::set_permissions(&config.data_root, std::fs::Permissions::from_mode(0o755))
        .expect("the data root's mode is settable");
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o700))
        .expect("the secrets directory's mode is settable");
    for name in [key::KEYS_FILE, vapid::VAPID_FILE] {
        let path = secrets.join(name);
        std::fs::write(&path, "").expect("the installed file is writable");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("the installed file's mode is settable");
    }

    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    let perms = finding(&report, Check::Perms);
    assert_eq!(perms.outcome, Outcome::Pass, "{}", says(perms));
    // A path the deploy has not written yet is not a wrong mode, and the pass says which ones it did not find.
    assert!(says(perms).contains("not there yet"), "{}", says(perms));
    for absent in [&tools.admin_dir, &tools.config_path] {
        assert!(
            says(perms).contains(&absent.display().to_string()),
            "the pass does not say {} is not there yet: {}",
            absent.display(),
            says(perms)
        );
    }

    // The two machine-wide paths exist and are right: the pass names no absence, which is what makes the arm above
    // an absence arm rather than a sentence that always says it.
    std::fs::create_dir_all(
        tools
            .admin_dir
            .parent()
            .expect("the admin directory has a parent"),
    )
    .expect("the admin tree is creatable");
    std::fs::create_dir_all(
        tools
            .config_path
            .parent()
            .expect("the config file has a parent"),
    )
    .expect("the etc tree is creatable");
    for (path, mode) in [
        (tools.admin_dir.clone(), 0o700),
        (tools.config_path.clone(), 0o644),
    ] {
        std::fs::write(&path, "").expect("the installed path is writable");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            .expect("the installed path's mode is settable");
    }
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    let perms = finding(&report, Check::Perms);
    assert_eq!(perms.outcome, Outcome::Pass, "{}", says(perms));
    assert!(
        !says(perms).contains("not there yet"),
        "a complete tree still names an absence: {}",
        says(perms)
    );

    // One wrong mode: a failure naming the path, both modes and the fix — and the only failure on this machine.
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o755))
        .expect("the secrets directory's mode is settable");
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    let perms = finding(&report, Check::Perms);
    assert_eq!(perms.outcome, Outcome::Fail, "{}", says(perms));
    for fact in [
        "0755".to_string(),
        "0700".to_string(),
        format!("chmod 0700 {}", secrets.display()),
    ] {
        assert!(
            says(perms).contains(&fact),
            "the failure does not carry {fact}: {}",
            says(perms)
        );
    }
    assert_eq!(
        report.failures().count(),
        1,
        "one wrong mode was reported as more than one failure:\n{}",
        report.render()
    );

    // The same finding on a path *outside* the data root: without `Tools`' two fields this arm would read the
    // machine's own `/run/holosmith` and pass over a tree it never looked at.
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o700))
        .expect("the secrets directory's mode is settable");
    std::fs::set_permissions(&tools.admin_dir, std::fs::Permissions::from_mode(0o755))
        .expect("the admin directory's mode is settable");
    let report = doctor::run(&config, Depth::Quick, &tools, &FakeMachine::healthy());
    let perms = finding(&report, Check::Perms);
    assert_eq!(perms.outcome, Outcome::Fail, "{}", says(perms));
    assert!(
        says(perms).contains(&tools.admin_dir.display().to_string()),
        "the failure does not name the admin directory: {}",
        says(perms)
    );
    std::fs::set_permissions(&tools.admin_dir, std::fs::Permissions::from_mode(0o700))
        .expect("the admin directory's mode is settable");

    // A machine that has not deployed: no tree, so not a verdict about modes — the third answer, not a failure.
    let mut missing = config.clone();
    missing.data_root = dir.file("no-data-root");
    let report = doctor::run(&missing, Depth::Quick, &tools, &FakeMachine::healthy());
    let perms = finding(&report, Check::Perms);
    assert_eq!(perms.outcome, Outcome::Unknown, "{}", says(perms));
    assert!(
        says(perms).contains("not there yet"),
        "the unknown answer does not say what is missing: {}",
        says(perms)
    );
    assert!(
        !report.failed(),
        "a machine that has not deployed failed `doctor`:\n{}",
        report.render()
    );
}
