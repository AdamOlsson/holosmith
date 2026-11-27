//! A promotion's launcher: one transient user unit, started from inside `serve`.
//!
//! A promotion started from a request cannot be a child of the process it restarts. [`crate::deploy::unit_text`]
//! renders `KillMode=mixed` with `TimeoutStopSec=30s`, so the `systemctl --user restart` a promotion performs sends
//! `SIGKILL` to every process in `holosmith.service`'s cgroup as soon as the main one exits — and the promotion's
//! whole promise lives *after* that restart: the twenty-second `/api/health` poll ([`crate::promote::WINDOW`]) and
//! the switch back to the previous release when the candidate does not answer. So the job is started as a
//! **transient user unit** of the operator's own user manager (ADR-0110 D4): a unit of its own is outside the
//! service's cgroup, and it outlives the restart it performs.
//!
//! One start is
//!
//! ```text
//! systemd-run --user --unit=holosmith-update --collect \
//!   --property=StandardOutput=append:<data>/update.log \
//!   --property=StandardError=append:<data>/update.log \
//!   --property=WorkingDirectory=<source tree> \
//!   --property=UMask=0077 \
//!   -- <program> <argument>...
//! ```
//!
//! and each part is a fact rather than a preference:
//!
//! - **The output is a file, appended to, under the data root** ([`LOG`]). A pipe would die with `serve`, and the log
//!   is the whole feedback a run has (ADR-0110 D2). Both streams go to it, because everything the job writes down —
//!   the gate's `FAIL` lines on stdout and the sentence a program that could not be run leaves on stderr — is what an
//!   operator reads after the restart. `append:` is what lets a second run's output sit beside the first's rather
//!   than replace it.
//! - **The log's mode is the machine's, not the manager's.** `UMask=0077` is the property [`crate::deploy::unit_text`]
//!   already renders for the service, so the job's file is created the way every other file the machine writes under the
//!   data root is: `0666 & ~0077 = 0600`. Left to itself the job inherits the **user manager's** default umask — `0002`
//!   on the machine this was measured on — and the file a promotion writes most of would be `-rw-rw-r--` while the rest
//!   of the data root is its operator's alone (ADR-0110's Fact 2 measured both modes). The property is a service
//!   property and not 240's spelling: [`FLOOR`] is the release that introduced `append:`'s spelling, and the job's
//!   `UMask=` adds no floor of its own.
//! - **`WorkingDirectory=` is the source tree**, so the job's own relative paths are that tree's and never depend on
//!   the directory `serve` was started in.
//! - **The unit's name is the lock.** systemd refuses a second unit under a live name, so two presses are one
//!   promotion and the second is answered [`Answer::AlreadyRunning`]; `--collect` is what makes the name usable again
//!   once the unit has ended — a failed one included — so the name is transient and not a reservation. "One at a
//!   time" is therefore systemd's answer and not a flag in `serve` that the restart the job performs would reset.
//!
//! # What it does not know
//!
//! **The program and its arguments are parameters** ([`Job`]). This module starts a command line and has no opinion
//! about what runs behind it, which is what makes it measurable with a stand-in; the verb, the candidate, the
//! free-space check and the route that answers "started, here is the log" are the update route's.
//!
//! **It blocks on two children.** Like [`crate::disk`], [`crate::trust`] and [`crate::snapshot`], the route that
//! starts a promotion runs [`start`] on `spawn_blocking`.
//!
//! **It refuses nothing at boot.** A LAN-only or hand-run `serve` that never promotes must start, so a machine with no
//! `systemd-run` is a refusal of *this attempt* and not of the machine — the whole of that refusal is [`start`]'s one
//! sentence, which says why such a machine cannot promote at all today. `doctor` reports no program for this for the
//! same reason it reports none for `systemctl`: what it reads as `systemd` is the unit **directory**, not a manager's
//! program, so there is nothing there for this to be reported beside.
//!
//! **One question is about the machine, and it is asked at the press.** The user manager is a version, and the two
//! `append:` properties above are [`FLOOR`]'s spelling (ADR-0110), so [`below_floor`] asks `systemctl --version` and
//! answers the one sentence a machine under it deserves. It is asked by the route and by both verbs **before** a press
//! starts anything, and never at boot: a systemd below the floor installs, boots and serves sessions like any other,
//! and only the job a promotion starts needs the two properties (ADR-0125). A version that could not be measured
//! refuses nothing — see [`below_floor`].

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::deploy::Tools;

