//! `uninstall`: the words, the tree, the refusal and the privilege (HS-050, spec's Commands table).
//!
//! The discipline is `tests/deploy.rs`'s, for the reason that story recorded: a teardown is a set of claims about
//! *a host* — its unit, its systemd, its daemon, its paths — so every claim that can be handed in is, and the two
//! that cannot be faked are driven for real.
//!
//! **Driven for real**: `rm` over a real `--root` install tree (a recorder would prove nothing about the file that
//! is the step's deliverable), and a real SQLite database with a real row (the refusal is a `live_count` over the
//! table, and a fake count would be a test of the fake).
//!
//! **Handed in**: `systemctl` and `sudo` are scripts this test wrote ([`uninstall::Tools`]), the daemon is a
//! [`Fake`] ([`uninstall::Machines`]), and `--root` is what keeps every path inside the temp directory.
//!
//! What this file cannot reach is the **system path's unit file**, and it says so rather than pretending: a
//! privileged run whose `/etc/systemd/system/holosmith.service` is absent has nothing to remove, so the system-path
//! arms here are the ones that ask `systemctl` (a unit systemd knows) and prefix a write with `sudo`. The gate's
//! `case_uninstall` drives the `--root` arm against the shipped artifact, and the by-hand procedure in
//! `docs/build.md` is what proves the privileged one on a machine that has a unit installed.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use holosmith::config::{Config, Role};
use holosmith::container;
use holosmith::db;
use holosmith::deploy::Target;
use holosmith::sessions::NewSession;
use holosmith::uninstall::{self, Args, Machines, Word};

mod support;
use support::TempDir;

/// Hold this for the whole body of a test here: one test at a time.
///
/// `tests/deploy.rs`'s trap and its fix: `exec` of a file any process holds open for writing fails with `ETXTBSY`,
/// and a forked child inherits the write descriptor of a shim written a moment earlier.
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
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(&path)
        .expect("the shim exists")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("a shim is executable");
    path
}

/// A `sudo` that records its argv and runs nothing — `tests/deploy.rs`'s, for its own reason: the argv here names
/// `/etc/systemd/system`, and a shim that forwarded to the real `sudo` would remove a unit from the machine running
/// the tests.
fn sudo_recorder(dir: &TempDir, log: &Path) -> PathBuf {
    shim(
        dir,
        "sudo",
        &format!("printf 'sudo %s\\n' \"$*\" >> '{}'", log.display()),
    )
}

/// A `rm` that records its argv and removes nothing.
fn rm_recorder(dir: &TempDir, log: &Path) -> PathBuf {
    shim(
        dir,
        "rm",
        &format!("printf 'rm %s\\n' \"$*\" >> '{}'", log.display()),
    )
}

/// A `systemctl` that records its argv, answering `is-enabled` with `enabled`.
///
/// The exit code is the read: `enabled` is the whole knob, and it is the question a privileged run asks when no unit
/// file is there.
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

/// The daemon, as a value: which containers and networks exist, and what it was asked to remove.
///
/// `AtomicBool`/`Mutex` because the seam takes `&self`, exactly as a daemon handle does: a second run in one test
/// has to see what the first removed.
#[derive(Default)]
struct Fake {
    containers: Mutex<Vec<String>>,
    networks: Mutex<Vec<String>>,
    removed_containers: Mutex<Vec<String>>,
    removed_networks: Mutex<Vec<String>>,
    /// A removal that refuses, for the arm where the daemon says no.
    refuse_removal: bool,
}

impl Fake {
    fn with(containers: &[&str], networks: &[&str]) -> Self {
        Self {
            containers: Mutex::new(containers.iter().map(|name| (*name).to_string()).collect()),
            networks: Mutex::new(networks.iter().map(|name| (*name).to_string()).collect()),
            ..Self::default()
        }
    }

