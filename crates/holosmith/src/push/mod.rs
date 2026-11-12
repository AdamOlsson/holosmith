//! Web Push: the notification a browser's subscription earns when a turn ends (spec item 11).
//!
//! Five rules hold the design together. Item 11 is the specification and it is quoted, not paraphrased:
//!
//! - "Triggers: a turn finished after running longer than 20 seconds; `session.status` error; a budget or timeout
//!   stop. Not sent while an SSE stream for that session is open."
//! - "Payload carries only `{host, session_id, kind}`, never content. Tapping opens that session."
//! - "The server, not the containers, needs outbound access to the push services."
//!
//! The rules that follow from those three:
//!
//! - **The triggers read the event stream, never the row and never the turn runner.** A [`Pusher`] subscribes to
//!   the writer's broadcast, so "a turn ended" is a frame's arrival rather than a poll of a
//!   column — a poll could miss a turn that started and ended between two ticks, which is exactly the short turn
//!   an operator does not need to be told about anyway, and could *not* see the difference between one long turn
//!   and two short ones. The table is one pure function ([`Watcher::observe`]), so all of its arms are a table
//!   test rather than eight integration cases.
//! - **The long-turn rule has no clock of its own.** A `turn.start` frame's arrival is the turn's start, recorded
//!   in a [`Watcher`]. A turn whose `turn.start` this task did not see — a restart mid-turn — has no duration, so
//!   `turn_finished` cannot fire for it: recorded, not guessed.
//! - **The stream check is read at the moment of sending**, not at the moment of the trigger. A client that
//!   connected during the turn is watching, and the registry's count is what says so
//!   ([`crate::events::StreamRegistry::count`], the real one — the state's own).
//! - **Nothing is sent while holding the database lock**, and every send is `spawn_blocking`: one message is four
//!   children (`ecparam`, `pkeyutl`, `dgst`, `s_client`) and a push service that answers slowly must not be able
//!   to stall a request. The rows are read under the lock, the guard is dropped, and only then is anything built.
//! - **A send that fails is a log line and a kept row.** Only a `404`/`410` from the service removes a
//!   subscription ([`send::Disposition`]); a `401`/`403` is a *key* problem, so a rotated pair must never delete a
//!   good subscription.
//!
//! There is **no config key**, for the sweeper's reason (ADR-0025, ADR-0071's D10): item 11 names none, and the
//! three numbers it does not name — the twenty-second threshold, the message TTL and the token's life — are
//! constants ([`LONG_TURN`], [`send::TTL`], [`jwt::EXP_SECONDS`]), like the sweeper's tick and the cap's interval.

pub mod crypto;
pub mod encrypt;
pub mod jwt;
pub mod repository;
pub mod send;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::clock;
use crate::events::{Event, EventStream};
use crate::http::AppState;
use crate::sessions::Status;
use crate::sessions::state::SESSION_STATUS;
use crate::vapid;
use repository::Stored;
use send::{Disposition, SslClient};

/// How long a turn has to have run before its end is worth a notification (spec item 11's "20 seconds").
///
/// A constant rather than a config key, and the story's own sentence is why: the threshold is not a taste, it is
/// "a turn that took long enough to be worth telling somebody about". A test sets its own with
/// [`Pusher::with_threshold`], which is the same seam `Compactor::with_window` is.
pub const LONG_TURN: Duration = Duration::from_secs(20);

/// The kind word for a turn that ended after [`LONG_TURN`]: item 11's first sentence, and the fourth word.
///
/// The other three are words this build already has — [`ERROR`] is the event kind item 11's second clause names,
/// and [`TURN_TIMEOUT`]/[`BUDGET_EXCEEDED`] are the two stop reasons it names — so a client reads one vocabulary
/// and not two.
pub const TURN_FINISHED: &str = "turn_finished";

/// A machine policy ending a session with an error: item 11's "`session.status` … to `error`", reused as the
/// payload's `kind`.
pub use crate::events::ERROR;

/// A turn that ran past `turn_timeout_min`: the reason word **is** the kind word.
pub use crate::turn::TURN_TIMEOUT;

/// A turn that spent the session's whole budget: the reason word **is** the kind word.
pub use crate::turn::BUDGET_EXCEEDED;

