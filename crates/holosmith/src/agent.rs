//! The agent process: pi in RPC mode over a container's stdio (HS-031).
//!
//! Spec item 2 launches one agent per session as `pi --mode rpc` inside the session's container, and item
//! 6's run line is `docker run -i …` — attached, with the container's stdin and stdout on the server's side
//! of the pipe. This module is that pipe as one value: an [`Agent`] is a spawned `docker run` whose stdout
//! is JSONL, whose stdin takes JSONL commands, and whose exit code and stderr are kept until somebody asks.
//!
//! **It holds no database, no events and no config.** Writing a row is the server's (ADR-0019), so the exit
//! goes out through a [`oneshot`] the caller awaits and records with [`crate::exit::Exits`]; this module
//! only knows that the process ended and what it printed.
//!
//! **Framing is strict JSONL split on LF only** (`docs/rpc.md`): one complete JSON object per record, `\n`
//! as the only separator, an optional `\r` before it stripped. A generic line reader is wrong here — Node's
//! `readline` also splits on `U+2028`/`U+2029`, which are legal inside a JSON string — so the reader uses
//! [`tokio::io::AsyncBufReadExt::read_until`] on the byte `b'\n'` and nothing else.
//!
//! **"Ready" is a protocol fact, not a sleep.** The handshake is a `get_state` command with an `id`, and the
//! answer is the `response` record that repeats that `id` (`docs/rpc.md`, "Correlate commands and
//! responses"). A process that never answers is a failed launch after [`READY_TIMEOUT`], and one that
//! answers `success: false` says what pi refused.
//!
//! **A record that is not a `response` belongs to the turn.** Session events, extension UI records and pi's own
//! bookkeeping are published to whoever is running the session's turn ([`Slots`], [`Received`]) while `response`
//! records are routed to the command waiting for them; only a line that is not JSON at all is counted. A turn
//! subscribes *before* it writes its command, and the channel is bounded so a slow writer slows pi rather than
//! losing text (ADR-0040).
//!
//! **stderr is a pipe that must be drained.** A client that stops reading stalls the process (`docs/rpc.md`:
//! "Read stdout continuously"), so both pipes have a reader from the moment the process starts. stderr is
//! kept as a bounded tail — [`STDERR_TAIL`] bytes — because the whole of it can be a build log, and the two
//! readers of it (the row's reason and `session.log`) are both about the end.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

use crate::docker::Docker;
use crate::sessions::Session;

/// The directory under `<data>/sessions/<id>` that pi calls `PI_CODING_AGENT_DIR` (spec item 2).
pub const HOME: &str = "pi-home";

/// The file the seed is written to, and the only file this module writes into the agent directory.
pub const SETTINGS: &str = "settings.json";

/// The seed: a template with model preferences and **no secrets** (the story's first acceptance line).
///
/// Its two keys are pi's own (`docs/settings.md`): `defaultModel` and `defaultThinkingLevel`, which are what
/// a process started *without* `--model`/`--thinking` would use. The launch passes those two flags as well
/// (see `container::SessionContainer::run_args`), so the file is the session's durable configuration and the
/// flags are how this build makes pi honour it with `PI_OFFLINE=1` — measured: the file alone leaves
/// `get_state` at `model.id: "unknown"`.
///
/// Embedded rather than read from disk, because the binary is the product (item 10 §1): a deployment has
/// no `templates/` next to the executable. The two fields are patched per session from the row
/// ([`seed`]); the key proxy's `models.json` is a second file written beside this one by the Docker
/// backend (HS-039), because it carries the session's dummy key and is therefore not part of this
/// secret-free template.
pub const TEMPLATE: &str = include_str!("../templates/pi-home/settings.json");

/// The launch environment's four fixed variables, and why each is one.
///
/// `PI_OFFLINE=1` keeps pi from refreshing anything (item 2's D1 text), `PI_SKIP_VERSION_CHECK=1` stops its
/// `pi.dev` request, `PI_TELEMETRY=0` stops install/update telemetry, and `NO_PROXY=key-proxy` makes the
/// provider calls through the key proxy skip the egress proxy that is not there yet (item 2 §3, HS-037).
/// The fifth variable is the agent directory, which is a path and so is not in this table.
pub const ENVIRONMENT: [(&str, &str); 4] = [
    ("NO_PROXY", "key-proxy"),
    ("PI_OFFLINE", "1"),
    ("PI_SKIP_VERSION_CHECK", "1"),
    ("PI_TELEMETRY", "0"),
];

