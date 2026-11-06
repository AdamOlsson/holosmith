//! The preflight (HS-024, spec item 10 §1): what a machine must have, and the sentences it fails with.
//!
//! These tests drive `preflight::check_with` against a machine they build: `git` and `df` are scripts the
//! test wrote (the `Tools` seam), and the daemon is a unix socket the test answers itself. That is the only
//! way to test a check about the host without owning the host, and it also keeps the tests independent of
//! this machine's git version and free space — which is exactly what a test against the real `df` would
//! make flaky.
//!
//! The sentences are asserted by their *fix*, not by their whole text: the rule is "one sentence naming the
//! fix", and a test that pinned the prose would fail on a wording edit that keeps the rule.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use holosmith::config::Config;
use holosmith::preflight::{self, Check, Tools};

mod support;
use support::TempDir;

/// Hold this for the whole body of a test here: one test at a time.
///
/// `exec` of a file that *any* process holds open for writing fails with `ETXTBSY` ("Text file busy"),
/// and `std::process::Command`'s fork happens on another thread's behalf: the child inherits the write
/// descriptor of a shim written a moment earlier and keeps it until its own `exec` finishes. Measured at
/// roughly 3 runs in 40 before this lock (PER-56's note), which is what made the gate redden at random.
/// Every test in this file writes shims and then runs them, and nothing else in this binary forks, so
/// serialising each whole body is the whole fix.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An executable shim for `name` answering `body`, in `dir`'s own `bin`.
fn shim(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
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

/// A `git` shim answering `version`.
fn git_shim(dir: &TempDir, version: &str) -> std::path::PathBuf {
    shim(dir, "git", &format!("echo 'git version {version}'"))
}

/// A `df -Pk` shim answering `available_kb` free.
fn df_shim(dir: &TempDir, available_kb: u64) -> std::path::PathBuf {
    shim(
        dir,
        "df",
        &format!(
            "printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n\
             /dev/mapper/root 1000000000 1 {available_kb} 1%% /\\n'"
        ),
    )
}

/// The tools a test built.
fn tools(dir: &TempDir, version: &str, available_kb: u64) -> Tools {
    Tools {
        git: git_shim(dir, version),
        df: df_shim(dir, available_kb),
    }
}

/// A config whose data root is inside `dir` and whose daemon is `docker_host`.
fn config(dir: &TempDir, docker_host: Option<String>) -> Config {
    let data_root = dir.path().join("data");
    // The directory exists, because that is the state `serve` is in when it asks: it creates the data root first,
    // and a missing one is a different finding (`doctor`'s, ADR-0054).
    std::fs::create_dir_all(&data_root).expect("the data root is creatable");
    Config {
        data_root,
        docker_host,
        ..Config::default()
    }
}

/// The complaint whose check is `check`, if the list has one.
fn find(complaints: &[preflight::Complaint], check: Check) -> Option<&str> {
    complaints
        .iter()
        .find(|complaint| complaint.check == check)
        .map(|complaint| complaint.sentence.as_str())
}

/// A daemon that answers one `GET /_ping` with Docker's own `200 OK`, on `path`.
///
/// One connection, one answer, then the listener goes: the preflight asks once, and a listener that
/// accepted twice would hide a check that retried.
fn fake_daemon(path: &Path) -> Arc<AtomicBool> {
    let listener = UnixListener::bind(path).expect("the test socket is bindable");
    let answered = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&answered);
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nApi-Version: 1.51\r\n\r\nOK");
            let _ = stream.shutdown(Shutdown::Both);
            seen.store(true, Ordering::SeqCst);
        }
    });
    answered
}

#[test]
fn a_machine_with_nothing_wrong_has_no_complaints() {
    let _serial = one_at_a_time();
    // Every check passes: a daemon on a socket this test answers, git 2.43, and 480 GB free.
    let dir = TempDir::new("preflight-clean");
    let socket = dir.file("docker.sock");
    let answered = fake_daemon(&socket);
    let complaints = preflight::check_with(
        &config(&dir, Some(format!("unix://{}", socket.display()))),
        &tools(&dir, "2.43.0", 480_000_000),
    );
    assert!(
        complaints.is_empty(),
        "a clean machine complained: {complaints:?}"
    );
    assert!(
        answered.load(Ordering::SeqCst),
        "the daemon was never asked: the check passed without a ping"
    );
}

