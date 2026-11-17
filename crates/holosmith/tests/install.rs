//! `holosmith install`: the one command, its one refusal, and the two runs that end it.
//!
//! The discipline is `tests/deploy.rs`'s, because this command is a deploy plus two `systemctl --user` commands and a
//! `loginctl`: a set of claims about *a host*, so every claim that can be handed in is. `systemctl`, `loginctl`, `id`,
//! `df` and `tailscale` are scripts this test wrote (the [`install::Tools`] and [`doctor::Tools`] seams), systemd is
//! a directory it leaves out, and `--root`/`--config` keep both trees inside the temp directory.
//!
//! **The machine is a hub's on purpose.** A hub runs no session, so `doctor` asks it no Docker and no git question and
//! every container finding is `unknown` rather than a failure — which is what lets every case here run with no daemon,
//! no image and no proxy in the picture. What a *server* deploy then does with a real daemon is `tests/deploy.rs`'s
//! subject, and the one thing this file still needs a server for — the daemon's group — is a read that happens before
//! any of it.
//!
//! **Which layer each exit class is pinned at.** `install::run` is the glue that turns a report into an exit code, and
//! no case here drives it: its tool seam is `install::Tools::default`, which is the host's real `systemctl` and
//! `loginctl`, and its config comes from a file rather than a value. What is pinned instead is the predicate those
//! lines read — `install::Report::failed` — at the layer that can fail, one case per class:
//!
//! - **a refusal is non-zero with nothing written** — `an_account_outside_the_docker_group_is_refused_and_nothing_is_written`,
//!   and `a_dry_run_is_refused_too` for the arm that has no plan either;
//! - **a fresh machine succeeds** — `a_first_install_whose_deploy_health_line_failed_still_succeeds`, where the
//!   deploy's own step 6 fails and install's wait is the verdict;
//! - **a stopped deploy fails** — `a_deploy_step_that_is_not_health_stops_the_run`, which is the one failed step an
//!   install does not tolerate;
//! - **an install that does not answer fails** — `a_health_poll_that_never_answers_fails_and_names_the_journal`.
//!
//! So the glue no case here drives is `run`'s own three lines — print the report, print `resume` or the URL and the
//! next step, return the code. `scripts/verify.sh`'s `case_install` runs the verb end to end under `bwrap` with a home
//! and a unit tree of its own, and that is where the report, the URL and next step, and the exit code are measured; a
//! failing install's `install: resume:` line is still measured nowhere.
//!
//! Driven for real: `install`/`chmod` and `openssl` over the `--root` tree (a recorder would prove nothing about the
//! modes step 2 writes), and a real socket answering `/api/health` (the wait is the install's own verdict).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use holosmith::commands::install::{self, What};
use holosmith::config::{Config, Origin, Role};
use holosmith::deploy::{self, DeployStep, Root, Word};
use holosmith::doctor;
use holosmith::images;
use holosmith::key;
use holosmith::preflight;
use holosmith::progress::{Progress, Silent};
use holosmith::proxies;
use holosmith::selftest;

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
    let mut permissions = std::fs::metadata(&path)
        .expect("the shim exists")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("a shim is executable");
    path
}

/// Every line a recorder wrote to `log`, in the order it wrote them.
fn diary(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The lines a run **applied** rather than read: `daemon-reload`, `enable --now` and `enable-linger`.
///
/// One predicate for both recorders, because the claim "a second run changes nothing" is about all three — and the
/// reads (`is-enabled`, `is-active`, `show-user`) have to be told apart from them by something other than the word
/// "enabled", which every one of the six argv contains.
fn applied(lines: &[String]) -> usize {
    lines
        .iter()
        .filter(|line| {
            line.contains(" daemon-reload")
                || line.contains(" enable ")
                || line.contains(" enable-linger")
        })
        .count()
}

/// An `id` answering the two questions a run asks about the account it is running as.
fn id_shim(dir: &TempDir, name: &str, groups: &str) -> PathBuf {
    shim(
        dir,
        "id",
        &format!(
            "case \"$1\" in\n  -un) echo {name} ;;\n  -nG) echo '{groups}' ;;\n  *) exit 1 ;;\nesac"
        ),
    )
}

/// A `df -Pk` answer of 480 GB free, so the preflight's disk floor is met.
fn df_shim(dir: &TempDir) -> PathBuf {
    shim(
        dir,
        "df",
        "printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n\
         /dev/mapper/root 1000000000 1 480000000 1%% /\\n'",
    )
}

/// A `tailscale status --json` that says this node is logged in, which is a `pass` rather than the `unknown` a
/// missing CLI earns.
fn tailscale_shim(dir: &TempDir) -> PathBuf {
    shim(
        dir,
        "tailscale",
        "case \"$1 $2\" in\n  'status --json') echo '{\"BackendState\":\"Running\"}' ;;\n  *) exit 1 ;;\nesac",
    )
}

/// `systemctl`, answering the two reads and recording the two applies — one line per call, into `systemctl.log`.
///
/// The state is a file rather than the shim's memory, because a second run in the same test has to see what the first
/// one enabled: `--user enable --now` writes it and `is-enabled`/`is-active` read it, which is the only way
/// "the second run changes nothing" can be a measurement.
fn systemctl_shim(dir: &TempDir) -> PathBuf {
    shim(
        dir,
        "systemctl",
        &format!(
            r#"log='{log}'
state='{state}'
printf 'systemctl %s\n' "$*" >> "$log"
case "$1 $2" in
  '--user is-enabled')
    if [ -e "$state" ]; then echo enabled; exit 0; else echo disabled; exit 1; fi ;;
  '--user is-active')
    if [ -e "$state" ]; then echo active; exit 0; else echo inactive; exit 4; fi ;;
  '--user daemon-reload') exit 0 ;;
  '--user enable') : > "$state"; exit 0 ;;
  *) exit 1 ;;