/// How long a launch may take to answer the ready handshake.
///
/// Thirty seconds is generous for a process that has already started: pi answers `get_state` from its own
/// state, so the time is process start plus reading one file. A bound rather than nothing, because an
/// agent that never answers must fail its session with a sentence instead of leaving it `starting`
/// forever.
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one line that is not a command may take to reach the agent's stdin.
///
/// The RPC extension UI protocol is the only writer besides [`Agent::request`]: a dialog the client answers
/// with an `extension_ui_response` (HS-035). It is a small write, so the bound is short, and it lives here
/// rather than being a caller's argument because a bound somebody has to remember to pass is a bound somebody
/// will forget — and the thing it protects against is a process that stopped reading its stdin, which would
/// otherwise pin the agent's stdin mutex for the session's life.
pub const RESPOND_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of stderr is kept for the row's reason and `session.log`.
///
/// A tail, not a cap on what is read: the pipe is drained to the end either way (a pipe nobody reads stalls
/// the process), and only the last 64 KiB is remembered. That is more than the five lines a reason uses and
/// enough of a crash's tail to diagnose it.
pub const STDERR_TAIL: usize = 64 * 1024;

/// The `--version` argument pi answers with the release it is (`pins/pi.txt` is the other side).
pub const VERSION_ARGUMENT: &str = "--version";

/// The command a container runs: pi in RPC mode, no session file, project trust approved.
///
/// `--no-session` because a session's own state is Holosmith's event log, not pi's JSONL file (spec item 5).
/// `--approve` is open question 9's answer: per process, visible in the launch line, and never a setting in
/// the seed — ADR-0039's "Project trust is `--approve`" says why.
pub const COMMAND: [&str; 5] = ["pi", "--mode", "rpc", "--no-session", "--approve"];

/// `<data>/sessions/<id>/pi-home`: the directory pi reads its configuration from (spec item 2).
pub fn home(data_root: &Path, session_id: &str) -> PathBuf {
    crate::exit::session_dir(data_root, session_id).join(HOME)
}

/// Make sure `home` exists and holds a seed, and answer what was done.
///
/// **Seed only what is not there.** A resume finds the directory the previous container left — with the
/// files pi itself wrote into it (`auth.json`, `models-store.json`) — and overwriting them would be this
/// module deciding that the agent's own state is disposable. `models.json` is the exception, and it is
/// written by the backend rather than here: it carries the session's dummy key, which must follow the key
/// store on a resume (HS-039). The row cannot change a session's model or
/// thinking after the create (no endpoint does), so a settings file that is already there is still the
/// right one, and one that is not is written from [`TEMPLATE`] with the row's two preferences.
///
/// A failure is returned rather than logged: the container's mount of this directory is the next thing that
/// needs it, and `Mounts::check` would report the missing source as a container problem rather than the
/// filesystem problem it is.
pub fn ensure_home(home: &Path, session: &Session) -> io::Result<Created> {
    std::fs::create_dir_all(home)?;
    let settings = home.join(SETTINGS);
    if settings.exists() {
        return Ok(Created::AlreadyThere);
    }
    std::fs::write(&settings, seed(session))?;
    Ok(Created::Seeded)
}

/// What [`ensure_home`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    /// The directory was there with its seed: a resume, or a second container for one session.
    AlreadyThere,
    /// The directory and its seed were written.
    Seeded,
}

/// The seed for `session`: the template, with the row's model and thinking level.
///
/// Built as JSON rather than by formatting the template, so a model id with a quote in it cannot make the
/// file unreadable. A template that does not parse is a bug in the embedded file, not a runtime condition,
/// so the fallback is the two keys and nothing else — the values are what the row asked for either way.
pub fn seed(session: &Session) -> String {
    let mut document: Value = serde_json::from_str(TEMPLATE).unwrap_or_else(|_| json!({}));
    if let Some(object) = document.as_object_mut() {
        object.insert("defaultModel".to_string(), json!(session.model));
        object.insert("defaultThinkingLevel".to_string(), json!(session.thinking));
    }
    let mut text = serde_json::to_string_pretty(&document).unwrap_or_default();
    text.push('\n');
    text
}

/// The launch line `session.log` records, built from an allow-list of fields.
///
/// **Nothing here is scrubbed, because nothing here can be a secret**: the line is assembled from the
/// fields below and no others, so no environment value, no key and no command line the operator did not
/// choose can reach it. That is the honest way to promise "redacted" — a filter over arbitrary text is a
/// filter that misses the next secret somebody adds.
///
/// It answers "which command did we run, as whom, with which versions" — the first question a failed
/// launch asks — and it is written *before* the process starts, so a launch that fails has one too.
pub fn launch_line(image: &str, version: &str, session: &Session, home: &Path) -> String {
    format!(
        "launched {image} (pi {version}) for session {id}: {} in {} — model {model}, thinking {thinking}, \
         project trust approved",
        COMMAND.join(" "),
        home.display(),
        id = session.id,
        model = session.model,
        thinking = session.thinking,
    )
}

