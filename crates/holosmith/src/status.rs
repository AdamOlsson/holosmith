//! `holosmith status`: the machine, as one line per fact (HS-050, spec's Commands table).
//!
//! The shape is `holosmith version`'s — `key value`, one fact per line — and every value has `doctor`'s three
//! arms (ADR-0054): what was **measured**, what could not be measured (`unknown: <sentence>`) and what failed to
//! be read (`fail: <sentence>`). The third arm is the point of this module. A `status` that printed `0` where it
//! could not look would be the one thing this build refuses to do, and `GLOSSARY.md` records the distinction the
//! report rests on: the **health endpoint** answers about the running binary, while `holosmith status` "also asks
//! other machines".
//!
//! # Which reader owns which fact
//!
//! Every line comes from the one place that already knew the answer, and none of them is re-derived here:
//!
//! | line | reader |
//! | --- | --- |
//! | `role` | the config file — the machine's role is a setting, not a measurement |
//! | `version` | [`crate::version::VersionInfo`], the same value `holosmith version --json` prints |
//! | `uptime` | `GET /api/health` ([`crate::http::health::ask`]): [`Runtime::uptime_s`] is the one owner of "how long has this process been up" |
//! | `sessions <status>` | `sessions` rows, through a **read-only** [`crate::db::Reader`] |
//! | `proxy <name>` | `preflight::daemon` first (did it answer?), then [`crate::proxies::running`] |
//! | `machines` | nothing yet — the registry is HS-068 (phase 3) |
//!
//! The proxy pair is the one line family whose first question is *whether it could be asked at all*: `Docker::running`
//! folds every daemon error into `false`, so a `status` that asked it alone would print `not running` for a machine
//! nobody looked at. `preflight::daemon` is the reader that separates the two, and a daemon that does not answer makes
//! both lines `unknown:` — not a failure, because describing a machine whose daemon is down is what this command is for.
//!
//! [`Runtime::uptime_s`]: crate::runtime::Runtime::uptime_s
//!
//! # Why uptime is a request and not a file
//!
//! `status` is a **second** process, so the server's start time has to be read from somewhere. The three
//! candidates were the process's own record (a file `serve` writes at boot), systemd's
//! `ActiveEnterTimestamp` (`systemctl show -p … --value`, which answers only on a systemd machine and not at all
//! for a `serve` run by hand), and the health endpoint — which the running binary already answers, with
//! `Runtime`'s own clock behind it. The request wins on three counts: the fact has one writer (the process that
//! knows it), it works for a hand-run `serve` and for a unit alike, and it needs neither a new file under the data
//! root nor `unsafe`. The cost is recorded rather than hidden: a machine whose server is not answering has no
//! uptime line, and says so instead of printing a number that belongs to nothing.
//!
//! # Why `role` and `version` are not the server's answer
//!
//! Both are facts this process always has — the config file it read and the binary it is running — so a `status`
//! on a stopped machine still describes the machine. The health payload carries copies of both, and they are
//! deliberately not consulted: two sources for one line is a shape that would have to say which it used. The
//! limit is worth stating: `version` is **this binary's**, and a hand-started older `serve` on the same machine
//! would have its own.
//!
//! # Why the counts are six lines and not two
//!
//! The acceptance says "live/stopped session counts", and `GLOSSARY.md` defines *Live* as exactly three statuses
//! and *Stopped* as one — so a command that summed `stopped`, `error` and `deleting` into one number would call an
//! `error` row stopped, and a command that printed only the two would hide the `error` rows an operator most needs
//! to see. One line per [`Status`], in the enum's own order, with the enum's own words, and the reader is one
//! `GROUP BY` rather than six counts.

use std::fmt::{self, Write as _};

use crate::config::Config;
use crate::db;
use crate::http::health;
use crate::preflight::{self, Outcome};
use crate::proxies;
use crate::sessions::Status;
use crate::version::VersionInfo;

/// The sentence the "other machines" line carries until there is a registry to read.
///
/// It names the story rather than saying `none`: "no machine has registered" and "the feature that would register
/// one does not exist" are different facts, and on a phase-2 machine only the second is true.
pub const NO_REGISTRY: &str = "no hub has registered a machine with this one, and the registry that would hold one \
     is phase 3 (HS-068) — so there is no other machine's health to report";