esac"#,
            log = dir.file("systemctl.log").display(),
            state = dir.file("unit-enabled").display(),
        ),
    )
}

/// `loginctl`, with the two knobs a run can meet: whether this account's linger is already on, and whether the
/// machine's polkit refuses to set it (a file, because a shim cannot be edited between two runs of one test).
fn loginctl_shim(dir: &TempDir) -> PathBuf {
    shim(
        dir,
        "loginctl",
        &format!(
            r#"log='{log}'
state='{state}'
printf 'loginctl %s\n' "$*" >> "$log"
case "$1" in
  show-user)
    if [ -e "$state" ]; then echo yes; else echo no; fi ;;
  enable-linger)
    if [ -e '{denied}' ]; then
      echo 'Interactive authentication required.' >&2
      exit 1
    fi
    : > "$state" ;;
  *) exit 1 ;;
esac"#,
            log = dir.file("loginctl.log").display(),
            state = dir.file("linger-on").display(),
            denied = dir.file("polkit-denies").display(),
        ),
    )
}

/// The programs an install runs, pointed at the shims above and at the tree's own unit.
///
/// `manager_unit` is the tree's unit because that is the arm this file measures — the manager is this run's to
/// command (ADR-0088). `a_tree_whose_unit_is_not_the_managers` is the other arm, and it builds its own tools.
fn tools(dir: &TempDir, root: &Root, account: (&str, &str)) -> install::Tools {
    install::Tools {
        deploy: deploy::Tools {
            doctor: doctor::Tools {
                preflight: preflight::Tools {
                    git: PathBuf::from("git"),
                    df: df_shim(dir),
                    du: PathBuf::from("du"),
                    openssl: PathBuf::from("openssl"),
                    tailscale: PathBuf::from("tailscale"),
                },
                tailscale: tailscale_shim(dir),
                systemd: dir.file("no-systemd-here"),
                ..doctor::Tools::default()
            },
            id: id_shim(dir, account.0, account.1),
            systemctl: systemctl_shim(dir),
            // Short because a test may not spend the minute a machine may; the deploy pass has its own, shorter.
            health_wait: Duration::from_millis(150),
            ..deploy::Tools::default()
        },
        loginctl: loginctl_shim(dir),
        manager_unit: root.unit(),
    }
}

/// The machine an install is asked about, with no daemon anywhere: a **hub**'s own answers.
///
/// Every trait is implemented because the seam is one parameter (`deploy::Machine`), and every answer is "nothing is
/// there" — which a hub's run never contradicts, because the four container checks are `unknown` for it before the
/// machine is asked (`doctor`'s `because` sentence).
struct NoDaemon;

impl proxies::Containers for NoDaemon {
    fn running(&self, _name: &str) -> bool {
        false
    }

    fn image_exists(&self, _image: &str) -> bool {
        false
    }

    fn network_names(&self, _filter: &str, _bound: Duration) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    fn ensure_network(&self, _name: &str) -> Result<(), String> {
        Err("this machine has no daemon".to_string())
    }

    fn replace(&self, _name: &str, _args: &[String], _bound: Duration) -> Result<(), String> {
        Err("this machine has no daemon".to_string())
    }
}

impl doctor::Machine for NoDaemon {
    fn running(&self, _name: &str) -> bool {
        false
    }

    fn image_pi(&self, _image: &str) -> Result<String, String> {
        Err("this machine has no daemon".to_string())
    }

    fn image_labels(&self, _image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        Ok(None)
    }

    fn selftest(&self, _names: &selftest::Names, _image: &str) -> selftest::Outcome {
        selftest::Outcome::Unknown("this machine has no daemon".to_string())
    }
}

impl images::Images for NoDaemon {
    fn daemon_arch(&self) -> Result<String, String> {
        Err("this machine has no daemon".to_string())
    }

    fn labels(&self, _image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        Ok(None)
    }