/// How a launched agent's process ended, as the process itself reported it.
///
/// Not to be confused with [`crate::exit::Exit`], which is what an exit *means* for the row: this is the
/// code and the tail of stderr, and that is the classifier's three answers over them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    /// The container's exit code, as `docker run -i` reports it: the CLI's own code *is* the container's
    /// (`--detach` is what would hide it). A process killed by a signal answers `128 + the signal`.
    pub code: i32,
    /// The tail of what it printed on stderr, at most [`STDERR_TAIL`] bytes.
    pub stderr: String,
}

/// One answer to one command: the `response` record that repeats the command's `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// Whether pi handled the command. `success: false` is a refusal, and `error` says why.
    pub success: bool,
    pub error: Option<String>,
    /// The command's data, for the caller that asked for it.
    pub data: Option<Value>,
}

/// Why a command did not get an answer.
#[derive(Debug)]
pub enum AgentError {
    /// The command could not be written: the process is gone.
    Write(io::Error),
    /// The agent answered `success: false`.
    Refused(String),
    /// The answer did not arrive in time.
    Timeout(Duration),
    /// The process ended (or its stdout closed) before answering.
    Closed,
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(error) => write!(f, "the agent's stdin could not be written ({error})"),
            Self::Refused(reason) => write!(f, "pi refused the command: {reason}"),
            Self::Timeout(duration) => write!(
                f,
                "pi did not answer within {} seconds",
                duration.as_secs_f32()
            ),
            Self::Closed => f.write_str("pi closed its output before answering"),
        }
    }
}

impl std::error::Error for AgentError {}

/// How many records may be waiting for a turn before the reader waits for the turn to catch up.
///
/// A streaming turn prints a record per token and the pump appends each frame through the writer's own bounded
/// queue (ADR-0013, and `EventWriter::append` awaits), so the two paces can differ. This is the slack between
/// them; when it fills, the reader **waits** — which stops it draining pi's stdout, which blocks pi on the pipe
/// — instead of dropping a record. Backpressure all the way to the agent is what makes the story's "every delta
/// reaches the writer" true by construction rather than by hope (ADR-0040).
pub const RECORDS_BUFFER: usize = 256;

/// One thing a subscriber for a turn can learn from the agent's stdout.
#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    /// One record pi printed that was not a `response`: a session event, an extension UI request, or pi's own
    /// bookkeeping. [`crate::rpc::Mapping`] decides what it means.
    Record(Value),
    /// The process is gone: nothing more will arrive, and every later call answers this too.
    Closed,
}

/// A turn's subscription to the records the agent's stdout reader publishes.
///
/// Taken **before** the prompt is written, because a record printed between the write and the subscription
/// would otherwise be lost — and the first records of a turn arrive immediately.
pub struct AgentRecords {
    receiver: tokio::sync::mpsc::Receiver<Value>,
    /// Set once the channel has ended, so every later call answers the same thing instead of waiting on a
    /// channel nothing will write to again.
    done: bool,
}

impl std::fmt::Debug for AgentRecords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AgentRecords")
    }
}

impl AgentRecords {
    /// The next record, or that the process is gone.
    ///
    /// A closed channel is the reader's own last word: it takes the sender out of the agent's slot as it ends
    /// ([`Slots::close`]), because the [`Agent`] value the backend keeps in its registry would otherwise keep the
    /// channel open for as long as the session exists — and a turn that waited for that would wait for its
    /// session to be deleted.
    pub async fn recv(&mut self) -> Received {
        if self.done {
            return Received::Closed;
        }
        match self.receiver.recv().await {
            Some(record) => Received::Record(record),
            None => {
                self.done = true;
                Received::Closed
            }
        }
    }
}

/// Where the reader publishes and where a turn subscribes, which is the one place a session's records cross.
///
/// A `std` mutex over an `Option<mpsc::Sender<Value>>`, never held across an `await`: the reader clones the sender
/// out, drops the guard and then awaits the send. **One subscriber is the design** — a session runs one turn at a
/// time — so a new subscription replaces the previous sender rather than fanning out: a fan-out would have to
/// pick a slow consumer to punish, and a single receiver is what lets the reader *wait* instead of dropping.
#[derive(Default)]
struct Slots {
    current: Mutex<Option<tokio::sync::mpsc::Sender<Value>>>,
}

impl Slots {
    /// Install a fresh channel for a turn and answer the receiver it reads.
    fn subscribe(&self) -> AgentRecords {
        let (sender, receiver) = tokio::sync::mpsc::channel(RECORDS_BUFFER);
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        // A previous turn's sender is dropped here, which ends its channel: a turn that has returned is not
        // waiting for records, and keeping two senders alive would mean choosing between them.
        *current = Some(sender);
        AgentRecords {
            receiver,
            done: false,
        }
    }