/// The program every push child runs, named once.
///
/// [`crate::preflight::Tools::default`] starts its `openssl` from this and production looks the same name up on
/// the same `PATH`: one spelling of the program, so a test's shim and a machine's tool cannot be two (the rule
/// [`crate::quota::DU`] keeps).
pub const OPENSSL: &str = "openssl";

/// The `sub` claim when the config names no owner.
///
/// `snapshot::IDENTITY`'s precedent: a local identity is a `mailto:` a push service can parse and an operator can
/// recognise, rather than an empty claim a service may refuse.
pub const SUB: &str = "mailto:holosmith@localhost";

/// The trigger table's state: when each session's current turn started, and the threshold that makes an end worth
/// telling somebody about.
///
/// The threshold lives here rather than on [`Pusher`] because the table is here: `observe` is a function of the
/// frame, the arrival instant **and** the threshold, and a signature that took three arguments would push an
/// argument every caller repeats. (`now` is a parameter for ADR-0011's reason — a test's clock is its own.)
#[derive(Debug)]
pub struct Watcher {
    started: HashMap<String, Instant>,
    long_turn: Duration,
}

impl Watcher {
    /// An empty watcher on spec item 11's twenty seconds.
    pub fn new() -> Self {
        Self {
            started: HashMap::new(),
            long_turn: LONG_TURN,
        }
    }

    /// Forget every recorded start, because an ending was missed.
    ///
    /// A subscriber that lagged the broadcast dropped whole frames, so what it still holds may be a start whose
    /// *ending* it never saw: measuring the next settle from it would announce a `turn_finished` for a turn that
    /// never ran that long, which is spec item 11's negative inverted. Forgetting everything turns a lag into
    /// silence — the honest outcome — until the next `turn.start`.
    pub fn forget_all(&mut self) {
        self.started.clear();
    }

    /// The same watcher with a test's threshold, so a five-hundred-millisecond turn can prove the long-turn rule
    /// instead of a twenty-second wait.
    pub fn with_threshold(mut self, long_turn: Duration) -> Self {
        self.long_turn = long_turn;
        self
    }

    /// The kind this frame announces, or `None`.
    ///
    /// The table, exactly as spec item 11 spells it:
    ///
    /// | frame | kind |
    /// | --- | --- |
    /// | `session.status` to `error` | `error` |
    /// | to `idle`/`stopped`, reason `turn_timeout` | `turn_timeout` |
    /// | to `idle`/`stopped`, reason `budget_exceeded` | `budget_exceeded` |
    /// | to `idle`, reason absent, elapsed ≥ the threshold | `turn_finished` |
    ///
    /// A `turn.start` records the arrival instant and announces nothing. Every other transition — including a
    /// `session.status` for a stop reason item 11 does not name (`forced_abort`, `idle_stop`, `evict`, `drain`,
    /// `disk_quota`, `deleted`, `expire`, `pi_exit`) — forgets a recorded start and announces nothing: item 11
    /// names three things, and this table is exhaustive.
    pub fn observe(&mut self, event: &Event, now: Instant) -> Option<&'static str> {
        if event.kind == crate::events::TURN_START {
            self.started.insert(event.session_id.clone(), now);
            return None;
        }
        if event.kind != SESSION_STATUS {
            return None;
        }
        let (to, reason) = status_of(&event.payload)?;
        // Every ending forgets the turn's start: a start left behind would let a later turn's `idle` be measured
        // from this turn's arrival, which is a duration that never existed.
        let started = self.started.remove(&event.session_id);
        if to == Status::Error.as_str() {
            return Some(ERROR);
        }
        if to != Status::Idle.as_str() && to != Status::Stopped.as_str() {
            return None;
        }
        match reason.as_deref() {
            Some(TURN_TIMEOUT) => Some(TURN_TIMEOUT),
            Some(BUDGET_EXCEEDED) => Some(BUDGET_EXCEEDED),
            Some(_) => None,
            None if to == Status::Idle.as_str() => {
                let start = started?;
                (now.saturating_duration_since(start) >= self.long_turn).then_some(TURN_FINISHED)
            }
            None => None,
        }
    }
}

impl Default for Watcher {
    fn default() -> Self {
        Self::new()
    }
}