    fn build(&self, _tag: &str, _archive: &[u8], _args: &[String]) -> Result<images::Log, String> {
        Err("this machine has no daemon".to_string())
    }
}

/// The body `/api/health` answers: the six keys the reader parses, because a `200` with anything else is "a port
/// somebody else holds" rather than "this machine serves".
const HEALTH_PAYLOAD: &str = r#"{"name":"holosmith","version":"verify","role":"hub","uptime_s":0,"preview_origin":"http://localhost:8081","application_server_key":null}"#;

/// A control listener answering `GET /api/health` for the rest of the test's life, and its address.
///
/// `ready` is asked per request, which is how a case makes the endpoint start answering at a moment of its own
/// choosing: before it says so, the connection is accepted and closed unanswered, which is what the deploy's last
/// step meets on a first install — the port is held, so the attempt is a real one rather than a refused one, and the
/// thing that turns it into an answer has not happened yet.
fn health_responder(ready: impl Fn() -> bool + Send + 'static) -> SocketAddr {
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
            if ready() {
                let _ = stream.write_all(answer.as_bytes());
            }
        }
    });
    listen
}

/// An address nothing answers on: a port the kernel just gave back.
fn quiet_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port is bindable");
    listener.local_addr().expect("the bound address is known")
}

/// The machine's config: a hub whose control origin is `listen`, and whose preview port is the kernel's.
fn hub_config(listen: SocketAddr) -> Config {
    Config {
        role: Role::Hub,
        control_listen: listen,
        preview_listen: quiet_address(),
        control_origin: Origin::parse(&format!("http://{listen}"))
            .expect("an origin for the listener"),
        ..Config::default()
    }
}

/// The same machine as a server, with a daemon reached at `docker_host`.
fn server_config(docker_host: String) -> Config {
    Config {
        role: Role::Server,
        docker_host: Some(docker_host),
        control_listen: quiet_address(),
        preview_listen: quiet_address(),
        ..Config::default()
    }
}

/// A root under `dir`, made the way an operator makes it: 0700, owned by the account running these tests, with the
/// config beside it (ADR-0088) — which is a directory the deploy writes into, not one it makes.
fn rooted(dir: &TempDir, label: &str) -> Root {
    let root = Root::resolve(Some(dir.file(label)), Some(dir.file("holosmith.toml")));
    std::fs::create_dir_all(root.path()).expect("the root is creatable");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the root's mode is settable");
    root
}

/// The `config` from [`Root::config`], which is what every step sees.
fn deployed(root: &Root, config: &Config) -> Config {
    root.config(config)
}

/// One of install's own lines, or a panic naming the whole report.
fn line(report: &install::Report, what: What) -> &install::Line {
    report
        .lines
        .iter()
        .find(|line| line.what == what)
        .unwrap_or_else(|| panic!("no {what} line in:\n{}", report.render()))
}

// ---- the refusal -----------------------------------------------------------------------------

#[test]
fn an_account_outside_the_docker_group_is_refused_and_nothing_is_written() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-docker-group");
    let config = server_config(format!("unix://{}", dir.file("docker.sock").display()));
    // The root is **not** made: the acceptance's "zero side effects" is a claim about a tree that does not exist yet.
    let root = Root::resolve(Some(dir.file("root")), Some(dir.file("holosmith.toml")));
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator nogroup"));

    let refusal = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut Silent)
        .expect_err("a machine whose account cannot open the daemon's socket must be refused");

    // The one command, exactly, as the last line — it is the thing to paste.
    assert_eq!(
        refusal.lines().last(),
        Some("sudo usermod -aG docker $USER"),
        "the refusal does not end with the one command:\n{refusal}"
    );
    // And why: the socket's owner and mode, and that a group reaches a login only at the next one.
    assert!(
        refusal.contains("root:docker") && refusal.contains("0660"),
        "the refusal does not say what the socket is:\n{refusal}"
    );
    assert!(
        refusal.contains("only at the next one"),
        "the refusal does not say a group needs a new login:\n{refusal}"
    );
    assert!(
        refusal.contains("nothing has been written"),
        "the refusal does not say the machine was left alone:\n{refusal}"
    );
    // Zero side effects: no tree, no config, no manager.
    assert!(
        !root.path().exists(),
        "a refused install wrote {}",
        root.path().display()
    );
    assert!(
        !root.config_file().exists(),
        "a refused install wrote {}",
        root.config_file().display()
    );
    for program in ["systemctl", "loginctl"] {
        assert!(
            !dir.file(&format!("{program}.log")).exists(),
            "a refused install ran {program}"
        );
    }
}