    /// The sender a record should go to, if a turn is listening.
    fn sender(&self) -> Option<tokio::sync::mpsc::Sender<Value>> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// End the channel: the reader is done, so the turn's `recv` answers [`Received::Closed`].
    ///
    /// Called from the reader's own end, so the answer is about the **process** rather than about the registry
    /// entry the backend keeps for the session.
    fn close(&self) {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        *current = None;
    }
}

/// A launched agent: its stdin and the answers awaiting it.
///
/// The process itself belongs to the task [`Agent::launch`] spawns, because a `Child` has to be waited on
/// exactly once and the value that does it must be the one that also collects stderr and hands the exit to
/// the server. That is why the exit's receiver comes back from [`Agent::launch`] beside the agent rather
/// than out of it: the process is the task's, and the report is the caller's.
pub struct Agent {
    stdin: AsyncMutex<tokio::process::ChildStdin>,
    /// Command id → the caller waiting for its answer. A `std` mutex, never held across an `await`: it is
    /// touched for one insert and one removal.
    waiting: Arc<Mutex<HashMap<String, oneshot::Sender<Reply>>>>,
    /// Where the stdout reader publishes the records it does not route as a `response` (HS-032).
    records: Arc<Slots>,
    session_id: String,
}

impl fmt::Debug for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agent")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// Spawn `command` and start reading its stdout and stderr.
    ///
    /// The two readers start here rather than when somebody first asks, because both pipes block the child
    /// when they fill. `kill_on_drop` is set: the process is a container's stdio, and a `docker run` left
    /// behind by a dropped task would keep a container alive with nobody to talk to it. Closing its stdin
    /// is what pi treats as an orderly shutdown (`docs/rpc.md`).
    ///
    /// The answer is the agent and the receiver its exit will arrive on. The exit is sent **after** every
    /// record has been routed, so a caller cannot be told the process ended before it read the answer it was
    /// waiting for.
    pub fn launch(
        command: std::process::Command,
        session_id: &str,
    ) -> io::Result<(Self, oneshot::Receiver<Ended>)> {
        let mut command = tokio::process::Command::from(command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("the launched process has no stdin to write to"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("the launched process has no stdout to read"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("the launched process has no stderr to read"))?;

        let waiting: Arc<Mutex<HashMap<String, oneshot::Sender<Reply>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let records = Arc::new(Slots::default());
        let (exit, exit_rx) = oneshot::channel();
        let session = session_id.to_string();
        let reader_waiting = Arc::clone(&waiting);
        let reader_records = records.clone();
        let reader_session = session.clone();
        tokio::spawn(async move {
            let reader = tokio::spawn(read_records(
                stdout,
                reader_waiting,
                reader_records,
                reader_session,
            ));
            let errors = tokio::spawn(read_stderr(stderr));
            let code = match child.wait().await {
                Ok(status) => exit_code(&status),
                Err(error) => {
                    tracing::warn!(
                        target: "holosmith::agent",
                        session = %session,
                        %error,
                        "the agent's process could not be waited on"
                    );
                    -1
                }
            };
            // stderr first: the record of how it ended is the reason it is kept, and the pipe closes when
            // the process is gone (so this cannot wait on the process's own lifetime).
            let stderr = errors.await.unwrap_or_default();
            // The stdout reader ends when the pipe closes; awaiting it here means the exit report is sent
            // after every record has been routed, so a caller cannot see an exit before its last answer.
            let _ = reader.await;
            if exit.send(Ended { code, stderr }).is_err() {
                tracing::debug!(
                    target: "holosmith::agent",
                    session = %session,
                    code,
                    "the agent exited and nobody was waiting to record it"
                );
            }
        });

        Ok((
            Self {
                stdin: AsyncMutex::new(stdin),
                waiting,
                records,
                session_id: session_id.to_string(),
            },
            exit_rx,
        ))
    }

    /// Subscribe to the records this agent prints from now on (HS-032).
    ///
    /// Taken before a command is written, so the records that command causes cannot arrive before there is
    /// somebody to receive them. Every call is an independent subscription: a turn's own.
    pub fn records(&self) -> AgentRecords {
        self.records.subscribe()
    }

    /// Send one command and await the response that repeats its `id`.
    ///
    /// The waiter is registered *before* the bytes are written, because pi answers asynchronously and a
    /// fast answer must not be lost between the write and the registration (`docs/rpc.md`: "command handling
    /// is asynchronous, so clients should correlate by ID"). The bound is the caller's, because a handshake
    /// and a future turn's command do not deserve the same one.
    pub async fn request(
        &self,
        id: &str,
        command: Value,
        timeout: Duration,
    ) -> Result<Reply, AgentError> {
        let (sender, receiver) = oneshot::channel();
        {
            let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
            waiting.insert(id.to_string(), sender);
        }
        // The write is bounded as well as the answer (`docs/rpc.md`: honour stdin backpressure). A process
        // that stops reading its stdin would otherwise hold the mutex for ever, and the bound would never
        // start: the handshake's payload is tiny, but HS-032's commands are not, and a session stuck in
        // `starting` because a `write_all` blocked is exactly the hang the bound exists to prevent.
        if let Err(error) = self.write_line(&command, timeout).await {
            self.forget(id);
            return Err(error);
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(reply)) => Ok(reply),
            // The sender was dropped: the reader ended, so the process is gone.
            Ok(Err(_)) => {
                self.forget(id);
                Err(AgentError::Closed)
            }
            Err(_) => {
                self.forget(id);
                Err(AgentError::Timeout(timeout))
            }
        }
    }

    /// Drop `id`'s waiter, after a write failed or a bound expired.
    fn forget(&self, id: &str) {
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        waiting.remove(id);
    }

    /// Write one JSONL line to the agent's stdin, for the sub-protocol that is not a command (HS-035).
    ///
    /// An extension's dialog is answered this way: pi is **blocked** until the `extension_ui_response` with the
    /// request's id arrives (`docs/rpc-extension-ui.md`), so the line is not a request that something answers —
    /// it is the answer. There is no reply to correlate, so no waiter is registered, and the bound is
    /// [`RESPOND_TIMEOUT`] because the payload is small and its only failure mode is a process that stopped
    /// reading.
    pub async fn respond(&self, line: Value) -> Result<(), AgentError> {
        self.write_line(&line, RESPOND_TIMEOUT).await
    }

    /// One JSONL line on the agent's stdin, under `bound`.
    ///
    /// The one spelling of the write, shared by [`Agent::request`] and [`Agent::respond`] because the two
    /// differ only in what they do about the answer: the first waits for it, the second has none to wait for.
    /// The bound covers the write as well as the lock it takes, and closing the gate on a process that stopped
    /// reading is what keeps a session from parking a worker for ever.
    async fn write_line(&self, line: &Value, bound: Duration) -> Result<(), AgentError> {
        let mut bytes = serde_json::to_vec(line).unwrap_or_default();
        bytes.push(b'\n');
        let written = tokio::time::timeout(bound, async {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(&bytes).await?;
            stdin.flush().await
        })
        .await;
        match written {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(AgentError::Write(error)),
            Err(_) => Err(AgentError::Timeout(bound)),
        }
    }

    /// The ready handshake: ask pi for its state and accept only a real answer.
    ///
    /// `get_state` rather than a bare "hello", because it is a documented command whose answer carries the
    /// session id and the model — so a `success: true` proves the loop is running and the runtime has
    /// loaded, which is what "ready" has to mean for the next step to be a prompt (HS-032).
    pub async fn ready(&self) -> Result<Reply, AgentError> {
        let reply = self
            .request(
                READY_COMMAND,
                json!({"id": READY_COMMAND, "type": "get_state"}),
                READY_TIMEOUT,
            )
            .await?;
        if reply.success {
            Ok(reply)
        } else {
            Err(AgentError::Refused(
                reply.error.unwrap_or_else(|| "no reason given".to_string()),
            ))
        }
    }
}