/// The transient unit's name, which is also the lock and the name both children spell.
///
/// systemd appends `.service`, so this one word is what `--unit=` takes and what `systemctl is-active` is asked
/// about: one spelling for the unit, however it is addressed (ADR-0110 D4).
pub const UNIT_NAME: &str = "holosmith-update";

/// The file the job's output is appended to, under the data root: `<data>/update.log`.
///
/// A name of the machine's own, because the job is the machine's and not a session's. Nothing here rotates or prunes
/// it: retention is the backups' question (ADR-0082 D9), and the reader a later change adds is `std::fs` on this one
/// path ([`log_path`]).
pub const LOG: &str = "update.log";

/// The systemd release a promotion's job needs: `StandardOutput=append:` and `StandardError=append:` are 240's
/// spelling, quoted from systemd's own `NEWS` under `CHANGES WITH 240` (ADR-0110). The job's `UMask=` is not part of
/// this number: it is a service property the argv spells for the mode of the file those two streams name, so the floor
/// is `append:`'s spelling and the job's `UMask=` adds no floor of its own.
///
/// The floor is **the job's and not the machine's**, which is why it is a constant here beside the argv that needs it:
/// a 239 installs, boots and serves sessions like any other, and only a promotion's transient unit asks for the two
/// properties. `docs/requirements.md` states the same number in its dependency table, scoped to the promotion.
pub const FLOOR: u32 = 240;

/// What the job is: the tree it works in, and the command line to run there.
///
/// The command line is one slice with the program first, which is the shape `systemd-run`'s `--` takes and the shape
/// `execv` takes: the words after `--` are the job's own, and nothing in them is read as an option of the launcher.
#[derive(Debug, Clone, Copy)]
pub struct Job<'a> {
    /// The tree the job works in, as `WorkingDirectory=`: the candidate's source tree, so a relative path in the
    /// job's own argv means that tree and not the caller's directory.
    pub source: &'a Path,
    /// The program and its arguments, in argv order, the program first.
    pub command: &'a [OsString],
}

