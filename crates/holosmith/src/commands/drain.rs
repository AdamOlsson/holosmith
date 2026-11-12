//! `holosmith drain [--wait 10m]`: refuse new work and wait for the turns that are running.
//!
//! The client half of [`crate::drain`], and deliberately small: one unix socket, one request line, then whatever
//! the server says until it says `result: <word>`. It is hand-written like [`crate::http::health::ask`] — a
//! socket, a read and a loop — because the alternative is a client library for a protocol this crate owns both
//! ends of.
//!
//! # Why the exit code is the `--wait`'s
//!
//! Spec item 10 step 3 loops the drain into a promotion that restarts the machine afterwards, so the one thing a
//! caller must be able to trust is "the machine is quiet, go" versus "it is not, do not": the terminal
//! `result: drained` is 0 and everything else — `expired` (a turn outlasted the wait), `refused`, a socket that
//! is not there, a server that went away mid-drain — is 1. The state is reported as well as the code, because the
//! operator's next move depends on which of those it was.
//!
//! # Why the lines are printed as they arrive
//!
//! A drain waits for a turn, which is minutes on a real machine. Printing what the server says as it says it is
//! the difference between a command that is working and one that looks hung, and it is the reason the server
//! writes progress lines at all.
//!
//! # What this command does not read
//!
//! No config file: the path is the admin directory ([`crate::deploy::ADMIN_DIR`], or `--admin-dir`), which is a
//! fixed path of the machine rather than a setting, and the wait is the flag's. A `--config` given to it is
//! therefore accepted and unused — the honest thing to do with a flag that names a file this command has no
//! question for, rather than refusing a command line that also serves `serve`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::deploy::ADMIN_DIR;
use crate::drain::{self, Ending};

/// How long the client waits for the server's **first** answer before it calls the socket dead.
///
/// Not a deadline for the drain — that is `--wait`, and it is the server's — but the answer to "is anything
/// there at all": a socket whose server accepted and then wedged would otherwise hang this command for ever.
/// Once the first line has arrived the bound is the wait the operator asked for (see [`ask_with`]), because a
/// drain is *silent* between progress lines by design: a turn that takes thirty seconds produces two lines and
/// then nothing until it settles.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// What the command line asked for.
#[derive(Debug)]
pub struct DrainArgs {
    pub wait: Duration,
    /// The admin directory, or `None` for the machine's own.
    pub admin_dir: Option<PathBuf>,
}

pub fn run(args: DrainArgs) -> ExitCode {
    let admin_dir = args.admin_dir.unwrap_or_else(|| PathBuf::from(ADMIN_DIR));
    let path = drain::socket_path(&admin_dir);
    match ask(&path, args.wait) {
        Ok(ending) => {
            if ending.succeeded() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(sentence) => {
            eprintln!("holosmith: drain: {sentence}");
            ExitCode::FAILURE
        }
    }
}

/// How one exchange over the socket ended, for the callers that need more than the terminal word.
///
/// `ending` is the `result:` line's word, or `None` when the exchange ended before one arrived — either the
/// server stopped talking (see [`ask_with`]) or the caller's callback asked to stop. `stopped` says which of the
/// two it was, because the callers treat them differently: `holosmith drain` never stops early, while a
/// promotion stops on the drain's own first line when the machine was already draining (the acceptance:
/// "refuses to upgrade while draining is already in progress").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Exchange {
    /// The `result:` word, when the server reached one.
    pub ending: Option<Ending>,
    /// Whether the callback asked to stop reading before a `result:` line arrived.
    pub stopped: bool,
}

/// Connect, ask, print every line, and answer how the drain ended.
///
/// The last line the server sends is `result: <word>`; everything before it is the account of what happened and
/// is printed as it arrives. A connection that ends without that line is a server that stopped mid-drain — a
/// `SIGTERM` or a crash — and says so rather than reporting the state it left.
fn ask(path: &Path, wait: Duration) -> Result<Ending, String> {
    let exchange = ask_with(path, wait, ANSWER_TIMEOUT, &mut |line| {
        println!("drain: {line}");
        // `holosmith drain` reads to the end whatever the machine says: its exit code is the drain's own word, and
        // a second drain on a draining machine is the server's idempotent answer rather than this client's refusal.
        true
    })?;
    match exchange.ending {
        Some(ending) => Ok(ending),
        None => Err(format!(
            "{} closed the connection before saying how the drain ended; the machine is draining if it said so, \
             and `holosmith drain --wait` will say again",
            path.display()
        )),
    }
}