/// The id of the ready handshake's command. Constant, because there is never more than one per agent.
pub const READY_COMMAND: &str = "ready-1";

/// The exit code `docker run -i` reports, with a signal's spelled the way a shell does.
fn exit_code(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(9))
}

/// Read one agent's stdout as JSONL, and route every `response` to the caller waiting for it.
///
/// Splitting is [`AsyncBufReadExt::read_until`] on `b'\n'`, which is the only separator the protocol has;
/// one trailing `\r` is stripped so a CRLF peer is still readable (`docs/rpc.md`). Two destinations: a
/// `response` goes to the command waiting on its id, and every other record is **published** to the session's
/// turn ([`Slots`]), which is what lets `crate::rpc::Mapping` decide what it means (HS-032) and what will let
/// HS-035 answer an extension's UI request. Only a line that is not JSON, or a `response` nobody is waiting for,
/// is counted and dropped — never fatal: a reader that dies on a frame it does not understand would take the
/// session with it.
async fn read_records(
    stdout: tokio::process::ChildStdout,
    waiting: Arc<Mutex<HashMap<String, oneshot::Sender<Reply>>>>,
    records: Arc<Slots>,
    session_id: String,
) {
    let mut reader = BufReader::new(stdout);
    let mut line: Vec<u8> = Vec::new();
    let mut skipped = 0_u64;
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    target: "holosmith::agent",
                    session = %session_id,
                    %error,
                    "the agent's output could not be read"
                );
                break;
            }
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            continue;
        }
        let record: Value = match serde_json::from_slice(&line) {
            Ok(record) => record,
            Err(error) => {
                skipped += 1;
                tracing::debug!(
                    target: "holosmith::agent",
                    session = %session_id,
                    %error,
                    line = %String::from_utf8_lossy(&line[..line.len().min(120)]),
                    "a record on the agent's output was not JSON"
                );
                continue;
            }
        };
        if record.get("type").and_then(Value::as_str) != Some("response") {
            // Session events, extension UI records and pi's own bookkeeping: published for the turn's mapping
            // (HS-032) and for the extension UI requests (HS-035). `send` fails when no turn is listening,
            // which is not an error — a steer's records and a settle after the pump has returned both land
            // there.
            if let Some(sender) = records.sender() {
                // Awaited, so a turn that is behind slows the reader instead of losing text (see RECORDS_BUFFER).
                // A failed send means the turn is gone, which is not an error: the next turn subscribes again.
                let _ = sender.send(record).await;
            }
            continue;
        }
        let Some(id) = record.get("id").and_then(Value::as_str) else {
            skipped += 1;
            continue;
        };
        let reply = Reply {
            success: record
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            error: record
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
            data: record.get("data").cloned(),
        };
        let sender = {
            let mut waiting = waiting.lock().unwrap_or_else(PoisonError::into_inner);
            waiting.remove(id)
        };
        match sender {
            Some(sender) => {
                // A receiver that has gone away (a timed-out handshake) is not an error worth a line.
                let _ = sender.send(reply);
            }
            None => {
                skipped += 1;
                tracing::debug!(
                    target: "holosmith::agent",
                    session = %session_id,
                    %id,
                    "a response named a command nobody is waiting for"
                );
            }
        }
    }
    if skipped > 0 {
        tracing::debug!(
            target: "holosmith::agent",
            session = %session_id,
            skipped,
            "records on the agent's output could not be read"
        );
    }
    // The last word: a turn subscribed to this agent learns that nothing more will arrive. Taken out of the slot
    // rather than dropped, because the `Agent` value keeps the slot for as long as the backend's registry holds
    // it, and a channel that stayed open would leave the turn waiting for a process that is gone.
    records.close();
    // The map's senders are dropped with this task, so every waiter learns the process is gone.
}