/// What became of one start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The unit was started: it is running now, and appending its output to `log`.
    Started { unit: &'static str, log: PathBuf },
    /// A unit under this name is running, and systemd refused this start because of it.
    ///
    /// This is a measurement and not a reading of systemd's prose: the start's non-zero exit is what raises the
    /// question, and `systemctl --user is-active --quiet` is what answers it ([`start`]). A route turns this into its
    /// `409 update_running`, with the name and the log of the job already in flight.
    AlreadyRunning { unit: &'static str, log: PathBuf },
}

/// The file a machine's job writes under its data root: `<data>/update.log`.
///
/// A function of the data root rather than of a [`crate::deploy::Root`], because the root a machine *serves* from is
/// its config's `data_root` — `[storage] data_dir` may move it — and that is the directory the API reads the log out
/// of after the restart.
pub fn log_path(data_root: &Path) -> PathBuf {
    data_root.join(LOG)
}

/// The user manager's version, asked of the one child that answers it without the bus.
///
/// `systemctl --version`'s first line is `systemd 255 (255.4-1ubuntu8.18)`, and the version is the field after the
/// program's name ([`major`]). No `--user` is passed and none must be: `--version` is answered by the client itself,
/// so it answers on a machine whose manager this process could not otherwise reach — a `serve` from cron, a container
/// — and the client and the manager it talks to are one package, which is what makes the binary's version the user
/// manager's version.
///
/// `None` is every way a version could not be measured: the program could not be run, it exited non-zero, or its
/// first line is not `systemd <number> …`. Nothing measured a version there, and [`below_floor`] refuses nothing on
/// it.
fn version(tools: &Tools) -> Option<u32> {
    let output = std::process::Command::new(&tools.systemctl)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    major(&String::from_utf8_lossy(&output.stdout))
}

/// The major version in `systemctl --version`'s first line: the field after the program's name, up to its first
/// character that is not a digit — so `systemd 255 (255.4-1ubuntu8.18)` and `systemd 255.4` are both 255.
fn major(text: &str) -> Option<u32> {
    let mut words = text.lines().next()?.split_whitespace();
    if words.next()? != "systemd" {
        return None;
    }
    words
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// `None` when this machine's systemd meets [`FLOOR`], and the one sentence when it measured a version below it.
///
/// **A version this build could not measure refuses nothing.** A `systemctl` that could not be run, one that answered
/// non-zero and a line with no version in it are three ways of not knowing, and a press refused on a fact nobody has
/// would be "we could not measure" read as "we measured" (ADR-0044): so they answer `None`, and the press goes on to
/// what the next step says about the machine.
///
/// The sentence names the version it found, the floor, and the half of the machine that is fine: everything but the
/// job works, so the move is the distribution's systemd rather than anything this tree could supply.
pub fn below_floor(tools: &Tools) -> Option<String> {
    let found = version(tools)?;
    (found < FLOOR).then(|| {
        format!(
            "this machine's systemd is version {found}, and a promotion's job needs {FLOOR} or newer: the job's two \
             output properties are `StandardOutput=append:` and `StandardError=append:`, which a {FLOOR} has and this \
             {found} refuses, while everything else on this machine — the install, the boot and the sessions — works. \
             Nothing in this tree can supply a newer systemd: take the one your distribution packages, {FLOOR} or \
             above, and start the promotion again"
        )
    })
}

/// Start [`Job`] as the transient user unit, and say what became of it.
///
/// `Ok(Started)` is one start. `Ok(AlreadyRunning)` is systemd's answer that a unit under this name is live, which is
/// asked of the manager **after** a refused start (`systemctl --user is-active --quiet`) rather than read out of
/// systemd's sentence: `already exists` is prose, and a route's `409` has to be a measurement. `Err` is one sentence
/// naming the child that failed and what that means.
///
/// A machine with no `systemd-run` is that `Err`, in these words: it cannot promote at all today, because the
/// pipeline's restart is `systemctl --user restart` ([`crate::promote`]) and a promotion started as a child of the
/// service that restart stops would be killed before its health poll. That is the attempt's refusal and not the
/// boot's — `serve` asks nothing about it.
pub fn start(tools: &Tools, data_root: &Path, job: &Job<'_>) -> Result<Answer, String> {
    let log = log_path(data_root);
    let argv = argv(&log, job);
    let output = std::process::Command::new(&tools.systemd_run)
        .args(&argv)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| {
            format!(
                "`{} {}` could not be run ({error}): a promotion is a transient unit of the operator's own user \
                 manager, and a machine without `systemd-run` cannot promote at all today — the pipeline's restart \
                 is `systemctl --user restart`, and a promotion started as a child of the service that restart \
                 stops would be killed before its health poll",
                tools.systemd_run.display(),
                spelled(&argv),
            )
        })?;
    if output.status.success() {
        return Ok(Answer::Started {
            unit: UNIT_NAME,
            log,
        });
    }
    // Not success is not by itself "already running": systemd says many things with a non-zero exit. The question is
    // put to the manager, and only a unit it reports live is the answer a route may call a conflict.
    let active = std::process::Command::new(&tools.systemctl)
        .args(["--user", "is-active", "--quiet", UNIT_NAME])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| {
            format!(
                "`{} --user is-active --quiet {}` could not be run ({error}), so this machine cannot say whether a \
                 promotion is already in flight — `{}` answered {}",
                tools.systemctl.display(),
                UNIT_NAME,
                tools.systemd_run.display(),
                output.status.code().unwrap_or(-1),
            )
        })?;
    if active.status.success() {
        return Ok(Answer::AlreadyRunning {
            unit: UNIT_NAME,
            log,
        });
    }
    // Only two of the manager's answers are about the unit: 3 is `inactive` and 4 is `unknown`, and either one with
    // nothing on stderr says the unit is not running. Every other non-zero exit — `Failed to connect to bus: …`, exit
    // 1, which is what a `serve` started from cron or a container gets — is the manager saying it could not answer,
    // and a sentence built on that must not assert anything about a unit nothing measured.
    let manager_said = String::from_utf8_lossy(&active.stderr);
    let manager_line = manager_said
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string());
    if !(matches!(active.status.code(), Some(3 | 4)) && manager_line.is_none()) {
        return Err(format!(
            "`{} --user is-active --quiet {}` answered {} ({}), so this machine cannot say whether a promotion is \
             already in flight — `{}` answered {}",
            tools.systemctl.display(),
            UNIT_NAME,
            active.status.code().unwrap_or(-1),
            manager_line.as_deref().unwrap_or("no output"),
            tools.systemd_run.display(),
            output.status.code().unwrap_or(-1),
        ));
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let line = said
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no output")
        .trim();
    Err(format!(
        "`{} {}` answered {} ({line}) and `{} --user is-active --quiet {}` does not find that unit running: a \
         promotion starts a transient unit of the operator's own user manager, and this one did not start",
        tools.systemd_run.display(),
        spelled(&argv),
        output.status.code().unwrap_or(-1),
        tools.systemctl.display(),
        UNIT_NAME,
    ))
}