    fn removed_containers(&self) -> Vec<String> {
        self.removed_containers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn removed_networks(&self) -> Vec<String> {
        self.removed_networks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Machines for Fake {
    fn exists(&self, name: &str) -> bool {
        self.containers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|there| there == name)
    }

    fn remove_container(&self, name: &str) -> bool {
        if self.refuse_removal {
            return false;
        }
        let mut containers = self
            .containers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        containers.retain(|there| there != name);
        drop(containers);
        self.removed_containers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(name.to_string());
        true
    }

    fn network_names(
        &self,
        filter: &str,
        _bound: std::time::Duration,
    ) -> Result<Vec<String>, String> {
        Ok(self
            .networks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|name| name.contains(filter))
            .cloned()
            .collect())
    }

    fn remove_network(&self, name: &str) -> bool {
        if self.refuse_removal {
            return false;
        }
        let mut networks = self
            .networks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        networks.retain(|there| there != name);
        drop(networks);
        self.removed_networks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(name.to_string());
        true
    }
}

/// A machine config: the spec's own paths by default (so `--root` prefixes `/var/lib/holosmith`, as a deploy
/// writes it), a listener the kernel would give, and no daemon handle of its own.
fn config() -> Config {
    Config {
        role: Role::Server,
        ..Config::default()
    }
}

/// The `--root` target under `dir`, and the config the steps see (`Target::config`'s contract).
fn root_target(dir: &TempDir) -> (Target, Config) {
    let target = Target::resolve(Some(dir.file("install")));
    let deployed = target.config(&config());
    (target, deployed)
}

/// The tree a `server deploy --root` leaves: the unit, the data root with its 0700 secrets and the two files.
fn installed_tree(target: &Target, deployed: &Config) {
    let unit = target.unit_path();
    std::fs::create_dir_all(unit.parent().expect("the unit has a directory"))
        .expect("the unit's directory is creatable");
    std::fs::write(&unit, "[Service]\nExecStart=/usr/bin/holosmith serve\n")
        .expect("the unit is writable");
    let secrets = deployed.data_root.join("secrets");
    std::fs::create_dir_all(&secrets).expect("the secrets directory is creatable");
    std::fs::write(secrets.join("keys.toml"), "provider = \"anthropic\"\n")
        .expect("the key store is writable");
    std::fs::write(secrets.join("vapid.toml"), "pair\n").expect("the pair is writable");
}

/// A database at `root`'s data root with one row in `status`.
fn seed_row(deployed: &Config, status: &str) -> String {
    std::fs::create_dir_all(&deployed.data_root).expect("the data root is creatable");
    let database = db::Database::open(
        &deployed.data_root.join(db::DATABASE_FILE),
        db::OpenOptions::default(),
        1,
    )
    .expect("a fresh database opens");
    let session = NewSession {
        project: "holosmith".to_string(),
        model: "m".to_string(),
        thinking: "high".to_string(),
        network: false,
        client_key: None,
        base_commit: None,
    };
    let row = database
        .sessions()
        .create(&session, 1)
        .expect("a row is written");
    if status != row.status.as_str() {
        database
            .sessions()
            .set_status(
                &row.id,
                status.parse().expect("a status word"),
                Some("test"),
                1,
            )
            .expect("the row moves");
    }
    row.id
}

/// Every word of a report, in order, for a whole-report assertion.
fn words(report: &uninstall::Report) -> Vec<(&'static str, Word)> {
    report
        .lines
        .iter()
        .map(|line| (line.what.as_str(), line.word))
        .collect()
}

#[test]
fn a_root_install_loses_the_unit_and_keeps_the_data() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("uninstall-root");
    let (target, deployed) = root_target(&dir);
    installed_tree(&target, &deployed);
    let machine = Fake::default();
    let (sudo_log, systemctl_log) = (dir.file("sudo.log"), dir.file("systemctl.log"));
    let tools = uninstall::Tools {
        sudo: sudo_recorder(&dir, &sudo_log),
        rm: PathBuf::from("rm"),
        systemctl: systemctl_shim(&dir, &systemctl_log, false),
        ..uninstall::Tools::default()
    };
    let report = uninstall::run(&deployed, &target, &tools, &machine, &Args::default())
        .expect("nothing refuses this run");
    assert_eq!(
        words(&report),
        [
            ("unit", Word::Removed),
            ("proxies", Word::Nothing),
            ("network", Word::Nothing),
            ("data", Word::Kept),
        ],
        "{}",
        report.render()
    );
    assert!(!target.unit_path().exists(), "the unit file is gone");
    assert!(
        deployed
            .data_root
            .join("secrets")
            .join("keys.toml")
            .exists(),
        "the provider keys are data, and --purge is the only thing that removes them"
    );
    assert!(
        deployed.data_root.exists(),
        "a plain uninstall keeps the data root"
    );
    // A `--root` unit is one systemd cannot read, so the step asks it nothing at all — a child here would be a
    // pretend half of the install.
    assert!(
        !systemctl_log.exists(),
        "a --root uninstall asked systemd: {}",
        std::fs::read_to_string(&systemctl_log).unwrap_or_default()
    );
    assert!(
        !sudo_log.exists(),
        "a --root uninstall used sudo: {}",
        std::fs::read_to_string(&sudo_log).unwrap_or_default()
    );
}