/// The same machine, asked for a plan: ADR-0091 D1's "a dry run is refused too".
///
/// The plan exists to be acted on, and a plan for a machine that this run's own first line refuses is a lie about what
/// the next run does — so `--dry-run` does not get as far as a step, let alone a write.
#[test]
fn a_dry_run_is_refused_too() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-docker-group-plan");
    let config = server_config(format!("unix://{}", dir.file("docker.sock").display()));
    let root = Root::resolve(Some(dir.file("root")), Some(dir.file("holosmith.toml")));
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator nogroup"));

    let refusal = install::execute(&deployed, &root, &tools, &NoDaemon, true, &mut Silent)
        .expect_err("a plan for a machine this account cannot serve is refused as the apply is");

    assert_eq!(
        refusal.lines().last(),
        Some("sudo usermod -aG docker $USER"),
        "a refused plan does not end with the one command:\n{refusal}"
    );
    // A plan writes nothing either, and it does not read the manager to describe one.
    assert!(
        !root.config_file().exists(),
        "a refused plan wrote {}",
        root.config_file().display()
    );
    for program in ["systemctl", "loginctl"] {
        assert!(
            !dir.file(&format!("{program}.log")).exists(),
            "a refused plan ran {program}"
        );
    }
}

// ---- the two renderings and the progress lines ----------------------------------------------

#[test]
fn the_terse_report_is_the_deploys_marked_steps_then_installs_own_unnumbered_ones() {
    // The shape an operator reads by default (ADR-0085): the deploy's six steps by number, then install's own three
    // lines unnumbered — they are not the spec's steps, and the verbose report says the same with `install:` where a
    // number would be. A `changed` line of install's own is its confirmation and prints the mark and nothing else; a
    // `needs` or a `failed` one keeps what the operator has to run.
    let report = install::Report {
        deploy: deploy::Report {
            lines: vec![
                deploy::Line {
                    step: DeployStep::Preflight,
                    word: Word::Satisfied,
                    sentence: "10 findings: 8 pass, 0 fail, 2 unknown (systemd, disk)".to_string(),
                    fix: None,
                },
                deploy::Line {
                    step: DeployStep::Files,
                    word: Word::Changed,
                    sentence: "2 actions:".to_string(),
                    fix: None,
                },
            ],
            failed: None,
            dry_run: false,
            root: "/home/operator/.local/share/holosmith".to_string(),
        },
        lines: vec![
            install::Line {
                what: What::Unit,
                word: Word::Changed,
                sentence: "`systemctl --user daemon-reload` and `systemctl --user enable --now \
                           holosmith.service` ran: the manager has loaded the unit this run wrote and started it"
                    .to_string(),
                fix: None,
            },
            install::Line {
                what: What::Linger,
                word: Word::Satisfied,
                sentence: "`loginctl show-user --property=Linger` answers `yes`, so the daemon starts at boot"
                    .to_string(),
                fix: None,
            },
            install::Line {
                what: What::Health,
                word: Word::Failed,
                sentence: "the control origin (127.0.0.1:8080) did not answer `/api/health` within 60 s"
                    .to_string(),
                fix: None,
            },
        ],
        url: "http://localhost:8080".to_string(),
        next: "holosmith key set <provider>".to_string(),
    };
    assert_eq!(
        report.render(),
        "1. preflight ✓\n\
         2. files ✓ (2 actions)\n\
         unit ✓\n\
         linger ✓\n\
         health ✗ the control origin (127.0.0.1:8080) did not answer `/api/health` within 60 s\n"
    );
    // The verbose rendering is the deploy's block as every earlier release printed it, then install's three lines
    // with the verb where the step's number would be.
    let verbose = report.render_verbose();
    assert!(
        verbose.starts_with("deploy: run\ndeploy: into /home/operator/.local/share/holosmith\n")
    );
    assert!(verbose.contains("\n\ninstall: unit: changed: `systemctl --user daemon-reload`"));
    assert!(verbose.contains("\ninstall: health: failed: "));
}

#[test]
fn a_failed_line_of_installs_own_gets_the_runs_resume_command_and_the_tolerated_health_line_does_not()
 {
    // The deploy's health line may be `failed` on a run that succeeded — on a first install it answers "not serving
    // yet", which is the very next thing this command does (ADR-0091 D2) — so a `fix:` there would offer a command
    // for a problem the operator does not have.
    let mut report = install::Report {
        deploy: deploy::Report {
            lines: vec![deploy::Line {
                step: DeployStep::Health,
                word: Word::Failed,
                sentence:
                    "the control origin (127.0.0.1:8080) did not answer `/api/health` within 1 s"
                        .to_string(),
                fix: None,
            }],
            failed: Some(DeployStep::Health),
            dry_run: false,
            root: "/home/operator/.local/share/holosmith".to_string(),
        },
        lines: Vec::new(),
        url: "http://localhost:8080".to_string(),
        next: "holosmith key set <provider>".to_string(),
    };
    report.fill_fix("holosmith install --config /home/operator/.config/holosmith.toml");
    assert_eq!(
        report.render(),
        "6. health ✗ the control origin (127.0.0.1:8080) did not answer `/api/health` within 1 s\n",
        "the tolerated health line was given a fix it must not have"
    );

    // The line that does decide the run gets it, and it is the one command that re-runs what failed.
    let mut failed = report.clone();
    failed.lines.push(install::Line {
        what: What::Health,
        word: Word::Failed,
        sentence: "the control origin (127.0.0.1:8080) did not answer `/api/health` within 60 s"
            .to_string(),
        fix: None,
    });
    failed.fill_fix("holosmith install --config /home/operator/.config/holosmith.toml");
    assert!(
        failed.render().ends_with(
            "    fix: holosmith install --config /home/operator/.config/holosmith.toml\n"
        ),
        "the failed install line carries no resume command:\n{}",
        failed.render()
    );
}

