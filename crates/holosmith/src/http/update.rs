//! `POST /api/update` and `GET /api/update`: a promotion started from a request, and the last one read back
//! (ADR-0110).
//!
//! The app's Service screen needs two things the route table did not have: a way to **start** a job, and a way to see
//! how it is going **after the process that served the request has been replaced** — which a promotion does on
//! purpose, because its last step restarts the unit it runs under. So the two routes are one pair:
//!
//! - `start` refuses what it can refuse, starts [`crate::update::start`]'s transient unit, and answers
//!   `202 {job, since}`. It writes no file: the job owns its report, and a route that seeded one could claim a run it
//!   did not start.
//! - `last` answers the **report file and the log** and nothing else, which is why it keeps answering across the
//!   restart: both are under the data root, and neither is in this process's memory.
//!
//! # The POST's refusals, in the order they are asked
//!
//! 1. **the body** — `{commit}` and no other field, `422 bad_body` / `422 unknown_field` (the create's split);
//! 2. **the role** — a `hub` answers `403 update_disabled` with the role's own sentence. The route is **not** hidden
//!    behind the role the way the forward route is: a page asking a hub "may I promote?" has to be answered, and
//!    "there is no such endpoint" would leave it drawing a button against a `404`;
//! 3. **the switch** — `[update] enabled` is `false` by default, and a machine that has not turned it on answers
//!    `403 update_disabled` (ADR-0110 D1's consequence: an upgrade must not hand a phone a restart button);
//! 4. **the release tree** — a machine that never deployed has nothing to switch and no release the unit runs:
//!    `409 not_deployed`;
//! 5. **the candidate** — [`crate::candidate::resolve`]'s five `git` reads, off the request's task: no candidate at
//!    all is `404 no_candidate`, one sentence per shape, each naming the command that repairs it;
//! 6. **the token** — the request's `commit` against `master`'s now: `422 stale_candidate` naming both commits.
//!
//! Then, and only then, the unit starts. **No digest is echoed and no acknowledgement is taken**: the gate is the
//! acknowledgement (ADR-0110 D2), so a request carrying a digest would be a client vouching for bytes this machine
//! has not built yet, and the commit it does carry is an optimistic-concurrency token rather than a vouch.
//!
//! # What the GET answers, and where "running" comes from
//!
//! The report file is [`crate::pass::Report`]'s own JSON, written by `holosmith update --report <data>/update.json`,
//! and the `GET` relays that object — through the file's bytes, not through a second type — and adds the one fact the
//! report does not hold: the log's last lines. A route that re-typed the report would be a second spelling of one
//! run, and a report written by an older release's job would stop being readable at exactly the moment an operator is
//! reading it to find out what happened. The relay has one gap, and it lasts one promotion after an upgrade: `since`
//! is this release's field, so a report the **previous** release's job wrote carries none, and that `GET` answers a run
//! with no date until a job of this release writes the file again. A client renders such a run's start as unknown;
//! parsing the object into a type that could default the field would be the second spelling this paragraph refuses.
//!
//! **`running` is the job's own first write.** [`crate::pass::run`] writes the seed before its first step, so "a
//! promotion is under way, and it began at `since`" is a question this file answers as soon as a job of this release
//! has started. **Before that it does not, and the window is not milliseconds**: [`update::start`]'s `systemd-run`
//! returns when the unit is *queued* rather than when its pass begins, and the job then loads its config and resolves
//! its candidate — so between the `202` and the seed the `GET` answers the **previous** promotion's ending, or
//! [`ApiError::no_promotion`]'s `404` when this machine has never promoted. The client's rule closes it with the two
//! clocks this header distinguishes: a report whose `since` is **before** the `202`'s is not this run yet, so the page
//! keeps waiting rather than rendering the last run as this one.
//!
//! The cost beyond that window is named rather than hidden: a job that is killed leaves `running` in place, and it says
//! so until the next run writes its own seed. The alternative — asking `systemctl --user is-active` on a `GET` — would
//! be a child per poll and a second source of truth for a fact the file already holds, and a route-written seed would
//! be a claim about a start this process never saw complete.
//!
//! **`since` is written twice and means one thing.** The `202` carries the instant this machine accepted the start
//! (the route's clock, before the unit was asked for); the report carries the instant the job's own pass began, which
//! is later by however long the unit took to be queued and to start — the window above, and the reason the two values
//! are compared rather than assumed equal. A client renders the report's, from the `GET`; a caller that has not polled
//! yet has the `202`'s.
//!
//! # The one layer, and no rule of its own
//!
//! Both routes sit in [`crate::http::router`]'s one table, so ADR-0010's four header checks and the trust check apply
//! to them because `App::new` wraps the whole table and for no other reason — no hand-written guard, and no entry in
//! `is_guard_exempt`. The `POST` adds **no rule the merge route does not already have** (ADR-0110 D1): no login
//! header, no owner check beyond the configured one, no promoter credential. The authority is the tailnet.
//!
//! # What this module does not decide
//!
//! **The gate, the space it needs and the promotion.** [`crate::pass`] runs the gate and checks the candidate's
//! filesystem for room before it starts one; this module starts that command and reads what it wrote. **The page** is
//! another change's. So is refusing a candidate whose `Cargo.lock` the running release's image cannot build: today
//! the gate answers `3` and [`crate::pass::Verdict::StaleCrateSet`] turns it into the host act's sentence, and
//! nothing refuses it before the unit is spawned.