#[test]
fn the_second_run_says_nothing_and_keeps_succeeding() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("uninstall-again");
    let (target, deployed) = root_target(&dir);
    installed_tree(&target, &deployed);
    let machine = Fake::default();
    let tools = uninstall::Tools {
        sudo: sudo_recorder(&dir, &dir.file("sudo.log")),
        rm: PathBuf::from("rm"),
        systemctl: systemctl_shim(&dir, &dir.file("systemctl.log"), false),
        ..uninstall::Tools::default()
    };
    let first = uninstall::run(&deployed, &target, &tools, &machine, &Args::default())
        .expect("nothing refuses this run");
    assert_eq!(first.lines[0].word, Word::Removed);
    let second = uninstall::run(&deployed, &target, &tools, &machine, &Args::default())
        .expect("nothing refuses the second run either");
    assert_eq!(
        words(&second),
        [
            ("unit", Word::Nothing),
            ("proxies", Word::Nothing),
            ("network", Word::Nothing),
            ("data", Word::Kept),
        ],
        "{}",
        second.render()
    );
    assert!(!second.failed(), "a teardown of nothing is not a failure");
}

#[test]
fn the_proxies_and_the_egress_network_go_containers_first() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("uninstall-proxies");
    let (target, deployed) = root_target(&dir);
    let machine = Fake::with(
        &[container::PROXY_NAME, holosmith::key::KEY_PROXY_NAME],
        &[holosmith::docker::EGRESS_NETWORK, "agents_0123456789abcdef"],
    );
    let tools = uninstall::Tools {
        sudo: sudo_recorder(&dir, &dir.file("sudo.log")),
        rm: PathBuf::from("rm"),
        systemctl: systemctl_shim(&dir, &dir.file("systemctl.log"), false),
        ..uninstall::Tools::default()
    };
    let report = uninstall::run(&deployed, &target, &tools, &machine, &Args::default())
        .expect("nothing refuses this run");
    assert_eq!(
        words(&report),
        [
            ("unit", Word::Nothing),
            ("proxies", Word::Removed),
            ("network", Word::Removed),
            ("data", Word::Kept),
        ],
        "{}",
        report.render()
    );
    assert_eq!(
        machine.removed_containers(),
        [container::PROXY_NAME, holosmith::key::KEY_PROXY_NAME],
        "both proxies are removed, by their own names"
    );
    assert_eq!(
        machine.removed_networks(),
        [holosmith::docker::EGRESS_NETWORK],
        "the machine's network goes, and a session's does not: a session's names belong to the row that holds it"
    );
}