/// What an install told a sink, in the order it told it.
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

#[test]
fn an_install_tells_its_sink_every_step_it_evaluates_and_names_the_waits_bound() {
    // The two silences an install has are the deploy's images and the wait after `enable --now`; the sink is what a
    // reader gets instead of them (ADR-0092). One begin per evaluated step, in the order the run does them, the two a
    // hub skips silent, and a health line that says how long it may wait and where.
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-progress");
    let listen = health_responder(|| true);
    let config = hub_config(listen);
    let root = rooted(&dir, "root");
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator docker"));
    let mut sink = Recorded::default();

    let report = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut sink)
        .expect("a readable account is not a refusal");
    assert!(!report.failed(), "the run failed:\n{}", report.render());

    assert_eq!(
        sink.0,
        [
            "begin [1/6 preflight] asking `doctor --quick` about this machine".to_string(),
            "begin [2/6 files] reading and writing the install tree, the config and the secrets"
                .to_string(),
            "begin [5/6 service] writing the systemd unit".to_string(),
            "begin [6/6 health] asking `/api/health` and `doctor --full` about the machine"
                .to_string(),
            "begin [unit] running this account's two `systemctl --user` commands for the unit"
                .to_string(),
            "begin [linger] setting this account's linger flag, so the daemon starts at boot"
                .to_string(),
            format!("begin [health] waiting for `/api/health` on {listen} for up to 150 ms"),
        ],
        "the run did not announce each step it evaluated, in order"
    );
}

#[test]
fn a_plan_tells_its_sink_the_same_seven_steps_a_real_run_does() {
    // `execute` announces every step it evaluates whether the run is a plan or an apply, and the **silence** of a plan
    // is `commands::install::run`'s choice of sink — `Silent` for `--dry-run`, `Announce` otherwise — which no test
    // here can reach without capturing a process's stderr. So what is measured at this layer is the other half: the
    // plan and the run it describes hand a sink the same seven lines, and the silence itself is `scripts/verify.sh`'s
    // `case_install`/`case_install_terse` `dry/stderr`, which must be zero bytes on the artifact.
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-progress-dry");
    let listen = health_responder(|| true);
    let config = hub_config(listen);
    let root = rooted(&dir, "root");
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator docker"));

    let mut planned = Recorded::default();
    let plan = install::execute(&deployed, &root, &tools, &NoDaemon, true, &mut planned)
        .expect("a plan is not a refusal");
    assert!(!plan.failed(), "the plan failed:\n{}", plan.render());

    let mut ran = Recorded::default();
    let real = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut ran)
        .expect("a readable account is not a refusal");
    assert!(!real.failed(), "the run failed:\n{}", real.render());

    assert_eq!(
        planned.0.len(),
        7,
        "a plan does not announce every step it evaluates: {:?}",
        planned.0
    );
    assert_eq!(
        planned.0, ran.0,
        "a plan and the run it describes announce different steps"
    );
}

// ---- linger ---------------------------------------------------------------------------------

#[test]
fn a_refused_linger_is_reported_and_the_run_still_succeeds() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-linger");
    let listen = health_responder(|| true);
    let config = hub_config(listen);
    let root = rooted(&dir, "root");
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator docker"));
    // The machine whose polkit will not let this account set its own linger flag.
    dir.write("polkit-denies", "");

    let report = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut Silent)
        .expect("a readable account is not a refusal");

    assert!(
        !report.failed(),
        "a refused linger must not fail the install:\n{}",
        report.render()
    );
    let linger = line(&report, What::Linger);
    assert_eq!(
        linger.word,
        Word::Needs,
        "a refused linger is the operator's remaining command: {linger}"
    );
    assert!(
        linger
            .sentence
            .contains("sudo loginctl enable-linger $USER"),
        "the refused linger does not print the command:\n{}",
        linger.sentence
    );
    assert!(
        linger
            .sentence
            .contains("Interactive authentication required."),
        "the refused linger does not say what loginctl answered:\n{}",
        linger.sentence
    );
    // It carried on: the unit was enabled and the machine answered afterwards.
    let unit = line(&report, What::Unit);
    assert_eq!(unit.word, Word::Changed, "the unit was not enabled: {unit}");
    assert_eq!(
        line(&report, What::Health).word,
        Word::Satisfied,
        "the run did not reach the wait that ends it:\n{}",
        report.render()
    );
    assert!(
        report
            .lines
            .iter()
            .map(|line| line.what)
            .eq([What::Unit, What::Linger, What::Health]),
        "install's own steps are not unit, linger, health"
    );
    assert_eq!(report.url, config.control_origin.as_str());
    assert_eq!(report.next, "holosmith hub host add <name> <url>");
}