use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};

use crate::candidate;
use crate::clock;
use crate::config::Role;
use crate::deploy::{Root, Tools};
use crate::http::AppState;
use crate::http::error::{ApiError, internal};
use crate::update;

/// The file a job's report is written to, under the data root: `<data>/update.json`.
///
/// Beside [`update::LOG`] and named for the same reason: the two files are one job's whole record — one machine
/// readable, one human readable. The path is the **route's** to choose (nothing in [`crate::update`] or
/// [`crate::pass`] knows it) because it is what a `GET` reads after the process that started the job has been
/// replaced.
pub const REPORT: &str = "update.json";

/// How many of the log's last lines the `GET` answers.
///
/// The report's `detail` already carries the gate's last few lines for an ending; this is what a run **under way** has
/// to show, and a month of appended runs is not something a phone should pull to find the line it wants.
pub const LOG_LINES: usize = 100;

/// How far before the end of the log the `GET` reads to find those lines.
///
/// A window rather than the whole file: a promotion's gate prints for three quarters of an hour, so the log is
/// megabytes, and `append:` keeps every earlier run in it too.
const LOG_BYTES: u64 = 64 * 1024;

/// What both routes ask of the machine: its install tree, the data root it serves from, and the programs a start runs.
///
/// One value rather than three parameters, because three call sites build it — the two handlers and the tests — and
/// the report path, the log path and the job's argv are the three things that must not be spelled twice. It is built
/// in [`AppState::updater`], beside the state's other wiring, so the shape a test drives is the shape `serve` gives.
#[derive(Debug, Clone)]
pub struct Promotion {
    root: Root,
    data_root: PathBuf,
    tools: Tools,
}

impl Promotion {
    pub(crate) fn new(root: Root, data_root: PathBuf, tools: Tools) -> Self {
        Self {
            root,
            data_root,
            tools,
        }
    }

    /// `<data>/update.json`: the file a job's report is written to and the `GET` reads.
    pub fn report(&self) -> PathBuf {
        self.data_root.join(REPORT)
    }

    /// `<data>/update.log`: the job's output, which [`crate::update`] owns the name of.
    pub fn log(&self) -> PathBuf {
        update::log_path(&self.data_root)
    }

    /// `None` when this machine has a release tree, or the refusal a promotion cannot answer for.
    ///
    /// The tree's existence is the whole test, and [`Root::releases`] is where it would be: a deploy writes it (the
    /// release directory and `current` are the same step's two writes), and a promotion's first act is to switch
    /// `current` inside it. A machine without it has no unit running a release — so there is nothing this request
    /// could restart, and no previous release to put back when the candidate does not answer.
    fn not_deployed(&self) -> Option<ApiError> {
        let releases = self.root.releases();
        (!releases.is_dir()).then(|| ApiError::not_deployed(&releases))
    }

    /// Start one pass as the transient user unit, and say what became of the start.
    ///
    /// [`update::start`] blocks on two children, so its caller runs it on the blocking pool; this is the argument
    /// gathering only — the program, the machine's config file, the install root and the report path.
    fn start(&self, source: &Path) -> Result<update::Answer, String> {
        let exe = std::env::current_exe().map_err(|error| {
            format!(
                "this process cannot name its own binary ({error}), so it cannot start `holosmith update` as a \
                 unit: a job runs the binary `serve` is running, which on a deployed machine is the live release's"
            )
        })?;
        let command = self.command(&exe);
        update::start(
            &self.tools,
            &self.data_root,
            &update::Job {
                source,
                command: &command,
            },
        )
    }