/// The transition a `session.status` frame reports: where it went, and the reason it recorded.
///
/// Read out of the payload as JSON rather than through `sessions::state::SessionStatus`, because that type is the
/// writer's and derives only `Serialize` — and this reader must not need to deserialize a `Step`, which is not part
/// of what a trigger reads. A payload that is not this shape is `None`: a malformed row is not a trigger.
fn status_of(payload: &str) -> Option<(&'static str, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let to = value.get("to")?.as_str()?;
    // `Status::as_str` is `&'static str`, so the arms below compare the words the column holds rather than a
    // second copy of them — and a sixth state is one line in that one table.
    let to: &'static str = Status::ALL
        .into_iter()
        .find(|status| status.as_str() == to)?
        .as_str();
    let reason = value
        .get("reason")
        .and_then(|reason| reason.as_str())
        .map(str::to_string);
    Some((to, reason))
}

/// How a message reaches one subscription: the seam a test replaces, and the production `openssl s_client`.
///
/// `Send + Sync` because a [`Pusher`] is cloned into nothing but is held across `await` points and its delivers run
/// on `spawn_blocking` threads: a `deliver` that could not be shared would make one message at a time the only
/// option.
pub trait Deliver: Send + Sync {
    /// POST `request` to `endpoint`, answering what the service's status means for the row.
    ///
    /// `Err` is "this message could not be sent at all" — a child that would not start, a TLS handshake that failed
    /// — and every caller keeps the row for it, because a dead *machine* must not delete a live subscription.
    /// `dir` is the per-send scratch directory the caller owns (it is removed when the send ends): the TLS child's
    /// response and stderr go there, so the wait for an answer is a deadline rather than a read that blocks on EOF.
    fn deliver(&self, endpoint: &str, request: &[u8], dir: &Path) -> Result<Disposition, String>;
}

/// The notification task: the event stream it reads, the table it applies, and the seam it sends through.
pub struct Pusher {
    state: AppState,
    events: EventStream,
    watcher: Watcher,
    long_turn: Duration,
    deliver: Arc<dyn Deliver>,
}

impl Pusher {
    /// A pusher wired to this state: the stream, spec item 11's threshold, and the shipped TLS child.
    ///
    /// One subscription, taken here: a subscriber made per notification would miss every frame committed while the
    /// last send ran, which is the retail of the `Lagged` arm below.
    pub fn new(state: &AppState) -> Self {
        Self {
            state: state.clone(),
            events: state.events.subscribe(),
            watcher: Watcher::new(),
            long_turn: LONG_TURN,
            deliver: Arc::new(SslClient {
                // A `PathBuf` in the tools table (a test points it at a shim) and a `String` here, because every
                // child this module starts is named by its arguments rather than by a path it resolves itself.
                openssl: state.tools.openssl.to_string_lossy().into_owned(),
                // The process's own trust configuration, read once at construction: a machine with a private CA
                // sets `SSL_CERT_FILE` and every `openssl` child here sees it (ADR-0071's D2).
                ca_file: std::env::var_os("SSL_CERT_FILE").map(PathBuf::from),
            }),
        }
    }

    /// The same pusher with a test's threshold, the seam `Compactor::with_window` is.
    pub fn with_threshold(mut self, long_turn: Duration) -> Self {
        self.long_turn = long_turn;
        self.watcher = self.watcher.with_threshold(long_turn);
        self
    }

    /// The same pusher sending through `deliver` instead of `openssl`, so a test records what was asked for.
    pub fn with_deliver(mut self, deliver: Arc<dyn Deliver>) -> Self {
        self.deliver = deliver;
        self
    }

    /// Hand the task to the runtime and return its handle.
    ///
    /// The handle is what stops it: `serve` aborts it once the listener's drain is over, so the task never outlives
    /// the process's own serving (the sweeper's and the expiry's rule).
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    /// The threshold this pusher measures a long turn against, for the `debug` line `serve` prints.
    pub fn threshold(&self) -> Duration {
        self.long_turn
    }