/// The `systemd-run` argv for one job: the unit, the output file, the tree, the mode that file is created under, and
/// then the command line after `--`.
///
/// `systemd-run` reads the properties in any order; this one is last because `case_web_update`'s tail match expects it
/// between `WorkingDirectory=` and `--`.
///
/// A function of its own so the argv the launcher runs and the argv a test asserts are one expression. `--` is the
/// boundary that keeps the job's words out of `systemd-run`'s: everything after it belongs to the job, whatever it
/// starts with.
fn argv(log: &Path, job: &Job<'_>) -> Vec<OsString> {
    let property = |name: &str, value: &OsStr| {
        let mut written = OsString::from(format!("--property={name}="));
        written.push(value);
        written
    };
    let mut append: OsString = OsString::from("append:");
    append.push(log.as_os_str());
    let mut argv = vec![
        OsString::from("--user"),
        OsString::from(format!("--unit={UNIT_NAME}")),
        OsString::from("--collect"),
        property("StandardOutput", &append),
        property("StandardError", &append),
        property("WorkingDirectory", job.source.as_os_str()),
        property("UMask", OsStr::new("0077")),
        OsString::from("--"),
    ];
    argv.extend(job.command.iter().cloned());
    argv
}

/// One argv as a sentence spells it: the words separated by spaces, a word that is not UTF-8 made lossy rather than
/// dropped, because a path this build did not write is still worth naming.
fn spelled(argv: &[OsString]) -> String {
    argv.iter()
        .map(|word| word.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::{Answer, FLOOR, Job, LOG, UNIT_NAME, below_floor, log_path, major, start};
    use crate::deploy;

    /// A directory of this test's own, emptied first: the stand-ins and the argv each one records go here, and no
    /// two tests share it.
    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("holosmith-update-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory is creatable");
        dir
    }

    /// A stand-in program at `dir/<name>`, as an executable script, and the file it writes its argv to.
    ///
    /// An executable rather than a recorder in process, because the question is what the **child** is given: every
    /// word here is argv, and the one bug it can have is being spelled nowhere but in a printed line.
    fn stand_in(dir: &Path, name: &str, status: i32, stderr: &str) -> (PathBuf, PathBuf) {
        answering(dir, name, status, "", stderr)
    }

    /// The same stand-in, with one line on its standard output: what a manager asked `--version` prints.
    fn answering(
        dir: &Path,
        name: &str,
        status: i32,
        stdout: &str,
        stderr: &str,
    ) -> (PathBuf, PathBuf) {
        let log = dir.join(format!("{name}.argv"));
        let program = dir.join(name);
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\n{{ printf '%s\\n' \"$@\"; }} > \"{}\"\n{}{}exit {status}\n",
                log.display(),
                if stdout.is_empty() {
                    String::new()
                } else {
                    format!("printf '%s\\n' \"{stdout}\"\n")
                },
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!("printf '%s\\n' \"{stderr}\" >&2\n")
                }
            ),
        )
        .expect("the stand-in is writable");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("the stand-in is executable");
        (program, log)
    }

    /// The argv a stand-in recorded, one word per line.
    fn recorded(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .expect("the stand-in recorded its argv")
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The program and its arguments, as the launcher takes them.
    fn command(program: &Path) -> Vec<OsString> {
        vec![
            program.as_os_str().to_owned(),
            OsString::from("run"),
            OsString::from("--flag"),
        ]
    }

    /// One start is one transient unit: the argv names the unit, both output properties append to the log under the
    /// data root, `UMask=0077` is what the manager creates that file under, `WorkingDirectory=` is the source tree, and
    /// the job's own words follow `--` untouched.
    #[test]
    fn one_start_is_a_transient_unit_writing_to_the_data_roots_log() {
        let dir = scratch("started");
        let (runner, ran) = stand_in(&dir, "systemd-run", 0, "");
        // A stand-in that would answer "active" if it were asked: a start that succeeded must not ask, and the empty
        // record is the assertion.
        let (manager, asked) = stand_in(&dir, "systemctl", 0, "");
        let tools = deploy::Tools {
            systemd_run: runner,
            systemctl: manager,
            ..deploy::Tools::default()
        };
        let data_root = dir.join("data");
        let source = dir.join("source");
        let program = dir.join("gate");
        let words = command(&program);

        let answer = start(
            &tools,
            &data_root,
            &Job {
                source: &source,
                command: &words,
            },
        )
        .expect("a start the user manager accepted is a start");
        let log = log_path(&data_root);
        assert_eq!(
            answer,
            Answer::Started {
                unit: UNIT_NAME,
                log: log.clone()
            },
            "the start did not name the unit and the log under the data root"
        );
        assert_eq!(
            log,
            data_root.join(LOG),
            "the log is not the constant's name under the data root"
        );
        assert_eq!(
            recorded(&ran),
            vec![
                "--user".to_string(),
                format!("--unit={UNIT_NAME}"),
                "--collect".to_string(),
                format!("--property=StandardOutput=append:{}", log.display()),
                format!("--property=StandardError=append:{}", log.display()),
                format!("--property=WorkingDirectory={}", source.display()),
                "--property=UMask=0077".to_string(),
                "--".to_string(),
                program.display().to_string(),
                "run".to_string(),
                "--flag".to_string(),
            ],
            "the argv is not the transient unit's"
        );
        assert!(
            !asked.exists(),
            "a start that succeeded asked the manager whether the unit was active"
        );
    }

    /// A refused start whose unit is **active** is the already-running answer, and it is systemd's is-active that
    /// says so — the name and the log travel with it, because that is what the job in flight is.
    #[test]
    fn a_refused_start_whose_unit_is_active_is_already_running() {
        let dir = scratch("running");
        let (runner, _) = stand_in(
            &dir,
            "systemd-run",
            1,
            "Unit holosmith-update.service already exists.",
        );
        let (manager, asked) = stand_in(&dir, "systemctl", 0, "");
        let tools = deploy::Tools {
            systemd_run: runner,
            systemctl: manager,
            ..deploy::Tools::default()
        };
        let data_root = dir.join("data");
        let source = dir.join("source");
        let program = dir.join("gate");
        let words = command(&program);

        let answer = start(
            &tools,
            &data_root,
            &Job {
                source: &source,
                command: &words,
            },
        )
        .expect("an already-running unit is an answer and not a failure");
        assert_eq!(
            answer,
            Answer::AlreadyRunning {
                unit: UNIT_NAME,
                log: log_path(&data_root)
            },
            "the refused start did not carry the unit and the log of the job in flight"
        );
        assert_eq!(
            recorded(&asked),
            vec![
                "--user".to_string(),
                "is-active".to_string(),
                "--quiet".to_string(),
                UNIT_NAME.to_string(),
            ],
            "the already-running question is not `systemctl --user is-active --quiet <unit>`"
        );
    }

    /// A refused start whose unit is **not** active is a failure, even though systemd's own sentence is the one an
    /// already-running unit produces: the prose is not the measurement, and a start that read it would answer a
    /// conflict for a job that is not there.
    #[test]
    fn a_refused_start_whose_unit_is_not_active_is_a_failure_whatever_its_prose_says() {
        let dir = scratch("not-active");
        let (runner, _) = stand_in(
            &dir,
            "systemd-run",
            1,
            "Unit holosmith-update.service already exists.",
        );
        // `is-active`'s non-zero exit for an inactive unit.
        let (manager, _) = stand_in(&dir, "systemctl", 3, "");
        let tools = deploy::Tools {
            systemd_run: runner,
            systemctl: manager,
            ..deploy::Tools::default()
        };
        let data_root = dir.join("data");
        let source = dir.join("source");
        let program = dir.join("gate");
        let words = command(&program);

        let said = start(
            &tools,
            &data_root,
            &Job {
                source: &source,
                command: &words,
            },
        )
        .expect_err("a start that failed with no unit running is a failure");
        assert!(
            said.contains("already exists"),
            "the failure does not carry the manager's own line: {said}"
        );
        assert!(
            said.contains("--user is-active --quiet holosmith-update"),
            "the failure does not say what was asked to tell the two apart: {said}"
        );
        assert!(
            said.contains("did not start"),
            "the failure does not end in what happened: {said}"
        );
    }

    /// A machine with no `systemd-run` gets one sentence, and it says the whole of what that means: the missing half,
    /// and that such a machine cannot promote at all today because the pipeline restarts through `systemctl --user`.
    /// No second child is started, because there is no job whose state could be asked about.
    #[test]
    fn a_machine_without_systemd_run_is_one_sentence_and_no_second_child() {
        let dir = scratch("no-runner");
        let (manager, asked) = stand_in(&dir, "systemctl", 0, "");
        let tools = deploy::Tools {
            systemd_run: dir.join("no-systemd-run-here"),
            systemctl: manager,
            ..deploy::Tools::default()
        };
        let data_root = dir.join("data");
        let source = dir.join("source");
        let program = dir.join("gate");
        let words = command(&program);

        let said = start(
            &tools,
            &data_root,
            &Job {
                source: &source,
                command: &words,
            },
        )
        .expect_err("a machine with no systemd-run cannot start a transient unit");
        assert!(
            said.contains("no-systemd-run-here") && said.contains("could not be run"),
            "the failure does not name the program it could not run: {said}"
        );
        assert!(
            said.contains("cannot promote at all today"),
            "the failure does not say what the absence costs this machine: {said}"
        );
        assert!(
            said.contains("systemctl --user restart"),
            "the failure does not say which half of the pipeline needs a manager: {said}"
        );
        assert!(
            said.contains("killed before its health poll"),
            "the failure does not say why a child of the service cannot do the job: {said}"
        );
        assert!(
            !asked.exists(),
            "a start with nothing to run asked the manager about a unit that no child named"
        );
    }

    /// An `is-active` that cannot be run at all is its own sentence: the start's failure cannot be classified, and a
    /// launcher that guessed would answer a conflict it never measured.
    #[test]
    fn an_is_active_that_cannot_run_is_its_own_sentence() {
        let dir = scratch("no-manager");
        let (runner, _) = stand_in(
            &dir,
            "systemd-run",
            1,
            "Unit holosmith-update.service already exists.",
        );
        let tools = deploy::Tools {
            systemd_run: runner,
            systemctl: dir.join("no-systemctl-here"),
            ..deploy::Tools::default()
        };
        let data_root = dir.join("data");
        let source = dir.join("source");
        let program = dir.join("gate");
        let words = command(&program);

        let said = start(
            &tools,
            &data_root,
            &Job {
                source: &source,
                command: &words,
            },
        )
        .expect_err("a manager that cannot be asked leaves the start unclassified");
        assert!(
            said.contains("no-systemctl-here") && said.contains("could not be run"),
            "the failure does not name the manager it could not run: {said}"
        );
        assert!(
            said.contains("already in flight"),
            "the failure does not say what the missing question was about: {said}"
        );
    }

    /// An `is-active` that ran and could not reach the manager is not a missing unit: `Failed to connect to bus` is
    /// exit 1 with a sentence of its own, which is what a `serve` started from cron or a container gets, and the
    /// launcher says this machine cannot tell rather than assert that the unit is not running — nothing measured that.
    #[test]
    fn an_is_active_that_cannot_reach_the_manager_does_not_say_the_unit_is_missing() {
        let dir = scratch("no-bus");
        let (runner, _) = stand_in(
            &dir,
            "systemd-run",
            1,
            "Unit holosmith-update.service already exists.",
        );
        let (manager, _) = stand_in(
            &dir,
            "systemctl",
            1,
            "Failed to connect to bus: No medium found",
        );
        let tools = deploy::Tools {
            systemd_run: runner,
            systemctl: manager,
            ..deploy::Tools::default()
        };
        let data_root = dir.join("data");
        let source = dir.join("source");
        let program = dir.join("gate");
        let words = command(&program);

        let said = start(
            &tools,
            &data_root,
            &Job {
                source: &source,
                command: &words,
            },
        )
        .expect_err("a manager that never answered leaves the start unclassified");
        assert!(
            said.contains("Failed to connect to bus"),
            "the failure does not quote the manager's own line: {said}"
        );
        assert!(
            said.contains("--user is-active --quiet holosmith-update"),
            "the failure does not say what could not be answered: {said}"
        );
        assert!(
            said.contains("cannot say whether a promotion is already in flight"),
            "the failure does not say what it does not know: {said}"
        );
        assert!(
            !said.contains("did not start"),
            "the failure claims the unit is not running, which nothing measured: {said}"
        );
    }

    /// The version is the field after the program's name on the first line, up to its first non-digit: `255` in
    /// `systemd 255 (255.4-1ubuntu8.18)`, and `255` in a `systemd 255.4` an older release prints. A first line that is
    /// not `systemd <number>` is no version at all, which is what makes it unmeasurable rather than zero.
    #[test]
    fn the_version_is_the_field_after_the_programs_name() {
        assert_eq!(major("systemd 255 (255.4-1ubuntu8.18)"), Some(255));
        assert_eq!(major("systemd 240 (240-1)"), Some(240));
        assert_eq!(major("systemd 255.4 (255.4-1)"), Some(255));
        assert_eq!(major("systemd 255\n+PAM +AUDIT +SELINUX"), Some(255));
        assert_eq!(major(""), None);
        assert_eq!(major("systemd"), None);
        assert_eq!(major("systemd (255)"), None);
        assert_eq!(major("systemctl 255"), None);
    }

    /// The floor is asked with one child and one flag: `systemctl --version`, no `--user` — the question is the
    /// package's version, and `--version` is answered without the bus a `serve` started from cron cannot reach.
    #[test]
    fn the_floor_is_asked_with_systemctl_version_and_nothing_else() {
        let dir = scratch("version-argv");
        let (manager, asked) =
            answering(&dir, "systemctl", 0, "systemd 255 (255.4-1ubuntu8.18)", "");
        let tools = deploy::Tools {
            systemctl: manager,
            ..deploy::Tools::default()
        };

        assert_eq!(
            below_floor(&tools),
            None,
            "a manager newer than the floor was refused"
        );
        assert_eq!(
            recorded(&asked),
            vec!["--version".to_string()],
            "the version question is not `systemctl --version`, and nothing else"
        );
    }

    /// The floor is inclusive: a 240 has both `append:` properties, so it is met and not refused, and a later release
    /// is met by the same reading.
    #[test]
    fn a_manager_at_or_above_the_floor_is_refused_nothing() {
        let dir = scratch("version-floor");
        for (label, said) in [
            ("floor", "systemd 240 (240-1)"),
            ("newer", "systemd 255 (255.4-1ubuntu8.18)"),
        ] {
            let (manager, _) = answering(&dir, &format!("systemctl-{label}"), 0, said, "");
            let tools = deploy::Tools {
                systemctl: manager,
                ..deploy::Tools::default()
            };
            assert_eq!(below_floor(&tools), None, "{said} is not below the floor");
        }
    }

    /// One version below the floor is one sentence, and it names the version it found, the floor, the half of the
    /// machine that works, and the only move there is: a newer systemd from the distribution.
    #[test]
    fn a_manager_below_the_floor_is_one_sentence_naming_both_numbers() {
        let dir = scratch("version-below");
        let (manager, _) = answering(&dir, "systemctl", 0, "systemd 239 (239-1)", "");
        let tools = deploy::Tools {
            systemctl: manager,
            ..deploy::Tools::default()
        };

        let said = below_floor(&tools).expect("239 is below the floor");
        assert!(said.contains("version 239"), "{said}");
        assert!(
            said.contains(&FLOOR.to_string()),
            "the sentence does not name the floor: {said}"
        );
        assert!(
            said.contains("append:"),
            "the sentence does not say what the floor is for: {said}"
        );
        assert!(
            said.contains("the install, the boot and the sessions"),
            "the sentence does not say what still works: {said}"
        );
        assert!(
            said.contains("distribution"),
            "the sentence does not say where a newer systemd comes from: {said}"
        );
    }

    /// Three ways of not knowing are not a refusal: a manager that cannot be run, one that exits non-zero, and output
    /// with no version in it all answer `None`, so a press on a machine nothing measured is never refused over the
    /// version (ADR-0044).
    #[test]
    fn a_version_that_could_not_be_measured_refuses_nothing() {
        let dir = scratch("version-unmeasured");
        // A manager that is not there at all: a container, or a `serve` on a machine with no systemd.
        let missing = deploy::Tools {
            systemctl: dir.join("no-systemctl-here"),
            ..deploy::Tools::default()
        };
        assert_eq!(below_floor(&missing), None);

        for (label, status, stdout) in [
            ("non-zero", 1, "systemd 239 (239-1)"),
            ("empty", 0, ""),
            ("unparsable", 0, "something else entirely"),
        ] {
            let (manager, _) = answering(&dir, &format!("systemctl-{label}"), status, stdout, "");
            let tools = deploy::Tools {
                systemctl: manager,
                ..deploy::Tools::default()
            };
            assert_eq!(
                below_floor(&tools),
                None,
                "{label} output was read as a version below the floor"
            );
        }
    }
}