/// One fact of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub key: String,
    pub value: Value,
}

impl fmt::Display for Line {
    /// `uptime 42s`, `uptime unknown: …`, `sessions fail: …` — `key value`, with the arm named when it is not a
    /// measurement.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            Value::Measured(value) => write!(f, "{} {}", self.key, value),
            Value::Unknown(sentence) => write!(f, "{} unknown: {}", self.key, sentence),
            Value::Failed(sentence) => write!(f, "{} fail: {}", self.key, sentence),
        }
    }
}

/// What one line's read answered: the three arms every fact has (ADR-0044, ADR-0054).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// The fact was measured, and this is it.
    Measured(String),
    /// The fact could not be measured, and this is why — not a failure, and not a zero.
    Unknown(String),
    /// The read itself failed. This is the only arm that changes the exit code.
    Failed(String),
}

/// The whole report, in the order it prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<Line>,
}

impl Report {
    /// Whether anything failed to be read, which is exactly what the exit code says.
    ///
    /// `unknown` is not a failure: a stopped machine, a machine with no database and a machine whose host registry
    /// does not exist yet are all machines this command describes successfully (ADR-0054's rule, applied to a
    /// report instead of a check).
    pub fn failed(&self) -> bool {
        self.lines
            .iter()
            .any(|line| matches!(line.value, Value::Failed(_)))
    }

    /// The whole report as the operator reads it: one line per fact, no header and no tally, because every line is
    /// already `key value` and a count of lines would be a fact nobody asked for.
    pub fn render(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            // A `String` cannot fail to be written to, which is why this is not a `?`.
            let _ = writeln!(text, "{line}");
        }
        text
    }
}

/// The machine questions `status` asks, as one parameter — [`crate::proxies::Containers`], because the only
/// question it has for the daemon is "is this container running", which is the proxy recipe's own read.
///
/// [`crate::docker::Docker`] implements it (and is what `commands::status` builds), so a test stands in for the
/// whole daemon with no socket at all — the [`crate::doctor::Machine`] pattern.
pub fn run(config: &Config, machine: &dyn proxies::Containers) -> Report {
    let mut lines = vec![
        measured("role", config.role.as_str()),
        measured("version", VersionInfo::current().version),
        uptime(config),
    ];
    lines.extend(counts(config));
    lines.extend(proxies(config, machine));
    lines.push(Line {
        key: "machines".to_string(),
        value: Value::Unknown(NO_REGISTRY.to_string()),
    });
    Report { lines }
}

/// The two `proxy <name>` lines, which are three-armed like every other fact.
///
/// **The daemon is asked first, and that question is what makes these lines honest.** [`crate::proxies::running`]
/// answers a `bool`, and [`crate::docker::Docker::running`] folds *every* failure into `false` — a socket this user
/// cannot open, a daemon that is down, an account outside the `docker` group — so asking it alone would print
/// `not running` for a machine nobody looked at. `preflight::daemon` is the one reader that separates "it answered"
/// from "it could not be asked" (ADR-0028/ADR-0054), and it is the same finding `doctor` and `serve` rest on, so
/// `status` cannot say something about the daemon that the boot's refusal would contradict.
///
/// A missing daemon is **not** a failure of this command: describing a machine whose daemon is down is exactly what
/// `status` is for, and the two `unknown:` lines say which fact was not measured and why.
fn proxies(config: &Config, machine: &dyn proxies::Containers) -> Vec<Line> {
    let daemon = preflight::daemon(config);
    if daemon.outcome != Outcome::Pass {
        return proxies::NAMES
            .into_iter()
            .map(|name| Line {
                key: format!("proxy {name}"),
                value: Value::Unknown(format!(
                    "whether it is running was not measured: {}",
                    daemon.sentence
                )),
            })
            .collect();
    }
    proxies::running(config, machine)
        .into_iter()
        .zip(proxies::NAMES)
        .map(|(running, name)| {
            measured(
                &format!("proxy {name}"),
                if running { "running" } else { "not running" },
            )
        })
        .collect()
}

/// One measured fact.
fn measured(key: &str, value: impl Into<String>) -> Line {
    Line {
        key: key.to_string(),
        value: Value::Measured(value.into()),
    }
}