/// [`ask`], with the "nothing yet" bound as a parameter so a test can make it small and the line callback as the
/// second caller's seam.
///
/// `on_line` sees every line the server sends and answers whether to keep reading. It is how one client serves two
/// commands with two different endings: `holosmith drain` prints and reads on, while `holosmith upgrade` stops on
/// [`crate::drain::ALREADY`] and refuses the promotion rather than waiting out a drain it did not start.
///
/// The read loop is the part that matters: a read that times out is **not** a dead socket. The server writes a
/// line when a fact changes — `waiting: 1 turn still running` and then nothing until the turn settles — so a
/// drain that is working looks exactly like a drain whose server has gone quiet, for minutes. Treating the first
/// timeout as fatal made `holosmith drain --wait 1m` fail after five seconds against any turn longer than that,
/// which is the story's own demo. So: the first line is bounded by `silence` (is anything there at all), and
/// after that the bound is `wait + silence` — the deadline the operator asked for, plus the same grace for the
/// last line. A read error that is not a timeout is still fatal, and a closed connection still ends the loop and
/// is reported as the missing `result:` it is.
pub(crate) fn ask_with(
    path: &Path,
    wait: Duration,
    silence: Duration,
    on_line: &mut dyn FnMut(&str) -> bool,
) -> Result<Exchange, String> {
    let mut stream = UnixStream::connect(path).map_err(|error| {
        format!(
            "{} could not be connected to ({error}); the drain socket is only there while this machine's server \
             is running, and it is never reachable over the network",
            path.display()
        )
    })?;
    let _ = stream.set_read_timeout(Some(silence));
    let _ = stream.set_write_timeout(Some(silence));
    // Milliseconds on the wire: the server's own sequence measures the wait in a `Duration`, and a unit suffix
    // would be a second grammar for the same number (see [`crate::drain::Request::parse`]).
    let millis = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
    stream
        .write_all(format!("drain {millis}\n").as_bytes())
        .map_err(|error| {
            format!(
                "the request could not be sent to {} ({error})",
                path.display()
            )
        })?;
    stream.flush().map_err(|error| {
        format!(
            "the request could not be sent to {} ({error})",
            path.display()
        )
    })?;
    // The two bounds: the first line, and everything after it. `checked_add` because a `--wait` of a million
    // years must not panic here; a `None` bound only means "no further bound", which the server's own refusal of
    // a wait the clock cannot add makes unreachable in practice.
    let started = std::time::Instant::now();
    let first = started.checked_add(silence);
    let overall = wait
        .checked_add(silence)
        .and_then(|total| started.checked_add(total));
    let mut seen = false;
    let mut ending = None;
    let mut stopped = false;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                seen = true;
                let line = line.trim_end_matches(['\r', '\n']);
                let keep = on_line(line);
                if let Some(word) = line.strip_prefix("result: ") {
                    // A word this build does not know is not a success: the client and the server are one binary,
                    // so it is a version skew or a corrupted line, and neither is something to call drained.
                    ending = Some(Ending::from_word(word.trim()).ok_or_else(|| {
                        format!(
                            "{} answered a result this build does not know",
                            path.display()
                        )
                    })?);
                }
                if !keep {
                    stopped = true;
                    break;
                }
            }
            // A timeout is "nothing has changed yet", which is what a drain waiting for a turn looks like.
            Err(error) if is_timeout(&error) => {
                let bound = if seen { overall } else { first };
                if let Some(bound) = bound
                    && std::time::Instant::now() >= bound
                {
                    return Err(format!(
                        "{} said nothing for {}; the drain's own state is whatever the machine says now",
                        path.display(),
                        crate::config::Duration::from_millis(silence.as_millis() as u64)
                    ));
                }
            }
            Err(error) => {
                return Err(format!(
                    "{} stopped answering mid-drain ({error}); the drain's own state is whatever the machine \
                     says now",
                    path.display()
                ));
            }
        }
    }
    Ok(Exchange { ending, stopped })
}