// ---- a first install, and the deploy health line that is not its failure --------------------

#[test]
fn a_first_install_whose_deploy_health_line_failed_still_succeeds() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-first-run");
    // The endpoint is bound from the start and answers from the moment the manager has the unit, which is a first
    // install exactly: the deploy's step 6 asks **before** `enable --now` has run, so its honest answer is no, and
    // install's own wait asks after it. The unit's state file is what the shim's `--user enable` writes, so the
    // moment is the real one rather than a marker this test invented.
    let state = dir.file("unit-enabled");
    let listen = health_responder(move || state.exists());
    let config = hub_config(listen);
    let root = rooted(&dir, "root");
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator docker"));

    let report = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut Silent)
        .expect("a readable account is not a refusal");

    // The deploy's step 6 did fail — that is the premise, not a detail — and its line and tally print as they stand.
    assert_eq!(
        report.deploy.failed,
        Some(DeployStep::Health),
        "the deploy pass answered before the unit was started:\n{}",
        report.render()
    );
    assert!(
        report.render_verbose().contains("1 failed"),
        "the deploy's tally lost the failure it is entitled to:\n{}",
        report.render()
    );
    // And it is **not** the verdict: install's own wait, after `enable --now`, is (ADR-0091 D2).
    assert!(
        !report.failed(),
        "a fresh install whose deploy health line failed must still succeed — otherwise it exits 1 and never \
         prints the URL it just earned:\n{}",
        report.render()
    );
    assert_eq!(
        line(&report, What::Health).word,
        Word::Satisfied,
        "the run did not reach the wait that ends it:\n{}",
        report.render()
    );
    assert_eq!(line(&report, What::Unit).word, Word::Changed);
    // The two lines `run` prints on this path, which are why the exit code matters.
    assert_eq!(report.url, config.control_origin.as_str());
    assert_eq!(report.next, "holosmith hub host add <name> <url>");
}

// ---- a second run ---------------------------------------------------------------------------

#[test]
fn a_second_run_changes_nothing_and_says_so() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-second-run");
    let listen = health_responder(|| true);
    let config = hub_config(listen);
    let root = rooted(&dir, "root");
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator docker"));

    let first = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut Silent)
        .expect("a readable account is not a refusal");
    assert!(!first.failed(), "the first run failed:\n{}", first.render());
    // The first run is where the three applies happen, one each.
    assert_eq!(
        applied(&diary(&dir.file("systemctl.log"))),
        2,
        "the first run did not enable the unit:\n{:?}",
        diary(&dir.file("systemctl.log"))
    );
    assert_eq!(
        applied(&diary(&dir.file("loginctl.log"))),
        1,
        "the first run did not set linger:\n{:?}",
        diary(&dir.file("loginctl.log"))
    );

    let systemctl_before = diary(&dir.file("systemctl.log"));
    let loginctl_before = diary(&dir.file("loginctl.log"));
    let second = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut Silent)
        .expect("a readable account is not a refusal");
    assert!(
        !second.failed(),
        "the second run failed:\n{}",
        second.render()
    );
    for what in [What::Unit, What::Linger, What::Health] {
        assert_eq!(
            line(&second, what).word,
            Word::Satisfied,
            "the second run's {what} line is not `satisfied`: {}",
            line(&second, what).sentence
        );
    }
    // Nothing was applied a second time, and the reads saw the state the first run left.
    assert_eq!(
        applied(&diary(&dir.file("systemctl.log"))[systemctl_before.len()..]),
        0,
        "the second run ran a systemctl command that changes something:\n{:?}",
        diary(&dir.file("systemctl.log"))
    );
    assert_eq!(
        applied(&diary(&dir.file("loginctl.log"))[loginctl_before.len()..]),
        0,
        "the second run ran a loginctl command that changes something:\n{:?}",
        diary(&dir.file("loginctl.log"))
    );
}

// ---- a re-run that rewrote the unit's bytes -------------------------------------------------