/// The `uptime` line: the running server's, asked of the health endpoint.
///
/// Three answers rather than two, and the sentences are different on purpose — an address the config asks the
/// kernel for (`:0`) has no port to ask, a port nothing answers on is a machine that is not serving, and a body
/// that is not the payload is a port somebody else holds. All three are `unknown` and none of them is a failure:
/// a stopped machine is a machine `status` is *for*.
fn uptime(config: &Config) -> Line {
    let listen = config.control_listen;
    match health::ask(listen) {
        Ok(answer) => measured("uptime", format!("{}s", answer.uptime_s)),
        Err(sentence) => Line {
            key: "uptime".to_string(),
            value: Value::Unknown(format!(
                "{sentence}: so this machine is not serving on {listen}, and there is no uptime to report"
            )),
        },
    }
}

/// The six `sessions <status>` lines, from one query, with the statuses that have no rows filled in with zero.
///
/// A missing database is six zeros rather than a finding: a machine that has never served holds no sessions, which
/// is what `serve` creating one at startup means (`doctor`'s database check says the same in its own words). A
/// database that exists and cannot be read **is** a failure, because then the zeros would be a guess.
fn counts(config: &Config) -> Vec<Line> {
    let path = config.data_root.join(db::DATABASE_FILE);
    let found = match db::Reader::open(&path) {
        Ok(None) => Ok(Vec::new()),
        Ok(Some(reader)) => reader.sessions().counts_by_status().map_err(|error| {
            format!(
                "the sessions in {} could not be counted ({error})",
                path.display()
            )
        }),
        Err(error) => Err(format!(
            "the database at {} could not be opened to be read ({error})",
            path.display()
        )),
    };
    match found {
        Ok(found) => Status::ALL
            .into_iter()
            .map(|status| {
                let count = found
                    .iter()
                    .find(|(seen, _)| *seen == status)
                    .map_or(0, |(_, count)| *count);
                measured(&format!("sessions {status}"), count.to_string())
            })
            .collect(),
        Err(sentence) => vec![Line {
            key: "sessions".to_string(),
            value: Value::Failed(sentence),
        }],
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::config::{Config, Role};
    use crate::proxies::Containers;

    /// The daemon, as two answers: which containers are up.
    #[derive(Default)]
    struct Fake {
        running: Vec<&'static str>,
    }

    impl Containers for Fake {
        fn running(&self, name: &str) -> bool {
            self.running.contains(&name)
        }
        fn image_exists(&self, _image: &str) -> bool {
            true
        }
        fn network_names(&self, _filter: &str, _bound: Duration) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }
        fn ensure_network(&self, _name: &str) -> Result<(), String> {
            Ok(())
        }
        fn replace(&self, _name: &str, _args: &[String], _bound: Duration) -> Result<(), String> {
            Ok(())
        }
    }

    /// A config whose data root is a fresh temp directory and whose daemon is one **this machine does not have**, so
    /// every test starts from "the daemon could not be asked" — the honest arm — unless it builds a socket.
    fn config(label: &str) -> (Config, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("holosmith-status-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the temp data root is creatable");
        let config = Config {
            role: Role::Server,
            data_root: root.clone(),
            control_listen: "127.0.0.1:1".parse().expect("a literal address"),
            docker_host: Some(format!("unix://{}", root.join("no-daemon.sock").display())),
            ..Config::default()
        };
        (config, root)
    }

    /// A daemon that answers `GET /_ping` for the rest of the test's life, at `path`.
    ///
    /// `tests/doctor.rs`'s helper, and here for the same reason: the daemon question is a real socket read, so the
    /// "it answered" arm has to be answered by something.
    fn fake_daemon(path: &PathBuf) {
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

    #[test]
    fn a_report_is_one_key_value_line_per_fact_in_order() {
        let (config, root) = config("order");
        let report = run(&config, &Fake::default());
        let keys: Vec<&str> = report.lines.iter().map(|line| line.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "role",
                "version",
                "uptime",
                "sessions starting",
                "sessions idle",
                "sessions running",
                "sessions stopped",
                "sessions error",
                "sessions deleting",
                "proxy hs_egress",
                "proxy hs_key",
                "machines",
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_three_arms_render_distinguishably() {
        assert_eq!(measured("role", "server").to_string(), "role server");
        assert_eq!(
            Line {
                key: "uptime".to_string(),
                value: Value::Unknown("nothing answers".to_string()),
            }
            .to_string(),
            "uptime unknown: nothing answers"
        );
        assert_eq!(
            Line {
                key: "sessions".to_string(),
                value: Value::Failed("the file is not readable".to_string()),
            }
            .to_string(),
            "sessions fail: the file is not readable"
        );
    }

    #[test]
    fn only_a_failed_read_changes_the_exit_code() {
        let unknown = Report {
            lines: vec![Line {
                key: "uptime".to_string(),
                value: Value::Unknown("nothing answers".to_string()),
            }],
        };
        assert!(!unknown.failed(), "an unknown is a verdict about measuring");
        let failed = Report {
            lines: vec![Line {
                key: "sessions".to_string(),
                value: Value::Failed("unreadable".to_string()),
            }],
        };
        assert!(failed.failed());
    }

    #[test]
    fn a_machine_that_never_served_has_zero_counts_and_an_unknown_uptime() {
        let (config, root) = config("never-served");
        let report = run(&config, &Fake::default());
        let rendered = report.render();
        assert!(
            rendered.contains("sessions idle 0\n"),
            "a missing database is zero rows, not a failure: {rendered}"
        );
        assert!(!report.failed(), "{rendered}");
        assert!(
            rendered.contains("uptime unknown: "),
            "nothing is serving, so the uptime line says so: {rendered}"
        );
        assert!(
            rendered.contains("proxy hs_egress unknown: whether it is running was not measured"),
            "a daemon nobody could reach is not a proxy that is down: {rendered}"
        );
        assert!(
            rendered.contains(&format!("machines unknown: {NO_REGISTRY}")),
            "the registry line names its story: {rendered}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_running_proxy_is_measured_from_the_daemons_answer() {
        let (mut config, root) = config("proxy");
        let socket = root.join("daemon.sock");
        fake_daemon(&socket);
        config.docker_host = Some(format!("unix://{}", socket.display()));
        let machine = Fake {
            running: vec![crate::container::PROXY_NAME],
        };
        let rendered = run(&config, &machine).render();
        assert!(rendered.contains("proxy hs_egress running\n"), "{rendered}");
        assert!(
            rendered.contains("proxy hs_key not running\n"),
            "{rendered}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_database_that_cannot_be_read_fails_the_sessions_reading_rather_than_printing_zeros() {
        // The one arm that changes the exit code: a file where the database should be, which SQLite will not open as
        // a database. Six zeros here would be a guess about a machine nobody could look at.
        let (config, root) = config("broken");
        std::fs::write(root.join(db::DATABASE_FILE), "this is not a database")
            .expect("the temp file is writable");
        let report = run(&config, &Fake::default());
        let rendered = report.render();
        assert!(
            rendered.contains("sessions fail: "),
            "an unreadable database is a failed read, not zero rows: {rendered}"
        );
        assert!(report.failed(), "{rendered}");
        assert!(
            !rendered.contains("sessions idle 0"),
            "the counts were printed over a read that failed: {rendered}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_counts_are_filled_from_the_status_table_rather_than_from_the_rows_found() {
        // The reader answers only the statuses that have rows; the report has all six lines. A status the table
        // gained and this enum did not would be a parse error in the reader, which is why the fill is over
        // `Status::ALL` rather than over the answer.
        let (config, root) = config("counts");
        let database = crate::db::Database::open(
            &root.join(db::DATABASE_FILE),
            crate::db::OpenOptions::default(),
            1,
        )
        .expect("a fresh database opens");
        let session = crate::sessions::NewSession {
            project: "holosmith".to_string(),
            model: "m".to_string(),
            thinking: "high".to_string(),
            base_commit: None,
            client_key: None,
            network: false,
        };
        database
            .sessions()
            .create(&session, 1)
            .expect("a row is written");
        drop(database);
        let rendered = run(&config, &Fake::default()).render();
        assert!(rendered.contains("sessions starting 1\n"), "{rendered}");
        assert!(rendered.contains("sessions idle 0\n"), "{rendered}");
        let _ = std::fs::remove_dir_all(root);
    }
}