    /// Read frames for ever: apply the table, and announce what it names.
    ///
    /// `Lagged` is a `warn` and a missed notification, never a stall — the stream's buffer is bounded and a burst of
    /// events can outrun one delivery, which is the honest cost of not blocking the writer. `Closed` means the
    /// writer has stopped, so the process is going down and this task ends with it.
    pub async fn run(mut self) {
        loop {
            match self.events.recv().await {
                Ok(event) => {
                    let now = Instant::now();
                    if let Some(kind) = self.watcher.observe(&event, now) {
                        let session = event.session_id.clone();
                        self.announce(&session, kind).await;
                    }
                }
                Err(RecvError::Lagged(missed)) => {
                    tracing::warn!(
                        target: "holosmith::push",
                        missed,
                        "the notification task fell behind the event stream, so some notifications were not sent"
                    );
                    // A missed ending makes every recorded start a lie about *some* turn, so none survives: see
                    // `Watcher::forget_all`.
                    self.watcher.forget_all();
                }
                Err(RecvError::Closed) => return,
            }
        }
    }

    /// One notification: the stream check, the rows, the machine's pair, and one send per subscription.
    ///
    /// Public so `tests/push.rs` can drive one frame through the real path without a broadcast, which is the only
    /// way to assert "the stream is open" and "the pair is missing" as rules rather than as races.
    ///
    /// The five steps are the module's rules, in order:
    ///
    /// 1. **A session with a stream open is not notified** — read *now*, not at the trigger.
    /// 2. The rows are read under one lock scope, and the guard is dropped before anything else happens.
    /// 3. The machine's pair is read; absent or unreadable is **one** `warn` and no sends, because a machine that
    ///    never deployed has no pair and that must not be an error per notification.
    /// 4. Each subscription is built and delivered on `spawn_blocking`, with the lock not held.
    /// 5. A `Gone` row is removed; anything else is kept, with a `warn` on `Err`.
    pub async fn announce(&self, session_id: &str, kind: &str) {
        if self.state.streams.count(session_id) > 0 {
            tracing::debug!(
                target: "holosmith::push",
                session = session_id,
                kind,
                "a stream is open for this session, so nothing was pushed"
            );
            return;
        }
        let rows = {
            let database = self
                .state
                .database
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            database.push().all()
        };
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(
                    target: "holosmith::push",
                    %error,
                    "the subscriptions could not be read, so nothing was pushed"
                );
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        let pair = match self.pair() {
            Some(pair) => pair,
            None => return,
        };
        let sub = match self.state.config.owner.as_deref() {
            Some(owner) => format!("mailto:{owner}"),
            None => SUB.to_string(),
        };
        for row in rows {
            let message = Message {
                pair: pair.clone(),
                sub: sub.clone(),
                host: self.state.config.machine_name.clone(),
                session_id: session_id.to_string(),
                kind: kind.to_string(),
                openssl: self.openssl(),
                data_root: self.state.config.data_root.clone(),
                deliver: Arc::clone(&self.deliver),
            };
            let endpoint = row.endpoint.clone();
            let sent = tokio::task::spawn_blocking(move || message.send(&row)).await;
            match sent {
                Ok(Ok((Disposition::Gone, host))) => {
                    tracing::info!(
                        target: "holosmith::push",
                        session = session_id,
                        kind,
                        host = %host,
                        disposition = "gone",
                        "the push service says this endpoint is dead, so its row was removed"
                    );
                    self.forget(&endpoint);
                }
                Ok(Ok((disposition, host))) => tracing::info!(
                    target: "holosmith::push",
                    session = session_id,
                    kind,
                    host = %host,
                    disposition = ?disposition,
                    "a push was sent"
                ),
                Ok(Err(sentence)) => tracing::warn!(
                    target: "holosmith::push",
                    session = session_id,
                    kind,
                    %sentence,
                    "a push could not be sent, so its subscription was kept"
                ),
                Err(error) => tracing::warn!(
                    target: "holosmith::push",
                    session = session_id,
                    kind,
                    %error,
                    "a push task did not finish, so its subscription was kept"
                ),
            }
        }
    }

    /// The `openssl` this pusher's children run: the tools table's program, as the name a child is started with.
    fn openssl(&self) -> String {
        self.state.tools.openssl.to_string_lossy().into_owned()
    }