#[test]
fn a_refused_removal_is_a_failed_step_and_stops_the_run() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("uninstall-refused");
    let (target, deployed) = root_target(&dir);
    installed_tree(&target, &deployed);
    let machine = Fake {
        containers: Mutex::new(vec![container::PROXY_NAME.to_string()]),
        refuse_removal: true,
        ..Fake::default()
    };
    let tools = uninstall::Tools {
        sudo: sudo_recorder(&dir, &dir.file("sudo.log")),
        rm: PathBuf::from("rm"),
        systemctl: systemctl_shim(&dir, &dir.file("systemctl.log"), false),
        ..uninstall::Tools::default()
    };
    let report = uninstall::run(&deployed, &target, &tools, &machine, &Args::default())
        .expect("nothing refuses this run");
    assert_eq!(
        words(&report),
        [("unit", Word::Removed), ("proxies", Word::Failed),],
        "a failed step stops the run: {}",
        report.render()
    );
    assert!(report.failed());
    assert!(
        deployed.data_root.exists(),
        "the steps after the failure did not run, so the data is untouched"
    );
}

#[test]
fn a_purge_refuses_over_a_live_session_and_removes_nothing() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("uninstall-refusal");
    let (target, deployed) = root_target(&dir);
    installed_tree(&target, &deployed);
    // `starting` is live: the three statuses the cap counts and `live_count` answers with.
    let id = seed_row(&deployed, "starting");
    let machine = Fake::default();
    let tools = uninstall::Tools {
        sudo: sudo_recorder(&dir, &dir.file("sudo.log")),
        rm: PathBuf::from("rm"),
        systemctl: systemctl_shim(&dir, &dir.file("systemctl.log"), false),
        ..uninstall::Tools::default()
    };
    let sentence = uninstall::run(
        &deployed,
        &target,
        &tools,
        &machine,
        &Args {
            purge: true,
            force: false,
        },
    )
    .expect_err("a live session refuses the purge");
    assert!(sentence.contains('1'), "the count is named: {sentence}");
    assert!(
        sentence.contains("live"),
        "the refusal says what is live: {sentence}"
    );
    assert!(
        sentence.contains("--force"),
        "the refusal names the override: {sentence}"
    );
    // Nothing ran: the unit is still there, the tree is still there, and systemd was not asked.
    assert!(target.unit_path().exists(), "the refusal removed the unit");
    assert!(deployed.data_root.exists(), "the refusal removed the data");
    assert!(!dir.file("systemctl.log").exists());
    // The row is still in the table, which is what the re-read proves: the refusal happened before anything.
    let reader = db::Reader::open(&deployed.data_root.join(db::DATABASE_FILE))
        .expect("the database reads")
        .expect("the database is there");
    assert_eq!(
        reader.sessions().live_count().expect("the count reads"),
        1,
        "the refusal left the row alone",
        // `id` is only used to prove the fixture: the row exists.
    );
    let _ = id;
}

#[test]
fn a_forced_purge_removes_the_data_and_the_rows_own_names() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("uninstall-forced");
    let (target, deployed) = root_target(&dir);
    installed_tree(&target, &deployed);
    let id = seed_row(&deployed, "starting");
    let machine = Fake::with(
        &[&format!("s_{id}")],
        &[&format!("agents_{id}"), "agents_0123456789abcdef"],
    );
    let tools = uninstall::Tools {
        sudo: sudo_recorder(&dir, &dir.file("sudo.log")),
        rm: PathBuf::from("rm"),
        systemctl: systemctl_shim(&dir, &dir.file("systemctl.log"), false),
        ..uninstall::Tools::default()
    };
    let report = uninstall::run(
        &deployed,
        &target,
        &tools,
        &machine,
        &Args {
            purge: true,
            force: true,
        },
    )
    .expect("--force purges over a live session");
    assert_eq!(
        words(&report).last(),
        Some(&("data", Word::Removed)),
        "{}",
        report.render()
    );
    assert!(!deployed.data_root.exists(), "the data root is gone");
    assert_eq!(
        machine.removed_containers(),
        [format!("s_{id}")],
        "the container the row named went with it"
    );
    assert_eq!(
        machine.removed_networks(),
        [format!("agents_{id}")],
        "and its network — and not the other session's, whose row this data root never held"
    );
    // The projects are not this command's, and the sentence says so.
    let sentence = &report.lines.last().expect("a data line").sentence;
    assert!(sentence.contains("projects"), "{sentence}");
}