#[test]
fn a_rerun_that_rewrote_the_unit_leaves_the_restart_to_the_operator() {
    // The one arrangement in which `enable --now` is not the whole story, and the one line that must not lose it: the
    // manager is already running the unit and this run has just put different bytes on disk, so the process serving
    // predates them. The word is `needs` and not `changed`, because `changed` is install's own confirmation and stops
    // at the mark in the terse report — and this line carries the command that starts the bytes (ADR-0085, ADR-0091).
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-unit-rewritten");
    let listen = health_responder(|| true);
    let config = hub_config(listen);
    let root = rooted(&dir, "root");
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator docker"));

    let first = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut Silent)
        .expect("a readable account is not a refusal");
    assert!(!first.failed(), "the first run failed:\n{}", first.render());
    assert_eq!(
        line(&first, What::Unit).word,
        Word::Changed,
        "the first run did not enable the unit: {}",
        line(&first, What::Unit).sentence
    );
    assert!(
        dir.file("unit-enabled").exists(),
        "the manager was left without the unit, so this is not the already-running arm"
    );

    // The bytes the manager loaded are no longer the bytes on disk, which is what a reinstall over a new release is.
    std::fs::write(root.unit(), "stale\n").expect("the unit file is writable");
    let before = diary(&dir.file("systemctl.log")).len();

    let second = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut Silent)
        .expect("a readable account is not a refusal");

    // The premise: the deploy pass did put new bytes on disk, so the manager is holding the older copy.
    let service = second
        .deploy
        .lines
        .iter()
        .find(|line| line.step == DeployStep::Service)
        .unwrap_or_else(|| panic!("the deploy reported no service step:\n{}", second.render()));
    assert_eq!(
        service.word,
        Word::Changed,
        "the run did not rewrite the unit:\n{}",
        second.render()
    );
    // Two applies, because the manager's copy is stale: the reload and the enable, the same as a first install.
    assert_eq!(
        applied(&diary(&dir.file("systemctl.log"))[before..]),
        2,
        "the run did not reload and enable the unit it wrote:\n{:?}",
        diary(&dir.file("systemctl.log"))
    );

    // The word, the command the terse line must keep, and the exit class: the three things this line owes.
    let unit = line(&second, What::Unit);
    assert_eq!(
        unit.word,
        Word::Needs,
        "a restart this run left to the operator is not `changed`: {unit}"
    );
    let terse = second
        .render()
        .lines()
        .find(|line| line.starts_with("unit "))
        .unwrap_or_else(|| panic!("the terse report has no unit line:\n{}", second.render()))
        .to_string();
    assert!(
        terse.contains("systemctl --user restart holosmith.service"),
        "the terse line lost the command the operator must run: {terse}"
    );
    assert!(
        terse.contains("the process serving predates these bytes"),
        "the terse line lost the reason it is the operator's: {terse}"
    );
    // And it is not a failure: `run` prints the URL and the next step, and returns success, exactly when the report's
    // own predicate is false (`install::Report::failed`) — so this is the success path's evidence.
    assert!(
        !second.failed(),
        "a line that only waits on a restart must not fail the run:\n{}",
        second.render()
    );
    assert_eq!(second.url, config.control_origin.as_str());
    assert_eq!(second.next, "holosmith hub host add <name> <url>");
}

// ---- the wait that is the verdict -----------------------------------------------------------

#[test]
fn a_health_poll_that_never_answers_fails_and_names_the_journal() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-unanswered");
    let config = hub_config(quiet_address());
    let root = rooted(&dir, "root");
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator docker"));

    let report = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut Silent)
        .expect("a readable account is not a refusal");

    assert!(report.failed(), "a machine that never answers passed");
    // The deploy's own last step failed first — nothing serves before the unit is started — and the run carried on
    // rather than stopping there, which is what makes the line below this command's verdict rather than a repeat.
    assert_eq!(
        report.deploy.failed,
        Some(DeployStep::Health),
        "the deploy pass did not reach its own health step:\n{}",
        report.render()
    );
    assert!(
        report.lines.len() == 3,
        "the run did not reach install's own health line:\n{}",
        report.render()
    );
    let health = line(&report, What::Health);
    assert_eq!(health.word, Word::Failed);
    assert!(
        health
            .sentence
            .contains("journalctl --user -u holosmith.service"),
        "the failed wait does not name the journal:\n{}",
        health.sentence
    );
    // The unit and the linger were done, and the failure is the last line: later steps are not claimed.
    assert_eq!(line(&report, What::Unit).word, Word::Changed);
    assert_eq!(
        report.lines.last().map(|line| line.what),
        Some(What::Health)
    );
}

// ---- the step that is not health -------------------------------------------------------------