    /// The machine's pair, or `None` with one `warn`.
    fn pair(&self) -> Option<vapid::Pair> {
        let path = vapid::store(&self.state.config.data_root);
        let readable = match vapid::is_pair(&path) {
            Ok(readable) => readable,
            Err(sentence) => {
                tracing::warn!(
                    target: "holosmith::push",
                    %sentence,
                    "no notification can be signed, so nothing was pushed"
                );
                return None;
            }
        };
        let pair = readable
            .then(|| std::fs::read_to_string(&path).ok())
            .flatten()
            .and_then(|text| vapid::parse(&text));
        if pair.is_none() {
            tracing::warn!(
                target: "holosmith::push",
                path = %path.display(),
                "this machine has no Web Push key pair, so nothing was pushed (`server deploy` writes it)"
            );
        }
        pair
    }

    /// Remove one dead subscription's row, under the lock for one statement.
    fn forget(&self, endpoint: &str) {
        let database = self
            .state
            .database
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(error) = database.push().remove(endpoint) {
            tracing::warn!(
                target: "holosmith::push",
                %error,
                "a dead subscription could not be removed, so it will be tried again"
            );
        }
    }
}

/// The payload D10 pins: `{host, session_id, kind}` and nothing else.
///
/// A struct rather than a `serde_json::json!` map, so "the body carries three keys" is a fact about this type: a
/// fourth field is a compile-time change here, and `tests/push.rs`/`encrypt.rs` assert the key set of what actually
/// arrives.
#[derive(serde::Serialize)]
struct Payload<'a> {
    host: &'a str,
    session_id: &'a str,
    kind: &'a str,
}

/// One message being built and sent, on a blocking thread.
///
/// Everything it needs is owned, because it crosses into `spawn_blocking`: a borrow would tie the send to the
/// caller's stack frame and make "off the writer's path" untrue.
struct Message {
    pair: vapid::Pair,
    sub: String,
    /// What this machine calls itself: the payload's `host`, and therefore the segment a tap's path is
    /// built from. Read from the config at the moment a notification is built, so a reload is the only thing a
    /// changed name needs.
    host: String,
    session_id: String,
    kind: String,
    openssl: String,
    data_root: PathBuf,
    deliver: Arc<dyn Deliver>,
}