    /// The job's command line: this binary's `update`, the machine's own config file and root, and the report path.
    ///
    /// `--config` and `--root` are passed explicitly rather than left to the environment and to `HOLOSMITH_ROOT`,
    /// because a job is a unit of the user manager: it inherits neither this process's flags nor its working
    /// directory, and a promotion that read a different config from the one `serve` is serving would build a
    /// candidate this machine never described.
    fn command(&self, exe: &Path) -> Vec<OsString> {
        let mut command = vec![exe.as_os_str().to_owned(), OsString::from("update")];
        let arguments: [(&str, PathBuf); 3] = [
            ("--config", self.root.config_file()),
            ("--root", self.root.path().to_path_buf()),
            ("--report", self.report()),
        ];
        for (flag, value) in arguments {
            command.push(OsString::from(flag));
            command.push(value.into_os_string());
        }
        command
    }

    /// The last job: the report file's own object, plus the log's last lines.
    ///
    /// The report is relayed as the JSON it is rather than parsed into a second type, and `log` is the one key added
    /// because it is the one fact the report cannot hold: its lines go to a file the job's output is appended to.
    fn last(&self) -> Result<Value, Reading> {
        let report = self.report();
        let text = match std::fs::read(&report) {
            Ok(text) => text,
            // No report is not a failure: no promotion has run since this data root was made, which is a machine
            // with nothing to answer rather than one that is broken.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Reading::Nothing);
            }
            Err(error) => return Err(Reading::Broken(read_failure(&report, &error))),
        };
        let Value::Object(mut answer) = serde_json::from_slice(&text)
            .map_err(|error| Reading::Broken(parse_failure(&report, &error)))?
        else {
            return Err(Reading::Broken(format!(
                "{} does not hold a JSON object, so it is not a report any run wrote",
                report.display()
            )));
        };
        let log = self.log();
        let lines = tail(&log).map_err(|error| Reading::Broken(read_failure(&log, &error)))?;
        // The path is rendered rather than embedded: `json!` converts through `serde_json::to_value(...).unwrap()`,
        // and `Path`'s `Serialize` errors on a data root that is not valid UTF-8 — which is a `--data-dir` this tree
        // tests and accepts. `read_failure` spells a path the same way.
        answer.insert(
            "log".to_string(),
            json!({ "path": log.display().to_string(), "lines": lines }),
        );
        Ok(Value::Object(answer))
    }
}

/// Answer `POST /api/update`: `202 {job, since}`, or one of the six refusals the module's header lists.
pub(crate) async fn start(state: &AppState, body: Result<Bytes, BytesRejection>) -> Response {
    let body = match body {
        Ok(body) => body,
        // Every rejection — a body hyper could not buffer, or one over axum's 2 MiB default — is this request's
        // `bad_body`, for the reason the create maps its own.
        Err(_) => return ApiError::bad_update_body().into_response(),
    };
    let commit = match commit_of(&body) {
        Ok(commit) => commit,
        Err(error) => return error.into_response(),
    };
    // The role first, for the create's reason: a machine that starts no promotion for itself makes the questions
    // after this unnecessary, and the client's next step is the sentence's (a server, through this machine's forward
    // route).
    if state.config.role == Role::Hub {
        return ApiError::hub_cannot_promote().into_response();
    }
    // Then the machine's switch. `false` by default, so a release that carries this route cannot hand a phone a
    // restart button by accident (ADR-0110 D1).
    if !state.config.update.enabled {
        return ApiError::update_disabled().into_response();
    }
    let promotion = state.updater();
    if let Some(refusal) = promotion.not_deployed() {
        return refusal.into_response();
    }
    // The candidate is five `git` children, so it goes to the blocking pool: a request path must not park a runtime
    // worker on one, and the config is cloned for it the way the host registry's probe clones it.
    let config = state.config.clone();
    let candidate = match tokio::task::spawn_blocking(move || candidate::resolve(&config)).await {
        Ok(Ok(candidate)) => candidate,
        Ok(Err(refusal)) => return ApiError::no_candidate(refusal.to_string()).into_response(),
        Err(error) => return internal("update candidate", error).into_response(),
    };
    // The token, not a vouch: the page rendered `master`'s commit and the request carried it back, so this refuses a
    // press about a tree that moved while the page was open.
    if let Some(stale) = candidate.stale(&commit) {
        return ApiError::stale_candidate(stale.to_string()).into_response();
    }
    // The acceptance's instant, taken before the unit is asked for: the `202` names when this machine took the
    // press, and the report names when the job's own pass began.
    let since = clock::unix_now();
    let source = candidate.source;
    let started = tokio::task::spawn_blocking(move || promotion.start(&source)).await;
    match started {
        Ok(Ok(update::Answer::Started { unit, .. })) => {
            (StatusCode::ACCEPTED, Json(Accepted { job: unit, since })).into_response()
        }
        Ok(Ok(update::Answer::AlreadyRunning { unit, log })) => {
            ApiError::update_running(unit, &log).into_response()
        }
        Ok(Err(said)) => {
            // The machine's failure and not the request's: no `systemd-run`, or a manager that could not answer. The
            // sentence names the child and what its absence means, and the log line keeps it beside the boot's.
            tracing::warn!(
                target: "holosmith::http",
                %said,
                "a promotion could not be started"
            );
            ApiError::update_not_started(said).into_response()
        }
        Err(error) => internal("update start", error).into_response(),
    }
}