/// Drain stderr to the end, keeping the last [`STDERR_TAIL`] bytes.
///
/// Draining is the point — a full pipe stalls the process — and the tail is what the row's reason and
/// `session.log` want. A chunk that would overflow the tail drops from the front, which can cut a line in
/// half; the log is a tail of a stream, and `Exits`'s reason trims what it takes.
async fn read_stderr(stderr: tokio::process::ChildStderr) -> String {
    let mut reader = BufReader::new(stderr);
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = vec![0_u8; 8 * 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => {
                kept.extend_from_slice(&chunk[..read]);
                if kept.len() > STDERR_TAIL {
                    let overflow = kept.len() - STDERR_TAIL;
                    kept.drain(..overflow);
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// The version the agent image runs, asked of the image itself.
///
/// `docker run --rm --network none --entrypoint pi <image> --version`: a container of its own with no
/// network, so the question costs a second and cannot reach anything. The answer is trimmed because
/// `--version` ends its line; what it printed is otherwise passed through untouched so a mismatch's sentence
/// can quote it.
pub fn version_of(docker: &Docker, image: &str) -> Result<String, String> {
    let arguments = [
        "run",
        "--rm",
        "--network",
        "none",
        "--entrypoint",
        "pi",
        image,
        VERSION_ARGUMENT,
    ];
    match docker.run(&arguments) {
        Ok(stdout) => Ok(stdout.trim().to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// Whether the pi an image runs is the one this binary is built for, and the sentence when it is not.
///
/// One function so both arms are testable without a second image: `Ok` carries the version (what the launch
/// line records) and `Err` the sentence the session fails with.
pub fn version_check(image: &str, found: &str) -> Result<String, String> {
    let pinned = crate::pins::pi_version();
    if found == pinned {
        Ok(found.to_string())
    } else {
        Err(version_sentence(image, found, pinned))
    }
}

/// The sentence a mismatched agent image fails its session with.
///
/// Three facts and the fix: which image, which pi it runs, which pi this binary is built for. The row's
/// `status_reason` is where an operator reads it, so it names the image by the name the config gave it.
pub fn version_sentence(image: &str, found: &str, pinned: &str) -> String {
    format!(
        "the agent image {image} runs pi {found}, but this binary is built for pi {pinned}: rebuild the \
         agent image from this release (agent_image names it)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Status;

    fn session() -> Session {
        Session {
            id: "0123456789abcdef".to_string(),
            project: "holosmith".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            thinking: "high".to_string(),
            worktree: String::new(),
            base_commit: None,
            status: Status::Starting,
            status_reason: None,
            client_key: None,
            network: false,
            created_at: 1_760_000_000,
            last_active_at: 1_760_000_000,
            stopped_at: None,
            usage_in: 0,
            usage_out: 0,
        }
    }

    fn scratch(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("holosmith-agent-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the scratch directory is creatable");
        path
    }

    #[test]
    fn the_agent_directory_is_the_sessions_own_under_the_data_root() {
        assert_eq!(
            home(Path::new("/var/lib/holosmith"), "0123456789abcdef"),
            Path::new("/var/lib/holosmith/sessions/0123456789abcdef/pi-home")
        );
    }

    #[test]
    fn the_template_parses_and_carries_no_secret() {
        // The story's first acceptance line: "seeded from a template with model preferences and no
        // secrets". A template that grew an `apiKey` would put a credential in every session's directory,
        // which item 2 forbids in three places.
        let document: Value = serde_json::from_str(TEMPLATE).expect("the template is JSON");
        let object = document.as_object().expect("the template is an object");
        for key in ["defaultModel", "defaultThinkingLevel"] {
            assert!(object.contains_key(key), "the template has no {key}");
        }
        for forbidden in ["apiKey", "api_key", "key", "token", "secret", "password"] {
            assert!(
                !object.contains_key(forbidden),
                "the template carries {forbidden}"
            );
        }
    }

    #[test]
    fn the_seed_is_the_template_with_the_rows_two_preferences() {
        let document: Value = serde_json::from_str(&seed(&session())).expect("the seed is JSON");
        assert_eq!(document["defaultModel"], "claude-sonnet-4.5");
        assert_eq!(document["defaultThinkingLevel"], "high");
        let template: Value = serde_json::from_str(TEMPLATE).expect("the template is JSON");
        assert_eq!(
            document.as_object().map(|object| object.len()),
            template.as_object().map(|object| object.len()),
            "the seed adds and removes no key: it is the template with two values"
        );
    }

    #[test]
    fn a_seed_is_written_once_and_a_second_launch_keeps_the_agents_own_files() {
        let dir = scratch("seed");
        let home = dir.join(HOME);
        let session = session();
        assert_eq!(
            ensure_home(&home, &session).expect("the seed is writable"),
            Created::Seeded
        );
        let settings = std::fs::read_to_string(home.join(SETTINGS)).expect("the seed is there");
        assert!(settings.contains("claude-sonnet-4.5"), "{settings}");
        // What pi itself writes into the directory must survive a resume: it is the agent's state.
        std::fs::write(home.join("auth.json"), "{}").expect("writable");
        std::fs::write(home.join(SETTINGS), "{\"model\":\"kept\"}").expect("writable");
        assert_eq!(
            ensure_home(&home, &session).expect("a second launch is not a failure"),
            Created::AlreadyThere
        );
        assert_eq!(
            std::fs::read_to_string(home.join(SETTINGS)).expect("the seed is there"),
            "{\"model\":\"kept\"}",
            "a seed that already exists is the session's, not this story's"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_launch_line_is_built_from_an_allow_list() {
        // Nothing is scrubbed because nothing can be a secret: the line names the image, the version, the
        // session, the directory, the model, the thinking level and the trust decision, and no environment.
        let line = launch_line(
            "agent-image:0.1.0",
            "1.2.3",
            &session(),
            Path::new("/var/lib/holosmith/sessions/0123456789abcdef/pi-home"),
        );
        for expected in [
            "agent-image:0.1.0",
            "pi 1.2.3",
            "0123456789abcdef",
            "/var/lib/holosmith/sessions/0123456789abcdef/pi-home",
            "claude-sonnet-4.5",
            "high",
            "project trust approved",
            "pi --mode rpc --no-session --approve",
        ] {
            assert!(line.contains(expected), "{expected} is not in {line}");
        }
        // The environment is a list of names, not of values: none of the four appears with a value.
        for (name, value) in ENVIRONMENT {
            assert!(
                !line.contains(&format!("{name}={value}")),
                "the launch line carries {name}"
            );
        }
        assert_eq!(
            line.matches('=').count(),
            0,
            "the line has an `=` in it, which is how a variable would look: {line}"
        );
    }

    #[test]
    fn the_pinned_version_is_the_one_the_image_must_run() {
        // Both arms, without a second image and without spelling the pin (which lives in `pins/pi.txt`
        // only — `tests/pin_single_source.rs`): the match is `Ok`, and anything else is the sentence.
        let pinned = crate::pins::pi_version();
        assert_eq!(
            version_check("agent-image:0.1.0", pinned),
            Ok(pinned.to_string())
        );
        let stale = format!("{pinned}-stale");
        let error = version_check("agent-image:0.1.0", &stale)
            .expect_err("a different version is a refusal");
        assert!(error.contains(&stale), "{error}");
        assert!(error.contains(pinned), "{error}");
        assert!(error.contains("agent-image:0.1.0"), "{error}");
    }

    #[test]
    fn the_version_sentence_names_the_image_both_versions_and_the_fix() {
        let sentence = version_sentence("agent-image:0.1.0", "1.2.3", "9.9.9");
        for expected in ["agent-image:0.1.0", "pi 1.2.3", "pi 9.9.9", "rebuild"] {
            assert!(
                sentence.contains(expected),
                "{expected} is not in {sentence}"
            );
        }
    }

    #[test]
    fn a_mismatched_or_missing_pi_is_a_sentence_rather_than_a_panic() {
        // The error path of `version_of` is docker's own words; what this pins is that a failed question
        // answers `Err` and that the caller's sentence can quote it.
        let docker = Docker::new(Some("tcp://127.0.0.1:1".to_string()));
        let error = version_of(&docker, "agent-image:0.1.0")
            .expect_err("a daemon that is not there cannot answer");
        assert!(!error.is_empty(), "the failure says nothing");
    }

    /// A child process that waits for one command, writes `records` on stdout and exits `code`.
    ///
    /// It reads one line first, so the handshake's write cannot race the process's exit: a fake that
    /// answered before its stdin was read failed the write with `Broken pipe` and read as a bug in the
    /// plumbing (measured — the first version of these tests did exactly that).
    fn fake(records: &[&str], code: i32) -> (Agent, oneshot::Receiver<Ended>) {
        let mut script = String::from("read line\n");
        for record in records {
            script.push_str(&format!("printf '%s\\n' '{record}'\n"));
        }
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg(format!("{script}exit {code}"));
        Agent::launch(command, "0123456789abcdef").expect("the fake agent starts")
    }

    #[tokio::test]
    async fn the_ready_handshake_is_the_response_that_repeats_the_id() {
        let (agent, _exit) = fake(
            &[
                r#"{"type":"agent_start"}"#,
                r#"{"id":"ready-1","type":"response","command":"get_state","success":true,"data":{"isStreaming":false}}"#,
            ],
            0,
        );
        let reply = agent.ready().await.expect("the handshake answers");
        assert!(reply.success);
        assert_eq!(reply.data.expect("data")["isStreaming"], false);
    }

    #[tokio::test]
    async fn a_refusal_is_the_agents_own_words() {
        let (agent, _exit) = fake(
            &[
                r#"{"id":"ready-1","type":"response","command":"get_state","success":false,"error":"no model"}"#,
            ],
            0,
        );
        let error = agent
            .ready()
            .await
            .expect_err("a refusal is a failed launch");
        assert_eq!(error.to_string(), "pi refused the command: no model");
    }

    #[tokio::test]
    async fn a_process_that_says_nothing_fails_the_handshake_instead_of_hanging() {
        // The bound is what makes "ready from a real signal" safe: a process that starts and never answers
        // must fail its session, and the story's demo is the case where it answers. A process that exits
        // first is the same failure by a shorter route — its stdin is gone before the command is written —
        // so all three arms are "not ready" and none of them is a hang.
        let (agent, _exit) = fake(&[], 0);
        let error = agent
            .request(
                "ready-1",
                json!({"id":"ready-1","type":"get_state"}),
                Duration::from_millis(50),
            )
            .await
            .expect_err("silence is not an answer");
        assert!(
            matches!(
                error,
                AgentError::Closed | AgentError::Timeout(_) | AgentError::Write(_)
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_record_that_is_not_json_is_skipped_and_the_next_one_still_answers() {
        let (agent, _exit) = fake(
            &[
                "not json at all",
                r#"{"type":"queue_update","steering":[]}"#,
                r#"{"id":"ready-1","type":"response","command":"get_state","success":true}"#,
            ],
            0,
        );
        let reply = agent
            .ready()
            .await
            .expect("the good record is still routed");
        assert!(reply.success);
    }

    #[tokio::test]
    async fn the_exit_carries_the_code_and_the_tail_of_stderr() {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("printf 'the reason\\n' >&2; exit 3");
        let (agent, exit) = Agent::launch(command, "0123456789abcdef").expect("it starts");
        assert!(format!("{agent:?}").contains("0123456789abcdef"));
        let exit = exit.await.expect("the exit is reported");
        assert_eq!(exit.code, 3);
        assert_eq!(exit.stderr.trim(), "the reason");
    }

    #[test]
    fn the_command_is_pi_in_rpc_mode_with_trust_approved() {
        assert_eq!(
            COMMAND,
            ["pi", "--mode", "rpc", "--no-session", "--approve"],
            "spec item 2's launch line, and open question 9's answer"
        );
        assert!(
            ENVIRONMENT
                .iter()
                .any(|(name, value)| *name == "NO_PROXY" && *value == "key-proxy"),
            "item 2 §3: provider calls must skip the egress proxy"
        );
    }
}