#[test]
fn the_system_path_asks_systemd_and_prefixes_its_write_with_sudo() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("uninstall-system");
    // No `--root`: the system paths, and `privileged()`. The config's data root is this test's temp directory, so
    // nothing the run says names the machine's real one — and every write is a recorder.
    let target = Target::resolve(None);
    let deployed = Config {
        data_root: dir.file("data"),
        ..config()
    };
    let machine = Fake::default();
    let (sudo_log, rm_log, systemctl_log) = (
        dir.file("sudo.log"),
        dir.file("rm.log"),
        dir.file("systemctl.log"),
    );
    let tools = uninstall::Tools {
        sudo: sudo_recorder(&dir, &sudo_log),
        rm: rm_recorder(&dir, &rm_log),
        // systemd knows a unit whose file is not there: a linked unit, or one left loaded after a hand removal.
        systemctl: systemctl_shim(&dir, &systemctl_log, true),
        ..uninstall::Tools::default()
    };
    let report = uninstall::run(&deployed, &target, &tools, &machine, &Args::default())
        .expect("nothing refuses this run");
    assert_eq!(
        words(&report)[0],
        ("unit", Word::Removed),
        "{}",
        report.render()
    );
    let asked = std::fs::read_to_string(&systemctl_log).expect("systemctl was asked");
    for argv in [
        "systemctl is-enabled holosmith.service",
        "systemctl disable --now holosmith.service",
        "systemctl daemon-reload",
    ] {
        assert!(asked.contains(argv), "no `{argv}`: {asked}");
    }
    // The unit's *file* was not there, so no write was needed and none ran: the systemd verbs are what this arm is
    // about, and a `rm` over a path that does not exist would be a child the read had already answered.
    assert!(
        !rm_log.exists() && !sudo_log.exists(),
        "a run with no file to remove wrote something: {} {}",
        std::fs::read_to_string(&rm_log).unwrap_or_default(),
        std::fs::read_to_string(&sudo_log).unwrap_or_default()
    );
}

#[test]
fn a_purge_is_one_sudo_prefixed_child_on_a_system_path_install() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("uninstall-sudo");
    let target = Target::resolve(None);
    // The data root is a temp directory, so the recorder's `rm` names something this machine does not have — which
    // is the point: `tests/deploy.rs`'s `sudo` recorder exists because a system-path run names real paths.
    let deployed = Config {
        data_root: dir.file("data"),
        ..config()
    };
    std::fs::create_dir_all(&deployed.data_root).expect("the data root is creatable");
    let (sudo_log, rm_log) = (dir.file("sudo.log"), dir.file("rm.log"));
    let tools = uninstall::Tools {
        sudo: sudo_recorder(&dir, &sudo_log),
        rm: rm_recorder(&dir, &rm_log),
        systemctl: systemctl_shim(&dir, &dir.file("systemctl.log"), false),
        ..uninstall::Tools::default()
    };
    let report = uninstall::run(
        &deployed,
        &target,
        &tools,
        &Fake::default(),
        &Args {
            purge: true,
            force: false,
        },
    )
    .expect("no session is live, so nothing refuses this purge");
    assert_eq!(
        words(&report).last(),
        Some(&("data", Word::Removed)),
        "{}",
        report.render()
    );
    let sudo = std::fs::read_to_string(&sudo_log).expect("sudo was used");
    assert!(
        sudo.starts_with("sudo ")
            && sudo.contains(&format!(" -rf {}", deployed.data_root.display())),
        "the purge is one sudo-prefixed `rm -rf`: {sudo}"
    );
    assert!(
        !rm_log.exists(),
        "the tree's `rm` ran without sudo: {}",
        std::fs::read_to_string(&rm_log).unwrap_or_default()
    );
}