/// Answer `GET /api/update`: the last job, or the envelope when there is none to read.
pub(crate) async fn last(state: &AppState) -> Response {
    match state.updater().last() {
        Ok(answer) => (StatusCode::OK, Json(answer)).into_response(),
        Err(Reading::Nothing) => ApiError::no_promotion().into_response(),
        // A file this machine wrote and cannot read back is a machine problem: the detail goes to the log, and the
        // client gets ADR-0008's one sentence.
        Err(Reading::Broken(said)) => internal("update report", said).into_response(),
    }
}

/// The `202`'s body: the unit systemd now holds, and the instant this machine accepted the press.
#[derive(Debug, Serialize)]
struct Accepted {
    /// [`update::UNIT_NAME`]: the transient unit's name, which is also the lock that makes two presses one promotion.
    job: &'static str,
    /// When this machine accepted the start, in unix seconds.
    ///
    /// The report's own `since` is when the job's pass began, a moment later; a client renders that one, from the
    /// `GET`.
    since: i64,
}

/// Why a `GET` has no report to answer: no job has run, or the machine's own file is not what a run writes.
#[derive(Debug)]
enum Reading {
    /// The report file is not there: no promotion has run since this machine's data root was made.
    Nothing,
    /// The report or the log could not be read as what a run writes.
    Broken(String),
}

/// The body's one field, and no other: `{commit}`.
const FIELDS: [&str; 1] = ["commit"];

/// Read the body as JSON and take its one field: the commit the page was rendered from.
///
/// A `commit` this build cannot compare is not the body's shape — comparing it is what
/// [`candidate::Candidate::stale`] does, and a request naming a commit `master` does not is the `422` that check
/// answers. What the shape refuses is a body with no `commit`, one that is not a string, and one that carries a field
/// this route does not read.
fn commit_of(bytes: &[u8]) -> Result<String, ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ApiError::bad_update_body())?;
    let object = value.as_object().ok_or_else(ApiError::bad_update_body)?;
    if object.keys().any(|field| !FIELDS.contains(&field.as_str())) {
        return Err(ApiError::unknown_field());
    }
    object
        .get("commit")
        .and_then(Value::as_str)
        .filter(|commit| !commit.is_empty())
        .map(str::to_string)
        .ok_or_else(ApiError::bad_update_body)
}

/// The last [`LOG_LINES`] lines of `path`, read from at most [`LOG_BYTES`] before its end.
///
/// A window read rather than the whole file, because the log is appended to across every run; the window's first
/// line is dropped when the read began inside it, since half a line is not one of the log's. A file that is not there
/// is no lines rather than a refusal: a job that was refused before it could write anything has an empty log, and a
/// report without one is still an answer.
fn tail(path: &Path) -> std::io::Result<Vec<String>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let length = file.metadata()?.len();
    let from = length.saturating_sub(LOG_BYTES);
    if from > 0 {
        file.seek(SeekFrom::Start(from))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    // Lossy rather than strict: a container's output is bytes, and one that is not UTF-8 must not make the whole log
    // unreadable — the rule `crate::pass` already keeps for the same stream.
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if from > 0
        && let Some(boundary) = text.find('\n')
    {
        text.drain(..=boundary);
    }
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if lines.last().is_some_and(String::is_empty) {
        // The file ends with a newline, so the last element is not a line.
        lines.pop();
    }
    if lines.len() > LOG_LINES {
        lines.drain(..lines.len() - LOG_LINES);
    }
    Ok(lines)
}