/// A deploy step that failed anywhere but health stops the run, and install's own lines are empty.
///
/// ADR-0091 D2's first consequence: the tree, the images and the proxies are what the two manager commands and the
/// wait stand on, so a report that failed earlier has not earned them and install owns no step. What the operator gets
/// is the deploy's own block — its `files` line and the resume-shaped line `run` prints for this verb.
///
/// The failure here is step 2's, and it is the one thing that step cannot work around: the key store is a file
/// `serve` and the key proxy read, so a directory sitting at that path is a tree to put right — the deploy never
/// writes over it.
#[test]
fn a_deploy_step_that_is_not_health_stops_the_run() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-files-failed");
    let listen = health_responder(|| true);
    let config = hub_config(listen);
    let root = rooted(&dir, "root");
    // The key store's path, occupied by a directory: step 1 reads no database there and passes, and step 2's read of
    // this item is the one `Err` it can have.
    std::fs::create_dir_all(root.data_root().join(key::SECRETS_DIR).join(key::KEYS_FILE))
        .expect("the key store's path is a directory");
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator docker"));

    let report = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut Silent)
        .expect("a failed step is a report, not a refusal");

    assert!(
        report.failed(),
        "a deploy that did not get past its files failed and the run passed:\n{}",
        report.render()
    );
    assert_eq!(
        report.deploy.failed,
        Some(DeployStep::Files),
        "the run did not stop at the step that failed:\n{}",
        report.render()
    );
    // None of install's own three steps was reached, in the report or on the machine.
    assert!(
        report.lines.is_empty(),
        "an install that stopped at the deploy still claimed a step of its own:\n{}",
        report.render()
    );
    for program in ["systemctl", "loginctl"] {
        assert!(
            !dir.file(&format!("{program}.log")).exists(),
            "an install that stopped at the deploy ran {program}"
        );
    }
}

// ---- the tree whose unit no manager reads ---------------------------------------------------

#[test]
fn a_tree_whose_unit_is_not_the_managers_enables_nothing() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-foreign-tree");
    let listen = health_responder(|| true);
    let config = hub_config(listen);
    let root = rooted(&dir, "root");
    let deployed = deployed(&root, &config);
    // The account's own unit, which this tree's is not: `systemctl --user` would resolve the *name* there.
    let tools = install::Tools {
        manager_unit: Root::resolve(None, None).unit(),
        ..tools(&dir, &root, ("operator", "operator docker"))
    };

    let report = install::execute(&deployed, &root, &tools, &NoDaemon, false, &mut Silent)
        .expect("a readable account is not a refusal");

    let unit = line(&report, What::Unit);
    assert_eq!(
        unit.word,
        Word::Needs,
        "a foreign tree's unit must not be claimed as enabled: {unit}"
    );
    assert!(
        unit.sentence
            .contains("systemctl --user enable --now holosmith.service"),
        "the line does not name the commands that are still the operator's:\n{}",
        unit.sentence
    );
    assert!(
        !diary(&dir.file("systemctl.log"))
            .iter()
            .any(|line| line.contains("daemon-reload") || line.contains(" enable ")),
        "a foreign tree's install commanded the account's own manager:\n{:?}",
        diary(&dir.file("systemctl.log"))
    );
    // And the boot flag too: linger is what makes the unit the manager loads a boot service, so on a tree it does not
    // load it would set one for a service this run was never asked about (ADR-0091 D4). The command is the
    // operator's, and `loginctl` is neither asked nor run.
    let linger = line(&report, What::Linger);
    assert_eq!(
        linger.word,
        Word::Needs,
        "a foreign tree's linger is the operator's remaining command: {linger}"
    );
    assert!(
        linger
            .sentence
            .contains("sudo loginctl enable-linger $USER"),
        "the linger line does not end with the command that is still the operator's:\n{}",
        linger.sentence
    );
    assert!(
        linger.sentence.contains("systemctl --user daemon-reload"),
        "the linger line does not name the two commands that come first:\n{}",
        linger.sentence
    );
    assert!(
        diary(&dir.file("loginctl.log")).is_empty(),
        "a foreign tree's install asked or commanded loginctl:\n{:?}",
        diary(&dir.file("loginctl.log"))
    );
}

// ---- the plan ------------------------------------------------------------------------------

#[test]
fn a_dry_run_prints_the_plan_and_writes_nothing() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("install-dry-run");
    let listen = health_responder(|| true);
    let config = hub_config(listen);
    let root = rooted(&dir, "root");
    let deployed = deployed(&root, &config);
    let tools = tools(&dir, &root, ("operator", "operator docker"));

    let report = install::execute(&deployed, &root, &tools, &NoDaemon, true, &mut Silent)
        .expect("a readable account is not a refusal");

    assert!(!report.failed(), "the plan failed:\n{}", report.render());
    for what in [What::Unit, What::Linger, What::Health] {
        assert_eq!(
            line(&report, what).word,
            Word::Needs,
            "a dry run's {what} is not a plan: {}",
            line(&report, what).sentence
        );
    }
    // No write of any kind: not the tree, not the manager, not linger.
    assert!(
        !root.config_file().exists(),
        "a dry run wrote {}",
        root.config_file().display()
    );
    for program in ["systemctl", "loginctl"] {
        assert_eq!(
            applied(&diary(&dir.file(&format!("{program}.log")))),
            0,
            "a dry run ran {program} with a changing command"
        );
    }
}