impl Message {
    /// The plaintext this message carries: `{host, session_id, kind}` and nothing else.
    ///
    /// A method rather than four lines inside [`Message::send`], so the one fact a sender cannot observe from
    /// outside — a sealed body reads the same whatever it holds — has a seam a unit test can read
    /// (`a_message_carries_the_machines_own_name`). The value's **source** is `Pusher::announce`, the config's
    /// `machine_name`, and the wire is the gate's `case_cross_machine`, which decrypts what arrives.
    fn plaintext(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&Payload {
            host: &self.host,
            session_id: &self.session_id,
            kind: &self.kind,
        })
        .map_err(|error| format!("the notification's payload could not be serialised ({error})"))
    }

    /// Build the request for one subscription and deliver it, answering the disposition and the **host** — never
    /// the endpoint, which is a capability URL.
    fn send(&self, row: &Stored) -> Result<(Disposition, String), String> {
        let audience = jwt::origin(&row.endpoint).ok_or_else(|| {
            "a stored subscription's endpoint is not an https URL with a host, so it cannot be the JWT's audience"
                .to_string()
        })?;
        let subscription = encrypt::Subscription {
            endpoint: row.endpoint.clone(),
            p256dh: vapid::unbase64url(&row.p256dh)
                .ok_or_else(|| "a stored subscription's p256dh is not base64url".to_string())?,
            auth: vapid::unbase64url(&row.auth)
                .ok_or_else(|| "a stored subscription's auth is not base64url".to_string())?,
        };
        let plaintext = self.plaintext()?;
        let tmp = jwt::Temp::new(&self.data_root)?;
        let as_pair = encrypt::ephemeral(&self.openssl)?;
        let salt = encrypt::salt()?;
        let body = encrypt::seal(
            &subscription,
            &plaintext,
            &as_pair,
            &salt,
            &self.openssl,
            &tmp,
        )?;
        let token = jwt::token(
            &self.pair,
            &audience,
            &self.sub,
            clock::unix_now(),
            &self.openssl,
            &tmp,
        )?;
        let request = send::request(&row.endpoint, &token, &self.pair.public_key, &body)?;
        let disposition = self.deliver.deliver(&row.endpoint, &request, tmp.path())?;
        // The audience is `https://host[:port]`, so it *is* the host: logging it says which service answered
        // without naming the capability.
        Ok((disposition, audience))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{NewEvent, TURN_START};
    use crate::sessions::state::SessionStatus;

    /// A frame, as the writer would store it: `Event::of` is `pub(crate)` and this module is in the crate.
    fn event(kind: &str, payload: &str) -> Event {
        Event::of(
            &NewEvent {
                session_id: "0123456789abcdef".to_string(),
                kind: kind.to_string(),
                payload: payload.to_string(),
                created_at: 1_760_000_000,
            },
            1,
        )
    }

    /// A `session.status` frame in the transitioner's own payload shape.
    fn status(from: Status, to: Status, reason: Option<&str>) -> Event {
        event(
            SESSION_STATUS,
            &SessionStatus::transitioned(from, to, reason.map(str::to_string)).to_json(),
        )
    }

    /// What a sender cannot observe from outside: a sealed body reads the same whatever it holds, so the plaintext's
    /// three keys and the name they carry are pinned here rather than by a recorder.
    ///
    /// The value's **source** is `Pusher::announce` — `state.config.machine_name` — and its **arrival** is stitched
    /// by the gate's `case_cross_machine`, which decrypts a real push from a real peer. This test is the middle: the
    /// message's own bytes, with a name that is not the default, so a constant would fail it.
    #[test]
    fn a_message_carries_the_machines_own_name_and_nothing_else() {
        let message = Message {
            pair: vapid::Pair {
                public_key: "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8".to_string(),
                private_key: "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw".to_string(),
            },
            sub: SUB.to_string(),
            host: "workshop".to_string(),
            session_id: "0123456789abcdef".to_string(),
            kind: TURN_FINISHED.to_string(),
            openssl: OPENSSL.to_string(),
            data_root: std::path::PathBuf::from("/nonexistent"),
            deliver: Arc::new(SslClient {
                openssl: OPENSSL.to_string(),
                ca_file: None,
            }),
        };
        let plaintext = message.plaintext().expect("the payload serialises");
        assert_eq!(
            String::from_utf8(plaintext).expect("the payload is UTF-8"),
            r#"{"host":"workshop","session_id":"0123456789abcdef","kind":"turn_finished"}"#
        );
    }

    #[test]
    fn the_table_is_the_four_firing_shapes_and_the_negatives() {
        let base = Instant::now();
        let at = |millis: u64| base + Duration::from_millis(millis);

        // `session.status` to `error` — item 11's second clause, and it fires whatever the duration.
        let mut watcher = Watcher::new();
        assert_eq!(
            watcher.observe(&status(Status::Running, Status::Error, None), at(0)),
            Some(ERROR)
        );
        // `turn_timeout`, from either resting state.
        for to in [Status::Idle, Status::Stopped] {
            let mut watcher = Watcher::new();
            assert_eq!(
                watcher.observe(&status(Status::Running, to, Some(TURN_TIMEOUT)), at(0)),
                Some(TURN_TIMEOUT),
                "{to}"
            );
            assert_eq!(
                watcher.observe(&status(Status::Running, to, Some(BUDGET_EXCEEDED)), at(0)),
                Some(BUDGET_EXCEEDED),
                "{to}"
            );
        }
        // The long turn: a start, then an `idle` with no reason at or past the threshold.
        let mut watcher = Watcher::new().with_threshold(Duration::from_millis(50));
        assert_eq!(
            watcher.observe(&event(TURN_START, "{}"), at(0)),
            None,
            "a start announces nothing"
        );
        assert_eq!(
            watcher.observe(&status(Status::Running, Status::Idle, None), at(50)),
            Some(TURN_FINISHED)
        );

        // The negatives. A settle under the threshold sends nothing.
        let mut watcher = Watcher::new().with_threshold(Duration::from_millis(50));
        watcher.observe(&event(TURN_START, "{}"), at(0));
        assert_eq!(
            watcher.observe(&status(Status::Running, Status::Idle, None), at(49)),
            None
        );
        // A settle with no `turn.start` has no duration, so it cannot fire — however long the process has been up.
        let mut watcher = Watcher::new().with_threshold(Duration::from_millis(50));
        assert_eq!(
            watcher.observe(&status(Status::Running, Status::Idle, None), at(60_000)),
            None
        );
        // A reason item 11 does not name is not a trigger — the table is exhaustive, and these are the words the
        // build's other policies record.
        for reason in [
            "forced_abort",
            "disk_quota",
            "idle_stop",
            "evict",
            "drain",
            "deleted",
            "expire",
            "pi_exit",
            "abort_completed",
        ] {
            let mut watcher = Watcher::new().with_threshold(Duration::from_millis(50));
            watcher.observe(&event(TURN_START, "{}"), at(0));
            assert_eq!(
                watcher.observe(
                    &status(Status::Running, Status::Idle, Some(reason)),
                    at(60_000)
                ),
                None,
                "{reason}"
            );
        }
        // A `stopped` with no reason is not the long-turn arm: item 11 says `idle`.
        let mut watcher = Watcher::new().with_threshold(Duration::from_millis(50));
        watcher.observe(&event(TURN_START, "{}"), at(0));
        assert_eq!(
            watcher.observe(&status(Status::Running, Status::Stopped, None), at(60_000)),
            None
        );
        // A running transition and a provisioning step are not endings.
        let mut watcher = Watcher::new().with_threshold(Duration::from_millis(50));
        assert_eq!(
            watcher.observe(&status(Status::Idle, Status::Running, None), at(0)),
            None
        );
        assert_eq!(
            watcher.observe(&status(Status::Idle, Status::Idle, None), at(60_000)),
            None,
            "a step frame has no turn behind it"
        );
        // A frame that is not `session.status` at all.
        assert_eq!(
            watcher.observe(&event(crate::events::MESSAGE_END, "{}"), at(0)),
            None
        );
        // A payload that is not the pinned shape is not a trigger rather than a panic.
        assert_eq!(
            watcher.observe(&event(SESSION_STATUS, "not json"), at(0)),
            None
        );
        assert_eq!(
            watcher.observe(&event(SESSION_STATUS, r#"{"to":"nowhere"}"#), at(0)),
            None
        );
    }

    #[test]
    fn an_ending_forgets_the_turn_its_start_recorded() {
        // A turn that ended with `error` and a later `idle` with no reason: the second frame must not be measured
        // from the first turn's arrival, because that duration never happened.
        let base = Instant::now();
        let at = |millis: u64| base + Duration::from_millis(millis);
        let mut watcher = Watcher::new().with_threshold(Duration::from_millis(50));
        watcher.observe(&event(TURN_START, "{}"), at(0));
        assert_eq!(
            watcher.observe(&status(Status::Running, Status::Error, None), at(10)),
            Some(ERROR)
        );
        assert_eq!(
            watcher.observe(&status(Status::Error, Status::Idle, None), at(60_000)),
            None
        );
        // And a second turn's own start is measured from itself.
        watcher.observe(&event(TURN_START, "{}"), at(60_000));
        assert_eq!(
            watcher.observe(&status(Status::Running, Status::Idle, None), at(60_060)),
            Some(TURN_FINISHED)
        );
    }

    #[test]
    fn the_kind_words_are_the_vocabulary_a_client_reads() {
        assert_eq!(TURN_FINISHED, "turn_finished");
        assert_eq!(ERROR, "error");
        assert_eq!(TURN_TIMEOUT, "turn_timeout");
        assert_eq!(BUDGET_EXCEEDED, "budget_exceeded");
        // The two reason words are the same strings `turn` records on the row, which is what makes the payload's
        // `kind` and the session's `status_reason` one vocabulary (ADR-0008).
        assert_eq!(TURN_TIMEOUT, crate::turn::TURN_TIMEOUT);
        assert_eq!(BUDGET_EXCEEDED, crate::turn::BUDGET_EXCEEDED);
        assert_eq!(ERROR, crate::events::ERROR);
        assert_eq!(
            LONG_TURN,
            Duration::from_secs(20),
            "item 11's twenty seconds"
        );
        assert!(SUB.starts_with("mailto:"));
    }
}