/// Whether a read error is the socket's own timeout rather than a broken connection.
fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    /// A fake admin socket that says `lines`, sleeping `pause` between them and `hold` before it closes.
    ///
    /// `hold` is what makes "says nothing at all" a measured state rather than a connection that closes at once:
    /// an accepted socket that closes immediately is the *other* arm (`… before saying how the drain ended`).
    fn fake(
        label: &str,
        lines: Vec<String>,
        pause: Duration,
        hold: Duration,
    ) -> (PathBuf, std::thread::JoinHandle<()>) {
        let dir = std::env::temp_dir().join(format!(
            "holosmith-drain-client-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the socket's directory is creatable");
        let path = crate::drain::socket_path(&dir);
        let listener = UnixListener::bind(&path).expect("the fake socket binds");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a client connects");
            let mut request = [0_u8; 64];
            let _ = std::io::Read::read(&mut stream, &mut request);
            for line in lines {
                if std::io::Write::write_all(&mut stream, format!("{line}\n").as_bytes()).is_err() {
                    return;
                }
                std::thread::sleep(pause);
            }
            std::thread::sleep(hold);
        });
        (path, handle)
    }

    /// The story's own demo, in miniature: a turn that outlasts the "nothing yet" bound must not be read as a
    /// dead server. Measured with a 100 ms silence bound and a 300 ms gap, which is the same shape a minute-long
    /// turn has against [`ANSWER_TIMEOUT`] — and it fails against the `for line in lines()` loop this replaced.
    #[test]
    fn a_silent_gap_longer_than_the_bound_is_not_a_dead_server() {
        let (path, server) = fake(
            "gap",
            vec![
                "state: draining".to_string(),
                "waiting: 1 turn still running".to_string(),
                "stopped: 1 session with reason server_restart".to_string(),
                "result: drained".to_string(),
            ],
            Duration::from_millis(150),
            Duration::ZERO,
        );
        let started = Instant::now();
        let mut seen: Vec<String> = Vec::new();
        let exchange = ask_with(
            &path,
            Duration::from_secs(30),
            Duration::from_millis(100),
            &mut |line| {
                seen.push(line.to_string());
                true
            },
        );
        server.join().expect("the fake server finished");
        assert_eq!(
            exchange,
            Ok(Exchange {
                ending: Some(Ending::Drained),
                stopped: false
            }),
            "a quiet drain must not be a failure"
        );
        assert_eq!(seen.len(), 4, "the callback saw every line: {seen:?}");
        assert!(
            started.elapsed() >= Duration::from_millis(450),
            "the client returned before the server had said everything"
        );
    }

    /// The bound that remains: a socket whose server accepted and then said nothing at all is still a failure.
    #[test]
    fn a_server_that_never_says_anything_is_refused() {
        let (path, server) = fake(
            "silent",
            Vec::new(),
            Duration::ZERO,
            Duration::from_millis(300),
        );
        let exchange = ask_with(
            &path,
            Duration::from_secs(30),
            Duration::from_millis(50),
            &mut |_| true,
        );
        server.join().expect("the fake server finished");
        let error = exchange.expect_err("a server that says nothing is not a drain");
        assert!(error.contains("said nothing for"), "{error}");
    }

    /// A connection that closes without a `result:` line is a server that went away mid-drain, and the caller is
    /// told so rather than given the state it left — even when it said `state: draining` first. (`ask` is where that
    /// becomes the command's exit code.)
    #[test]
    fn a_connection_closed_before_the_result_hands_back_no_ending() {
        let (path, server) = fake(
            "closed",
            vec!["state: draining".to_string()],
            Duration::from_millis(10),
            Duration::ZERO,
        );
        let exchange = ask_with(
            &path,
            Duration::from_secs(30),
            Duration::from_millis(100),
            &mut |_| true,
        );
        server.join().expect("the fake server finished");
        assert_eq!(
            exchange,
            Ok(Exchange {
                ending: None,
                stopped: false
            })
        );
        let error =
            ask(&path, Duration::from_secs(30)).expect_err("a missing result is not a drain");
        assert!(
            error.contains("the drain socket is only there")
                || error.contains("before saying how the drain ended"),
            "{error}"
        );
    }

    /// The promotion's seam: a callback that answers "stop" ends the read at once, and the exchange says so rather
    /// than reporting a `result:` the client never waited for. This is what makes "refuse while draining is already
    /// in progress" a refusal instead of a ten-minute wait.
    #[test]
    fn a_callback_that_stops_ends_the_read_at_that_line() {
        let (path, server) = fake(
            "stop",
            vec![
                crate::drain::ALREADY.to_string(),
                "stopped: 2 sessions with reason server_restart".to_string(),
                "result: drained".to_string(),
            ],
            Duration::from_millis(10),
            Duration::ZERO,
        );
        let started = Instant::now();
        let mut seen: Vec<String> = Vec::new();
        let exchange = ask_with(
            &path,
            Duration::from_secs(30),
            Duration::from_millis(100),
            &mut |line| {
                seen.push(line.to_string());
                line != crate::drain::ALREADY
            },
        );
        server.join().expect("the fake server finished");
        assert_eq!(
            exchange,
            Ok(Exchange {
                ending: None,
                stopped: true
            })
        );
        assert_eq!(seen, vec![crate::drain::ALREADY.to_string()]);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the read waited for lines after the one it stopped on"
        );
    }
}