#[test]
fn a_machine_with_nothing_installed_is_nothing_rather_than_a_failure() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("uninstall-empty");
    let target = Target::resolve(None);
    let deployed = Config {
        data_root: dir.file("data"),
        ..config()
    };
    let machine = Fake::default();
    let tools = uninstall::Tools {
        sudo: sudo_recorder(&dir, &dir.file("sudo.log")),
        rm: rm_recorder(&dir, &dir.file("rm.log")),
        // systemd answers that it knows no such unit, which is what a machine that has never deployed looks like.
        systemctl: systemctl_shim(&dir, &dir.file("systemctl.log"), false),
        ..uninstall::Tools::default()
    };
    let report = uninstall::run(&deployed, &target, &tools, &machine, &Args::default())
        .expect("nothing refuses this run");
    assert_eq!(
        words(&report),
        [
            ("unit", Word::Nothing),
            ("proxies", Word::Nothing),
            ("network", Word::Nothing),
            ("data", Word::Kept),
        ],
        "{}",
        report.render()
    );
    assert!(!report.failed());
    assert!(!dir.file("sudo.log").exists(), "nothing was removed");
    // The one child this run paid: the question that tells "no unit" from "a unit systemd knows".
    assert_eq!(
        std::fs::read_to_string(dir.file("systemctl.log")).unwrap_or_default(),
        "systemctl is-enabled holosmith.service\n"
    );
}

#[test]
fn the_report_prints_its_target_and_one_line_per_thing() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("uninstall-render");
    let (target, deployed) = root_target(&dir);
    let tools = uninstall::Tools {
        sudo: sudo_recorder(&dir, &dir.file("sudo.log")),
        rm: PathBuf::from("rm"),
        systemctl: systemctl_shim(&dir, &dir.file("systemctl.log"), false),
        ..uninstall::Tools::default()
    };
    let report = uninstall::run(
        &deployed,
        &target,
        &tools,
        &Fake::default(),
        &Args::default(),
    )
    .expect("nothing refuses this run");
    let text = report.render();
    let mut lines = text.lines();
    let header = lines.next().expect("a header line");
    assert_eq!(
        header,
        format!("uninstall: from {}", target.describe()),
        "the header names the tree and the privilege: {text}"
    );
    let rest: Vec<&str> = lines.collect();
    assert_eq!(rest.len(), 4, "{text}");
    for (line, what) in rest.iter().zip(["unit", "proxies", "network", "data"]) {
        assert!(
            line.starts_with(&format!("uninstall: {what}: ")),
            "{line} does not start with `uninstall: {what}: `"
        );
    }
    // The report's own words, and none of the deploy's: ADR-0056's five belong to `server deploy`.
    for word in ["satisfied", "needs", "changed", "skipped"] {
        assert!(!text.contains(word), "{word} is a deploy word: {text}");
    }
}

#[test]
fn a_root_target_prefixes_the_system_paths_and_a_plain_one_does_not() {
    let dir = TempDir::new("uninstall-paths");
    let (target, deployed) = root_target(&dir);
    assert_eq!(
        target.unit_path(),
        dir.file("install/etc/systemd/system/holosmith.service")
    );
    assert_eq!(deployed.data_root, dir.file("install/var/lib/holosmith"));
    let plain = Target::resolve(None);
    assert_eq!(
        plain.unit_path(),
        Path::new("/etc/systemd/system/holosmith.service")
    );
    assert_eq!(plain.data_root(&config()), Path::new("/var/lib/holosmith"));
}
