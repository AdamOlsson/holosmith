//! `holosmith drain`, the command half: the request line, the printed report and the exit code (HS-051).
//!
//! The sequence is `tests/drain.rs`'s; this file is about the thing an operator and a promotion script see. Its
//! subject is the **exit code**, because spec item 10 step 3 loops the drain into a restart: 0 must mean "the
//! machine is quiet, go" and 1 everything else, and the two arms that are easy to confuse — a `--wait` that ran
//! out and a socket that was not there — must not both read as success.
//!
//! The server side is a hand-written socket in each test, so the assertion is the bytes the binary really sends
//! and the lines it really prints, with no `serve` to boot and no daemon to have.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc;

mod support;
use support::TempDir;

/// A fake admin socket that reads one request line and answers with `lines`.
///
/// The returned receiver carries the request line the binary sent, so the wire's own spelling is an assertion
/// rather than an assumption.
fn answer_once(path: &Path, lines: &[&'static str]) -> mpsc::Receiver<String> {
    let listener = UnixListener::bind(path).expect("the fake admin socket binds");
    let (sender, receiver) = mpsc::channel();
    let lines: Vec<&'static str> = lines.to_vec();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut request = String::new();
        let mut reader = BufReader::new(stream.try_clone().expect("the connection clones"));
        let _ = reader.read_line(&mut request);
        let _ = sender.send(request);
        for line in lines {
            if writeln!(stream, "{line}").is_err() {
                return;
            }
        }
        let _ = stream.flush();
        // Returning drops the connection, which is how the client learns the report is over.
    });
    receiver
}

/// The admin directory a run uses, under its own temp dir so no test can meet another's socket.
fn admin(dir: &TempDir) -> PathBuf {
    let path = dir.file("run/holosmith");
    std::fs::create_dir_all(&path).expect("the admin directory is creatable");
    path
}

fn drain(dir: &TempDir, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .arg("drain")
        .arg("--admin-dir")
        .arg(admin(dir))
        .args(arguments)
        .env_remove("HOLOSMITH_LOG")
        .output()
        .expect("the binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn a_finished_drain_prints_every_line_and_exits_zero() {
    let dir = TempDir::new("drain-cli-drained");
    let path = admin(&dir).join("drain.sock");
    let request = answer_once(
        &path,
        &[
            "state: draining",
            "waiting: 1 turn still running",
            "stopped: 2 sessions with reason server_restart",
            "result: drained",
        ],
    );

    // The default wait is the spec's `--wait 10m`.
    let output = drain(&dir, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(request.recv().expect("the request line"), "drain 600000\n");
    assert_eq!(
        stdout(&output),
        "drain: state: draining\n\
         drain: waiting: 1 turn still running\n\
         drain: stopped: 2 sessions with reason server_restart\n\
         drain: result: drained\n",
        "the report is printed as it arrives and nowhere else"
    );
}

#[test]
fn an_expired_wait_exits_non_zero_although_the_answer_was_whole() {
    let dir = TempDir::new("drain-cli-expired");
    let path = admin(&dir).join("drain.sock");
    let request = answer_once(
        &path,
        &[
            "state: draining",
            "waiting: 1 turn still running",
            "note: the wait ran out after 200ms with a turn still running; this machine is still draining",
            "result: expired",
        ],
    );

    let output = drain(&dir, &["--wait", "200ms"]);
    assert_eq!(request.recv().expect("the request line"), "drain 200\n");
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("drain: result: expired"),
        "{}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("still draining"),
        "the operator is told the state was left on: {}",
        stdout(&output)
    );
}

#[test]
fn every_ending_but_drained_is_a_failure() {
    let dir = TempDir::new("drain-cli-refused");
    let path = admin(&dir).join("drain.sock");
    let _request = answer_once(
        &path,
        &[
            "note: the only request this socket answers is `drain <milliseconds>`",
            "result: refused",
        ],
    );
    let output = drain(&dir, &["--wait", "1s"]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
}

#[test]
fn a_socket_that_is_not_there_names_the_path_and_the_rule() {
    let dir = TempDir::new("drain-cli-missing");
    let output = drain(&dir, &[]);
    let said = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{said}");
    assert!(said.contains("drain.sock"), "{said}");
    assert!(
        said.contains("never reachable over the network"),
        "the refusal says what this command is not: {said}"
    );
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
}

#[test]
fn a_server_that_stops_mid_drain_is_a_failure_rather_than_a_claim() {
    let dir = TempDir::new("drain-cli-cut");
    let path = admin(&dir).join("drain.sock");
    // No `result:` line: the server went away after saying what it was doing.
    let _request = answer_once(&path, &["state: draining"]);
    let output = drain(&dir, &["--wait", "1s"]);
    let said = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{said}");
    assert!(
        said.contains("before saying how the drain ended"),
        "a connection that ended early is named as such: {said}"
    );
}

#[test]
fn the_wait_is_the_configs_own_duration_grammar() {
    let dir = TempDir::new("drain-cli-wait");
    let path = admin(&dir).join("drain.sock");
    let request = answer_once(&path, &["state: draining", "result: drained"]);
    let output = drain(&dir, &["--wait", "1500ms"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(request.recv().expect("the request line"), "drain 1500\n");

    // A bare number has no unit and is refused by the parser the config file uses, before anything is dialled.
    let output = drain(&dir, &["--wait", "10"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("has no unit"),
        "the config's sentence, not a second one: {}",
        stderr(&output)
    );
}