/// One sentence for a file of this machine's that could not be read.
fn read_failure(path: &Path, error: &std::io::Error) -> String {
    format!(
        "{} could not be read ({error}), so the last promotion on this machine cannot be answered from it",
        path.display()
    )
}

/// One sentence for a report that is not the JSON object a run writes.
fn parse_failure(path: &Path, error: &serde_json::Error) -> String {
    format!(
        "{} is not the JSON object a run writes there ({error}), so the last promotion on this machine cannot be \
         answered from it",
        path.display()
    )
}

/// Whether a job's command line names a word, for a test that asserts the argv without building one.
#[cfg(test)]
fn words(command: &[OsString]) -> Vec<String> {
    command
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// A promotion over a scratch root and data root, with the programs at a name no machine has: every test here
    /// reads or builds, and none of them starts a process.
    fn promotion(label: &str) -> (crate::sql::guard::TempDir, Promotion) {
        let directory = crate::sql::guard::TempDir::new(label);
        let root = Root::resolve(Some(directory.path().to_path_buf()), None);
        let promotion = Promotion::new(root, directory.path().join("data"), Tools::default());
        std::fs::create_dir_all(promotion.data_root.clone()).expect("a data root");
        (directory, promotion)
    }

    /// One JSON object as the file writes it, so an assertion reads the same bytes the route does.
    fn value(text: &str) -> Value {
        serde_json::from_str(text).expect("one JSON object")
    }

    /// The two files are the data root's, both of them: a route that wrote its report somewhere else would answer a
    /// `GET` from a different file than the job it started writes.
    #[test]
    fn the_report_and_the_log_are_the_data_roots_two_files() {
        let (_directory, promotion) = promotion("update-paths");
        assert_eq!(promotion.report(), promotion.data_root.join(REPORT));
        assert_eq!(
            promotion.log(),
            crate::update::log_path(&promotion.data_root),
            "the log the route reads is not the one the launcher appends to"
        );
        assert_eq!(
            promotion.log().file_name().and_then(OsStr::to_str),
            Some(crate::update::LOG)
        );
    }

    /// The job's argv: this binary's `update`, and then the machine's config file, its root and the report path —
    /// spelled with the state's values, because a unit inherits neither this process's flags nor its directory.
    #[test]
    fn the_job_is_asked_for_this_machines_config_root_and_report() {
        let (_directory, promotion) = promotion("update-argv");
        let exe = Path::new("/opt/holosmith/releases/v1/holosmith");
        assert_eq!(
            words(&promotion.command(exe)),
            vec![
                exe.display().to_string(),
                "update".to_string(),
                "--config".to_string(),
                promotion.root.config_file().display().to_string(),
                "--root".to_string(),
                promotion.root.path().display().to_string(),
                "--report".to_string(),
                promotion.report().display().to_string(),
            ],
            "the job's command line is not the one a promotion needs"
        );
    }

    /// No release tree is `409 not_deployed`, and one is not; the test's own root holds neither until it writes one.
    #[test]
    fn a_machine_with_no_release_tree_is_refused_before_anything_starts() {
        let (_directory, promotion) = promotion("update-deployed");
        let refusal = promotion
            .not_deployed()
            .expect("a root with no releases is not deployed");
        assert_eq!(refusal.status, StatusCode::CONFLICT);
        assert_eq!(refusal.body.code, crate::http::error::NOT_DEPLOYED);
        assert!(
            refusal
                .body
                .message
                .contains(&promotion.root.releases().display().to_string()),
            "the refusal does not name the tree it did not find: {}",
            refusal.body.message
        );
        assert!(
            refusal.body.message.contains("holosmith server deploy"),
            "the refusal does not name the repair: {}",
            refusal.body.message
        );

        std::fs::create_dir_all(promotion.root.releases()).expect("a release tree");
        assert!(promotion.not_deployed().is_none());
    }

    /// The `GET` relays the report's own object and adds the log's lines: the report's state and `since` are what a
    /// client switches on, and the added key is the one fact the report cannot hold.
    #[test]
    fn the_last_job_is_the_reports_object_plus_the_logs_lines() {
        let (_directory, promotion) = promotion("update-last");
        std::fs::write(
            promotion.report(),
            "{\"state\":\"running\",\"since\":1700000000,\"candidate\":null,\"gate\":null,\
             \"artifact\":null,\"steps\":[],\"detail\":null}\n",
        )
        .expect("a report");
        std::fs::write(
            promotion.log(),
            "update: candidate: abc at ded\nupdate: gate: started\n",
        )
        .expect("a log");

        let answer = promotion.last().expect("a report file is an answer");
        assert_eq!(answer["state"], "running");
        assert_eq!(answer["since"], 1_700_000_000u64);
        assert!(answer["candidate"].is_null());
        assert_eq!(answer["log"]["path"], promotion.log().display().to_string());
        assert_eq!(
            answer["log"]["lines"],
            value("[\"update: candidate: abc at ded\",\"update: gate: started\"]"),
            "the log's lines are not the file's"
        );
    }

    /// A machine that has run nothing answers nothing rather than an empty job; a report that is not a JSON object is
    /// the machine's own file being wrong, which is a different answer.
    #[test]
    fn a_missing_report_is_nothing_to_answer_and_a_broken_one_is_a_sentence() {
        let (_directory, promotion) = promotion("update-reading");
        assert!(matches!(promotion.last(), Err(Reading::Nothing)));

        std::fs::write(promotion.report(), "not json at all").expect("a broken report");
        match promotion.last() {
            Err(Reading::Broken(said)) => {
                assert!(
                    said.contains(&promotion.report().display().to_string()),
                    "the sentence does not name the file: {said}"
                );
            }
            _ => panic!("a file that is not a report is not nothing"),
        }

        // A JSON value that is not an object is the same answer: a run writes one object and nothing else.
        std::fs::write(promotion.report(), "[1, 2, 3]\n").expect("a list, not a report");
        assert!(matches!(promotion.last(), Err(Reading::Broken(_))));
    }

    /// The log's path is rendered rather than embedded: `json!` converts through `serde_json::to_value(...).unwrap()`,
    /// and a `Path` whose bytes are not valid UTF-8 is an `Err` there — a `--data-dir` this tree accepts and tests
    /// (`config`'s `a_data_dir_that_is_not_utf8_is_still_a_path`). A `GET` on such a machine answers; it does not
    /// panic the request.
    #[test]
    fn a_data_root_that_is_not_utf8_is_answered_rather_than_panicked_on() {
        use std::os::unix::ffi::OsStringExt;
        let directory = crate::sql::guard::TempDir::new("update-not-utf8");
        let data_root = directory
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'd', 0xff, b'x']));
        std::fs::create_dir_all(&data_root)
            .expect("a data root that is not UTF-8 is still a directory");
        let promotion = Promotion::new(
            Root::resolve(Some(directory.path().to_path_buf()), None),
            data_root,
            Tools::default(),
        );
        std::fs::write(
            promotion.report(),
            "{\"state\":\"refused\",\"since\":1700000000,\"candidate\":null,\"gate\":null,\
             \"artifact\":null,\"steps\":[],\"detail\":null}\n",
        )
        .expect("a report");

        let answer = promotion.last().expect("a report file is an answer");
        assert_eq!(answer["state"], "refused");
        assert!(
            answer["log"]["path"].is_string(),
            "the log's path is not a string: {}",
            answer["log"]["path"]
        );
    }

    /// The log's tail keeps the last lines and drops a line the window cut in half — and a log longer than the line
    /// cap answers the cap, because a phone asking how a run is going does not need its first hour.
    #[test]
    fn the_logs_tail_is_its_last_lines_and_never_half_of_one() {
        let directory = crate::sql::guard::TempDir::new("update-tail");
        let log = directory.path().join("update.log");

        assert!(
            tail(&log).expect("no log is no lines").is_empty(),
            "a log that is not there answered lines"
        );

        let many: String = (0..LOG_LINES + 10)
            .map(|index| format!("line {index}\n"))
            .collect();
        std::fs::write(&log, &many).expect("a log");
        let lines = tail(&log).expect("a log is readable");
        assert_eq!(lines.len(), LOG_LINES);
        assert_eq!(lines[0], format!("line {}", 10));
        assert_eq!(lines[LOG_LINES - 1], format!("line {}", LOG_LINES + 9));

        // A window that begins inside a line: the first entry is not a line of the log.
        let padding = "x".repeat(LOG_BYTES as usize);
        std::fs::write(&log, format!("{padding}\nafter the window\n")).expect("a wide log");
        assert_eq!(
            tail(&log).expect("a log is readable"),
            vec!["after the window".to_string()],
            "the window's first, half-read line was answered as one of the log's"
        );
    }

    /// The refusals' codes, as one table: a client switches on these, so the status and the word cannot drift from
    /// what the route's header says it answers.
    #[test]
    fn every_refusal_is_the_status_and_the_code_its_sentence_names() {
        use crate::http::error::{
            BAD_BODY, INTERNAL, NO_CANDIDATE, NOT_DEPLOYED, NOT_FOUND, STALE_CANDIDATE,
            UPDATE_DISABLED, UPDATE_RUNNING,
        };
        let cases = [
            (
                ApiError::bad_update_body(),
                StatusCode::UNPROCESSABLE_ENTITY,
                BAD_BODY,
            ),
            (
                ApiError::hub_cannot_promote(),
                StatusCode::FORBIDDEN,
                UPDATE_DISABLED,
            ),
            (
                ApiError::update_disabled(),
                StatusCode::FORBIDDEN,
                UPDATE_DISABLED,
            ),
            (
                ApiError::not_deployed(Path::new("/root/releases")),
                StatusCode::CONFLICT,
                NOT_DEPLOYED,
            ),
            (
                ApiError::no_candidate("no candidate"),
                StatusCode::NOT_FOUND,
                NO_CANDIDATE,
            ),
            (
                ApiError::stale_candidate("stale"),
                StatusCode::UNPROCESSABLE_ENTITY,
                STALE_CANDIDATE,
            ),
            (
                ApiError::update_running("holosmith-update", Path::new("/data/update.log")),
                StatusCode::CONFLICT,
                UPDATE_RUNNING,
            ),
            (ApiError::no_promotion(), StatusCode::NOT_FOUND, NOT_FOUND),
            (
                ApiError::update_not_started("no systemd-run"),
                StatusCode::INTERNAL_SERVER_ERROR,
                INTERNAL,
            ),
        ];
        for (error, status, code) in cases {
            assert_eq!(error.status, status, "{code}");
            assert_eq!(error.body.code, code);
        }

        // Two codes carry two sentences, and the sentence is the only thing that differs.
        assert_ne!(
            ApiError::update_disabled().body.message,
            ApiError::hub_cannot_promote().body.message,
            "the config's refusal and the role's refusal share one sentence"
        );
        assert!(
            ApiError::hub_cannot_promote().body.message.contains("hub"),
            "the role's refusal does not say whose fact it is"
        );
        // A `409 update_running` is what a client acts on, so it names the unit and the file it can read.
        let running = ApiError::update_running("holosmith-update", Path::new("/data/update.log"));
        assert!(running.body.message.contains("holosmith-update"));
        assert!(running.body.message.contains("/data/update.log"));
    }

    /// The body's shape: `commit` and nothing else. A body that is not an object, one with no `commit`, an empty one,
    /// and one with a second field are four different mistakes with the two codes the create's body already uses.
    #[test]
    fn the_body_is_one_commit_and_no_other_field() {
        use crate::http::error::{BAD_BODY, UNKNOWN_FIELD};
        assert_eq!(
            commit_of(br#"{"commit":"0f1e2d"}"#).expect("one commit is the body"),
            "0f1e2d"
        );
        for (body, code) in [
            ("not json", BAD_BODY),
            ("[]", BAD_BODY),
            ("{}", BAD_BODY),
            (r#"{"commit":""}"#, BAD_BODY),
            (r#"{"commit":7}"#, BAD_BODY),
            (r#"{"commit":null}"#, BAD_BODY),
            (r#"{"commit":"0f1e2d","digest":"abc"}"#, UNKNOWN_FIELD),
        ] {
            let error = commit_of(body.as_bytes()).expect_err("the body is refused");
            assert_eq!(error.body.code, code, "{body}");
        }
        assert!(
            !ApiError::bad_update_body().body.message.contains("digest"),
            "the sentence offers a field the request must not carry"
        );
    }
}