#[test]
fn a_daemon_that_is_not_there_is_one_sentence_naming_the_fix() {
    let _serial = one_at_a_time();
    let dir = TempDir::new("preflight-no-daemon");
    let complaints = preflight::check_with(
        &config(
            &dir,
            Some(format!("unix://{}", dir.file("absent.sock").display())),
        ),
        &tools(&dir, "2.43.0", 480_000_000),
    );
    assert_eq!(
        complaints.len(),
        1,
        "only the daemon is wrong here: {complaints:?}"
    );
    let docker = find(&complaints, Check::Docker).expect("an absent daemon is a complaint");
    assert!(
        docker.contains("start it with") && docker.contains("docker_host"),
        "the sentence names no fix: {docker}"
    );
}

#[test]
fn an_old_git_and_a_full_disk_are_both_reported_at_once() {
    let _serial = one_at_a_time();
    // The rule the config validator follows: every problem in one answer, because a machine missing two
    // things should take two edits and one restart.
    let dir = TempDir::new("preflight-two");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let complaints = preflight::check_with(
        &config(&dir, Some(format!("unix://{}", socket.display()))),
        // 1 GB free, which is under the default `min_free_gb` of 20.
        &tools(&dir, "2.34.1", 1_024_000),
    );
    assert_eq!(
        complaints.len(),
        2,
        "one of the two failures was swallowed: {complaints:?}"
    );
    let git = find(&complaints, Check::Git).expect("git 2.34 is too old");
    assert!(
        git.contains("2.38") && git.contains("upgrade"),
        "the git sentence names no fix: {git}"
    );
    let disk = find(&complaints, Check::Disk).expect("1 GB is under the 20 GB default");
    assert!(
        disk.contains("min_free_gb"),
        "the disk sentence does not name the key that decided it: {disk}"
    );
}

#[test]
fn a_git_that_cannot_be_run_is_a_complaint_and_not_a_pass() {
    let _serial = one_at_a_time();
    // A `git` that is not there: `Command` fails with `NotFound`, which must be reported rather than
    // treated as "no opinion". A check that cannot run is never a pass.
    let dir = TempDir::new("preflight-no-git");
    let socket = dir.file("docker.sock");
    fake_daemon(&socket);
    let complaints = preflight::check_with(
        &config(&dir, Some(format!("unix://{}", socket.display()))),
        &Tools {
            git: dir.file("no-such-git"),
            df: df_shim(&dir, 480_000_000),
        },
    );
    assert_eq!(
        complaints.len(),
        1,
        "only git is wrong here: {complaints:?}"
    );
    let git = find(&complaints, Check::Git).expect("a git that cannot run is a complaint");
    assert!(
        git.contains("install git"),
        "the sentence names no fix: {git}"
    );
}

#[test]
fn a_daemon_that_answers_something_else_is_a_complaint() {
    let _serial = one_at_a_time();
    // A socket that accepts and answers `500` is not a daemon that can run sessions, and a check that
    // only proved "something is listening" would pass it.
    let dir = TempDir::new("preflight-bad-daemon");
    let socket = dir.file("docker.sock");
    let listener = UnixListener::bind(&socket).expect("the test socket is bindable");
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\n\r\nno");
        }
    });
    // A ping raced against the accept: the listener is bound before the check runs, so the connect cannot
    // fail, and the answer is what has to.
    let complaints = preflight::check_with(
        &config(&dir, Some(format!("unix://{}", socket.display()))),
        &tools(&dir, "2.43.0", 480_000_000),
    );
    let docker = find(&complaints, Check::Docker).expect("a daemon answering 500 is a complaint");
    assert!(
        docker.contains("did not answer"),
        "the sentence does not say what went wrong: {docker}"
    );
}

#[test]
fn the_check_word_leads_every_sentence() {
    let _serial = one_at_a_time();
    // The report is one line per complaint, so the word before the colon is what an operator greps for.
    let dir = TempDir::new("preflight-words");
    let complaints = preflight::check_with(
        &config(
            &dir,
            Some(format!("unix://{}", dir.file("gone.sock").display())),
        ),
        &tools(&dir, "2.20.1", 480_000_000),
    );
    assert_eq!(complaints.len(), 2, "{complaints:?}");
    for complaint in &complaints {
        assert_eq!(
            complaint.to_string(),
            format!("{}: {}", complaint.check, complaint.sentence),
            "a sentence does not name its check"
        );
    }
}
